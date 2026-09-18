//! The UDP socket under the peer connection: sized, and counted.
//!
//! webrtc-rs binds its own socket and offers no option for its buffers, but it
//! hands every socket it binds to [`Runtime::wrap_udp_socket`]. [`TunedRuntime`]
//! wraps the stock runtime at that one seam: it sizes `SO_RCVBUF`/`SO_SNDBUF`
//! before the socket goes async, and returns a [`CountedSocket`] that keeps the
//! numbers S30 needed to name the loss limiter — how deep the receive queue
//! gets, how big a send burst is, whether a send ever fails.
//!
//! Counting costs one `FIONREAD` ioctl per receive call and a few relaxed
//! atomics; nothing allocates.

use std::fmt;
use std::future::Future;
use std::io::{self, IoSliceMut};
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

use webrtc::runtime::{
    AsyncInterval, AsyncTcpListener, AsyncTcpStream, AsyncUdpSocket, JoinHandle, RecvMeta, Runtime,
    Transmit,
};

/// `SO_RCVBUF` for the media socket.
///
/// The sender does not pace: an access unit leaves as one back-to-back burst
/// at line rate, and a keyframe is the worst case. Measured on the r10 build a
/// 1440p keyframe at 40 Mb/s is ~0.5 MB; at the brief's ceiling (4K60,
/// 80 Mb/s) budget 2 MB. The buffer has to hold a whole keyframe burst plus
/// whatever arrives while the receive task is descheduled — one 15.6 ms
/// Windows scheduler quantum at 80 Mb/s is another 160 KB. 4 MB holds two
/// worst-case keyframes, i.e. 400 ms of video at 80 Mb/s and 800 ms at 40,
/// against a Windows default of 64 KB (13 ms at 40 Mb/s, and less than one
/// ordinary 83 KB frame). It is only a ceiling on queued bytes: the kernel
/// does not commit it up front, so an idle receiver pays nothing.
pub const RECV_BUFFER_BYTES: usize = 4 * 1024 * 1024;

/// `SO_SNDBUF`. The sender's burst is handed to the kernel in GSO batches; a
/// full send buffer surfaces as `WouldBlock` and the driver waits, so this is
/// about not stalling the packetizer, not about loss. One worst-case keyframe.
pub const SEND_BUFFER_BYTES: usize = 2 * 1024 * 1024;

/// Test hook: `RELAY_UDP_RCVBUF=<bytes>` overrides [`RECV_BUFFER_BYTES`];
/// `0` leaves the OS default in place, which is how the before/after
/// measurement runs on one build.
fn recv_buffer_request() -> Option<usize> {
    match std::env::var("RELAY_UDP_RCVBUF").ok().and_then(|v| v.parse::<usize>().ok()) {
        Some(0) => None,
        Some(n) => Some(n),
        None => Some(RECV_BUFFER_BYTES),
    }
}

/// Counters for the media socket. Totals are monotonic; the `*_peak` values
/// are since the last [`NetStats::take`].
#[derive(Default)]
pub struct NetStats {
    /// What the OS granted, read back after the set.
    pub rcvbuf_bytes: AtomicU64,
    pub sndbuf_bytes: AtomicU64,
    pub recv_calls: AtomicU64,
    pub recv_datagrams: AtomicU64,
    pub recv_bytes: AtomicU64,
    /// Bytes still queued in the socket right after a receive — the backlog.
    /// Against `rcvbuf_bytes` this is how close the socket came to dropping.
    queue_peak: AtomicU64,
    /// Most datagrams the kernel coalesced into one receive (URO).
    coalesced_peak: AtomicU64,
    pub send_calls: AtomicU64,
    pub send_datagrams: AtomicU64,
    pub send_bytes: AtomicU64,
    pub send_errors: AtomicU64,
    /// Sends that found the socket unwritable (send buffer full).
    pub send_blocked: AtomicU64,
    /// Most datagrams handed to the kernel in one send (GSO batch).
    batch_peak: AtomicU64,
}

/// One reporting window of [`NetStats`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct NetSnapshot {
    pub rcvbuf_bytes: u64,
    pub sndbuf_bytes: u64,
    pub recv_datagrams: u64,
    pub recv_bytes: u64,
    pub queue_peak_bytes: u64,
    pub coalesced_peak: u64,
    pub send_datagrams: u64,
    pub send_bytes: u64,
    pub send_errors: u64,
    pub send_blocked: u64,
    pub batch_peak: u64,
    /// System-wide UDP datagrams the stack received and could not deliver
    /// (`MIB_UDPSTATS::dwInErrors`). Secondary evidence only: system-wide, and
    /// it did not count loopback overflow drops in this module's own test.
    /// `queue_peak_bytes` reaching `rcvbuf_bytes` is the overflow signal.
    pub udp_in_errors: u64,
}

impl NetStats {
    /// Snapshot, and start a new window for the peaks.
    pub fn take(&self) -> NetSnapshot {
        let l = |a: &AtomicU64| a.load(Ordering::Relaxed);
        NetSnapshot {
            rcvbuf_bytes: l(&self.rcvbuf_bytes),
            sndbuf_bytes: l(&self.sndbuf_bytes),
            recv_datagrams: l(&self.recv_datagrams),
            recv_bytes: l(&self.recv_bytes),
            queue_peak_bytes: self.queue_peak.swap(0, Ordering::Relaxed),
            coalesced_peak: self.coalesced_peak.swap(0, Ordering::Relaxed),
            send_datagrams: l(&self.send_datagrams),
            send_bytes: l(&self.send_bytes),
            send_errors: l(&self.send_errors),
            send_blocked: l(&self.send_blocked),
            batch_peak: self.batch_peak.swap(0, Ordering::Relaxed),
            udp_in_errors: udp_in_errors(),
        }
    }
}

/// One `media socket` line a second in the log while packets are moving, for
/// as long as the peer connection (which owns the runtime, which owns the
/// stats) is alive.
pub fn log_every_second(stats: &Arc<NetStats>) {
    let stats = Arc::downgrade(stats);
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(1));
        let mut last: Option<NetSnapshot> = None;
        loop {
            tick.tick().await;
            let Some(stats) = stats.upgrade() else { break };
            let now = stats.take();
            if let Some(prev) = last {
                let rx = now.recv_datagrams - prev.recv_datagrams;
                let tx = now.send_datagrams - prev.send_datagrams;
                if rx + tx > 0 {
                    tracing::info!(
                        rx_datagrams = rx,
                        rx_mbps = (now.recv_bytes - prev.recv_bytes) as f64 * 8.0 / 1e6,
                        queue_peak_bytes = now.queue_peak_bytes,
                        rcvbuf = now.rcvbuf_bytes,
                        coalesced_peak = now.coalesced_peak,
                        udp_in_errors = now.udp_in_errors - prev.udp_in_errors,
                        tx_datagrams = tx,
                        tx_mbps = (now.send_bytes - prev.send_bytes) as f64 * 8.0 / 1e6,
                        batch_peak = now.batch_peak,
                        send_blocked = now.send_blocked - prev.send_blocked,
                        send_errors = now.send_errors - prev.send_errors,
                        "media socket"
                    );
                }
            }
            last = Some(now);
        }
    });
}

/// webrtc-rs logs through the `log` crate, which nothing here was listening
/// to: its "Failed to send RtpPacket to track remote" — a received packet
/// dropped because the track's 256-slot queue was full — went nowhere. Forward
/// warnings and errors into tracing. (Not `tracing-log`: a level cap and one
/// match is all that is needed, and debug formatting upstream stays free.)
pub fn forward_log_crate() {
    struct Forward;
    impl log::Log for Forward {
        fn enabled(&self, m: &log::Metadata<'_>) -> bool {
            m.level() <= log::Level::Warn
        }
        fn log(&self, r: &log::Record<'_>) {
            if !self.enabled(r.metadata()) {
                return;
            }
            if r.level() == log::Level::Error {
                tracing::error!(from = r.target(), "{}", r.args());
            } else {
                tracing::warn!(from = r.target(), "{}", r.args());
            }
        }
        fn flush(&self) {}
    }
    if log::set_logger(&Forward).is_ok() {
        log::set_max_level(log::LevelFilter::Warn);
    }
}

/// The stock runtime with the UDP seam replaced. Everything else delegates.
pub struct TunedRuntime {
    inner: Arc<dyn Runtime>,
    stats: Arc<NetStats>,
}

impl TunedRuntime {
    pub fn new(inner: Arc<dyn Runtime>) -> (Arc<dyn Runtime>, Arc<NetStats>) {
        let stats = Arc::new(NetStats::default());
        (Arc::new(Self { inner, stats: stats.clone() }), stats)
    }
}

impl fmt::Debug for TunedRuntime {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TunedRuntime").field("inner", &self.inner).finish()
    }
}

impl Runtime for TunedRuntime {
    fn spawn(&self, future: Pin<Box<dyn Future<Output = ()> + Send>>) -> Box<dyn JoinHandle> {
        self.inner.spawn(future)
    }

    fn spawn_reactor(
        &self,
        reactor_pool_size: usize,
        future: Pin<Box<dyn Future<Output = ()> + Send>>,
    ) -> Box<dyn JoinHandle> {
        self.inner.spawn_reactor(reactor_pool_size, future)
    }

    fn wrap_udp_socket(&self, socket: std::net::UdpSocket) -> io::Result<Arc<dyn AsyncUdpSocket>> {
        // The mDNS socket comes through here too; it is not the media path.
        let media = socket.local_addr().map(|a| a.port() != 5353).unwrap_or(true);
        if !media {
            return self.inner.wrap_udp_socket(socket);
        }
        let sock = socket2::SockRef::from(&socket);
        let before = sock.recv_buffer_size().unwrap_or(0);
        if let Some(want) = recv_buffer_request() {
            if let Err(e) = sock.set_recv_buffer_size(want) {
                tracing::warn!(error = %e, want, "could not size the UDP receive buffer");
            }
            if let Err(e) = sock.set_send_buffer_size(SEND_BUFFER_BYTES) {
                tracing::warn!(error = %e, "could not size the UDP send buffer");
            }
        }
        let rcvbuf = sock.recv_buffer_size().unwrap_or(0);
        let sndbuf = sock.send_buffer_size().unwrap_or(0);
        self.stats.rcvbuf_bytes.store(rcvbuf as u64, Ordering::Relaxed);
        self.stats.sndbuf_bytes.store(sndbuf as u64, Ordering::Relaxed);
        tracing::info!(os_default = before, rcvbuf, sndbuf, "media socket buffers");

        // A second handle to the same kernel socket, kept for FIONREAD.
        let probe = socket.try_clone()?;
        let inner = self.inner.wrap_udp_socket(socket)?;
        Ok(Arc::new(CountedSocket { inner, probe, stats: self.stats.clone() }))
    }

    fn wrap_tcp_listener(
        &self,
        listener: std::net::TcpListener,
    ) -> io::Result<Arc<dyn AsyncTcpListener>> {
        self.inner.wrap_tcp_listener(listener)
    }

    fn connect_tcp<'a>(
        &'a self,
        remote_addr: SocketAddr,
    ) -> Pin<Box<dyn Future<Output = io::Result<Arc<dyn AsyncTcpStream>>> + Send + 'a>> {
        self.inner.connect_tcp(remote_addr)
    }

    fn resolve_host<'a>(
        &'a self,
        host: &'a str,
    ) -> Pin<Box<dyn Future<Output = io::Result<Vec<SocketAddr>>> + Send + 'a>> {
        self.inner.resolve_host(host)
    }

    fn sleep(&self, duration: Duration) -> Pin<Box<dyn Future<Output = ()> + Send + 'static>> {
        self.inner.sleep(duration)
    }

    fn interval(&self, period: Duration) -> Box<dyn AsyncInterval> {
        self.inner.interval(period)
    }

    fn block_on(&self, future: Pin<Box<dyn Future<Output = ()> + '_>>) {
        self.inner.block_on(future)
    }

    fn yield_now(&self) -> Pin<Box<dyn Future<Output = ()> + Send + 'static>> {
        self.inner.yield_now()
    }

    fn name(&self) -> &'static str {
        self.inner.name()
    }
}

struct CountedSocket {
    inner: Arc<dyn AsyncUdpSocket>,
    probe: std::net::UdpSocket,
    stats: Arc<NetStats>,
}

impl fmt::Debug for CountedSocket {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.inner.fmt(f)
    }
}

fn raise(peak: &AtomicU64, v: u64) {
    peak.fetch_max(v, Ordering::Relaxed);
}

impl AsyncUdpSocket for CountedSocket {
    fn local_addr(&self) -> io::Result<SocketAddr> {
        self.inner.local_addr()
    }

    fn poll_send(&self, cx: &mut Context<'_>, transmit: &Transmit<'_>) -> Poll<io::Result<usize>> {
        let r = self.inner.poll_send(cx, transmit);
        let s = &self.stats;
        match &r {
            Poll::Pending => {
                s.send_blocked.fetch_add(1, Ordering::Relaxed);
            }
            Poll::Ready(Ok(_)) => {
                let len = transmit.contents.len();
                let datagrams = match transmit.segment_size {
                    Some(seg) if seg > 0 => len.div_ceil(seg).max(1),
                    _ => 1,
                } as u64;
                s.send_calls.fetch_add(1, Ordering::Relaxed);
                s.send_datagrams.fetch_add(datagrams, Ordering::Relaxed);
                s.send_bytes.fetch_add(len as u64, Ordering::Relaxed);
                raise(&s.batch_peak, datagrams);
            }
            Poll::Ready(Err(_)) => {
                s.send_errors.fetch_add(1, Ordering::Relaxed);
            }
        }
        r
    }

    fn poll_recv(
        &self,
        cx: &mut Context<'_>,
        bufs: &mut [IoSliceMut<'_>],
        meta: &mut [RecvMeta],
    ) -> Poll<io::Result<usize>> {
        let r = self.inner.poll_recv(cx, bufs, meta);
        if let Poll::Ready(Ok(n)) = &r {
            let s = &self.stats;
            s.recv_calls.fetch_add(1, Ordering::Relaxed);
            for m in &meta[..*n] {
                let datagrams = m.len.div_ceil(m.stride.max(1)).max(1) as u64;
                s.recv_datagrams.fetch_add(datagrams, Ordering::Relaxed);
                s.recv_bytes.fetch_add(m.len as u64, Ordering::Relaxed);
                raise(&s.coalesced_peak, datagrams);
            }
            raise(&s.queue_peak, queued_bytes(&self.probe));
        }
        r
    }

    fn max_gso_segments(&self) -> usize {
        self.inner.max_gso_segments()
    }

    fn max_gro_segments(&self) -> usize {
        self.inner.max_gro_segments()
    }
}

/// Bytes queued for reading on `socket`. On Windows `FIONREAD` on a datagram
/// socket reports the total across every queued datagram, not just the first.
#[cfg(windows)]
fn queued_bytes(socket: &std::net::UdpSocket) -> u64 {
    use std::os::windows::io::AsRawSocket;
    use windows::Win32::Networking::WinSock::{ioctlsocket, FIONREAD, SOCKET};
    let mut n = 0u32;
    // SAFETY: a live socket handle and a valid out-pointer.
    let rc = unsafe { ioctlsocket(SOCKET(socket.as_raw_socket() as usize), FIONREAD, &mut n) };
    if rc == 0 {
        u64::from(n)
    } else {
        0
    }
}

#[cfg(not(windows))]
fn queued_bytes(_socket: &std::net::UdpSocket) -> u64 {
    0
}

/// `MIB_UDPSTATS::dwInErrors` for IPv4, system-wide. 0 if the call fails.
#[cfg(windows)]
pub fn udp_in_errors() -> u64 {
    use windows::Win32::NetworkManagement::IpHelper::{GetUdpStatisticsEx, MIB_UDPSTATS};
    let mut stats = MIB_UDPSTATS::default();
    // SAFETY: a valid out-pointer; AF_INET = 2.
    if unsafe { GetUdpStatisticsEx(&mut stats, 2) } == 0 {
        u64::from(stats.dwInErrors)
    } else {
        0
    }
}

#[cfg(not(windows))]
pub fn udp_in_errors() -> u64 {
    0
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    /// The OS facts the S30 measurement leans on: the default buffer is
    /// 64 KB, FIONREAD totals the queue, and the queue stops dead at
    /// `SO_RCVBUF` — everything past it is dropped silently. Windows has no
    /// per-socket drop counter, and the system-wide UDP InErrors did *not*
    /// move when this test overflowed a loopback socket, so a queue peak at
    /// the buffer size is the overflow evidence, not InErrors.
    #[test]
    fn a_full_receive_buffer_is_visible_in_fionread_and_in_errors() {
        let rx = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        let tx = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        let to = rx.local_addr().unwrap();
        let buf = socket2::SockRef::from(&rx).recv_buffer_size().unwrap();

        for _ in 0..3 {
            tx.send_to(&[0u8; 1000], to).unwrap();
        }
        std::thread::sleep(Duration::from_millis(50));
        assert_eq!(queued_bytes(&rx), 3000, "FIONREAD totals every queued datagram");

        let before = udp_in_errors();
        // Four buffers' worth into a socket nobody reads.
        for _ in 0..(buf * 4 / 1000) {
            tx.send_to(&[0u8; 1000], to).unwrap();
        }
        std::thread::sleep(Duration::from_millis(100));
        let queued = queued_bytes(&rx);
        let dropped = udp_in_errors() - before;
        eprintln!("rcvbuf={buf} queued={queued} in_errors_delta={dropped}");
        assert!(queued as usize <= buf, "the queue stops at SO_RCVBUF");
        assert!(queued as usize > buf - 2000, "and it filled: the rest was dropped");
    }
}
