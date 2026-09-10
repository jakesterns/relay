//! IPC between the core and the Tauri shell (or the CLI).
//!
//! Transport: a Windows named pipe, newline-delimited JSON, one message per
//! line. Requests carry an `id` that is echoed on the response. After a
//! `subscribe` request the server also pushes `event` lines on that connection.
//!
//! Hardening: the pipe carries a DACL that admits only the SID of the current
//! user, remote clients are rejected, any line over [`IPC_MAX_LINE`] closes the
//! connection, and a connection that has not subscribed is dropped after
//! [`IDLE_TIMEOUT`] without a request. Handler execution is bounded by
//! [`REQUEST_TIMEOUT`].
//!
//! The TypeScript mirror of these shapes is `ui/src/lib/ipc.ts`.

use std::time::Duration;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub use crate::config::IPC_MAX_LINE;
use crate::share::ShareRequest;
use crate::types::{CoreState, ProcessInfo, Profile, ProfileSummary};

/// A connection that has neither subscribed nor sent a request for this long is closed.
pub const IDLE_TIMEOUT: Duration = Duration::from_secs(30);
/// Upper bound on the handling time of one request.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

const NEWLINE: u8 = b'\n';

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "method", content = "params", rename_all = "snake_case")]
pub enum Method {
    Ping,
    Status,
    ListProfiles,
    GetProfile {
        id: Uuid,
    },
    SaveProfile {
        profile: Box<Profile>,
    },
    DeleteProfile {
        id: Uuid,
    },
    /// Manually apply (ignores focus until blur/restore).
    ApplyProfile {
        id: Uuid,
    },
    RestoreAll,
    /// Processes that currently own a visible top-level window.
    ListProcesses,
    GetAutostart,
    SetAutostart {
        enabled: bool,
    },
    /// Spawn the share engine (a child process) to share this PC to a peer.
    StartShare {
        request: Box<ShareRequest>,
    },
    /// Stop the running share engine.
    StopShare,
    /// Browse the LAN for Relay receivers (blocks briefly).
    DiscoverReceivers,
    Subscribe,
    Shutdown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Request {
    pub id: u64,
    #[serde(flatten)]
    pub method: Method,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Reply {
    Pong,
    Status { state: Box<CoreState> },
    Profiles { profiles: Vec<ProfileSummary> },
    Profile { profile: Box<Profile> },
    Processes { processes: Vec<ProcessInfo> },
    Autostart { enabled: bool },
    Receivers { receivers: serde_json::Value },
    Ok,
    Error { message: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum Event {
    StateChanged {
        state: Box<CoreState>,
    },
    Notice {
        text: String,
    },
    /// Instrument-strip stats from the share engine (verbatim JSON).
    ShareStats {
        data: serde_json::Value,
    },
    /// The share engine started, connected, or stopped.
    ShareStatus {
        sharing: bool,
        peer: Option<String>,
        message: Option<String>,
    },
}

/// One line on the wire is exactly one of these.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Outbound {
    Response { id: u64, result: Reply },
    Event(Event),
}

/// Read one newline-terminated line into `buf`, refusing to buffer more than
/// `max` bytes. `Ok(None)` at EOF. Shared by server and client so neither
/// side can be made to allocate without bound.
pub async fn read_line_capped<R>(
    rd: &mut R,
    buf: &mut Vec<u8>,
    max: usize,
) -> std::io::Result<Option<()>>
where
    R: tokio::io::AsyncBufRead + Unpin,
{
    use tokio::io::AsyncBufReadExt;
    buf.clear();
    loop {
        let available = rd.fill_buf().await?;
        if available.is_empty() {
            return if buf.is_empty() { Ok(None) } else { Ok(Some(())) };
        }
        let (chunk, done) = match available.iter().position(|b| *b == NEWLINE) {
            Some(i) => (&available[..i], true),
            None => (available, false),
        };
        if buf.len() + chunk.len() > max {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("ipc line exceeds {max} bytes"),
            ));
        }
        buf.extend_from_slice(chunk);
        let used = chunk.len() + usize::from(done);
        rd.consume(used);
        if done {
            return Ok(Some(()));
        }
    }
}

#[cfg(windows)]
pub mod security {
    //! Security descriptor that admits only the current user.
    use std::ffi::c_void;

    use anyhow::{Context, Result};
    use windows::core::{PCWSTR, PWSTR};
    use windows::Win32::Foundation::{CloseHandle, LocalFree, HANDLE, HLOCAL};
    use windows::Win32::Security::Authorization::{
        ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
        SDDL_REVISION_1,
    };
    use windows::Win32::Security::{
        GetTokenInformation, TokenUser, PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES, TOKEN_QUERY,
        TOKEN_USER,
    };
    use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    /// String SID of the user this process runs as, e.g. `S-1-5-21-...-1001`.
    pub fn current_user_sid() -> Result<String> {
        let mut token = HANDLE::default();
        // SAFETY: opening our own token for read.
        unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) }
            .context("opening process token")?;
        let result = (|| -> Result<String> {
            let mut len = 0u32;
            // SAFETY: size query; a failure with ERROR_INSUFFICIENT_BUFFER is expected.
            let _ = unsafe { GetTokenInformation(token, TokenUser, None, 0, &mut len) };
            anyhow::ensure!(len > 0, "GetTokenInformation reported zero size");
            let mut buf = vec![0u8; len as usize];
            // SAFETY: `buf` is `len` bytes, as reported by the size query.
            unsafe {
                GetTokenInformation(
                    token,
                    TokenUser,
                    Some(buf.as_mut_ptr() as *mut c_void),
                    len,
                    &mut len,
                )
            }
            .context("reading token user")?;
            // SAFETY: the buffer holds a TOKEN_USER written by the OS.
            let user = unsafe { &*(buf.as_ptr() as *const TOKEN_USER) };
            let mut s = PWSTR::null();
            // SAFETY: `user.User.Sid` is valid for the life of `buf`.
            unsafe { ConvertSidToStringSidW(user.User.Sid, &mut s) }.context("SID to string")?;
            // SAFETY: `s` is a NUL-terminated string allocated by the OS.
            let out = unsafe { s.to_string() }?;
            // SAFETY: freeing exactly what ConvertSidToStringSidW allocated.
            unsafe {
                let _ = LocalFree(Some(HLOCAL(s.0 as *mut c_void)));
            }
            Ok(out)
        })();
        // SAFETY: closing the token handle we opened above.
        unsafe {
            let _ = CloseHandle(token);
        }
        result
    }

    /// SDDL granting generic-all to one SID and nothing to anyone else.
    pub fn sddl_for(sid: &str) -> String {
        format!("D:P(A;;GA;;;{sid})")
    }

    /// `SECURITY_ATTRIBUTES` pointing at a protected DACL for the current user.
    pub struct PipeSecurity {
        sd: PSECURITY_DESCRIPTOR,
        attrs: SECURITY_ATTRIBUTES,
    }

    // SAFETY: the descriptor is an immutable OS allocation owned by this struct.
    unsafe impl Send for PipeSecurity {}

    impl PipeSecurity {
        pub fn current_user_only() -> Result<Self> {
            let sddl = sddl_for(&current_user_sid()?);
            let wide: Vec<u16> = sddl.encode_utf16().chain(std::iter::once(0)).collect();
            let mut sd = PSECURITY_DESCRIPTOR::default();
            // SAFETY: `wide` is NUL-terminated; `sd` receives an OS allocation.
            unsafe {
                ConvertStringSecurityDescriptorToSecurityDescriptorW(
                    PCWSTR(wide.as_ptr()),
                    SDDL_REVISION_1,
                    &mut sd,
                    None,
                )
            }
            .with_context(|| format!("parsing SDDL {sddl}"))?;
            let attrs = SECURITY_ATTRIBUTES {
                nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
                lpSecurityDescriptor: sd.0,
                bInheritHandle: false.into(),
            };
            Ok(Self { sd, attrs })
        }

        /// Raw pointer for `ServerOptions::create_with_security_attributes_raw`.
        pub fn as_ptr(&mut self) -> *mut c_void {
            &mut self.attrs as *mut SECURITY_ATTRIBUTES as *mut c_void
        }
    }

    impl Drop for PipeSecurity {
        fn drop(&mut self) {
            // SAFETY: freeing the descriptor allocated by the conversion call.
            unsafe {
                let _ = LocalFree(Some(HLOCAL(self.sd.0)));
            }
        }
    }
}

#[cfg(windows)]
pub mod server {
    use super::*;
    use crate::config::pipe_name;
    use anyhow::{Context, Result};
    use std::future::Future;
    use std::sync::Arc;
    use tokio::io::{AsyncWriteExt, BufReader};
    use tokio::net::windows::named_pipe::{NamedPipeServer, ServerOptions};
    use tokio::sync::broadcast;
    use tracing::{debug, warn};

    /// Handles one decoded request. Implemented by the service.
    pub trait Handler: Send + Sync + 'static {
        fn handle(&self, method: Method) -> impl Future<Output = Reply> + Send;
    }

    fn create(
        name: &str,
        sec: &mut security::PipeSecurity,
        first: bool,
    ) -> Result<NamedPipeServer> {
        // SAFETY: `sec` outlives the call and points at a valid SECURITY_ATTRIBUTES.
        unsafe {
            ServerOptions::new()
                .first_pipe_instance(first)
                .reject_remote_clients(true)
                .create_with_security_attributes_raw(name, sec.as_ptr())
        }
        .with_context(|| format!("creating pipe {name}"))
    }

    pub async fn serve<H: Handler>(
        handler: Arc<H>,
        events: broadcast::Sender<Event>,
    ) -> Result<()> {
        let name = pipe_name();
        let mut sec = security::PipeSecurity::current_user_only()?;
        let mut server = create(&name, &mut sec, true)?;
        loop {
            server.connect().await?;
            let connected = server;
            server = create(&name, &mut sec, false)?;
            let h = handler.clone();
            let rx = events.subscribe();
            tokio::spawn(async move {
                if let Err(e) = connection(connected, h, rx).await {
                    debug!(error = %e, "ipc connection closed");
                }
            });
        }
    }

    async fn connection<H: Handler>(
        pipe: NamedPipeServer,
        handler: Arc<H>,
        mut events: broadcast::Receiver<Event>,
    ) -> Result<()> {
        let (rd, mut wr) = tokio::io::split(pipe);
        let mut rd = BufReader::new(rd);
        let mut buf = Vec::new();
        let mut subscribed = false;
        loop {
            let idle = tokio::time::sleep(IDLE_TIMEOUT);
            tokio::select! {
                line = read_line_capped(&mut rd, &mut buf, IPC_MAX_LINE) => {
                    if line?.is_none() { return Ok(()) }
                    let text = std::str::from_utf8(&buf).unwrap_or("");
                    if text.trim().is_empty() { continue; }
                    let out = match serde_json::from_str::<Request>(text) {
                        Ok(req) => {
                            if matches!(req.method, Method::Subscribe) { subscribed = true; }
                            let handled = tokio::time::timeout(REQUEST_TIMEOUT, handler.handle(req.method));
                            let result = match handled.await {
                                Ok(r) => r,
                                Err(_) => Reply::Error { message: "request timed out".into() },
                            };
                            Outbound::Response { id: req.id, result }
                        }
                        Err(e) => Outbound::Response {
                            id: 0,
                            result: Reply::Error { message: format!("bad request: {e}") },
                        },
                    };
                    write_line(&mut wr, &out).await?;
                }
                ev = events.recv(), if subscribed => {
                    match ev {
                        Ok(ev) => write_line(&mut wr, &Outbound::Event(ev)).await?,
                        Err(broadcast::error::RecvError::Lagged(n)) => warn!(n, "ipc subscriber lagged"),
                        Err(broadcast::error::RecvError::Closed) => return Ok(()),
                    }
                }
                _ = idle, if !subscribed => {
                    debug!("ipc connection idle; closing");
                    return Ok(());
                }
            }
        }
    }

    async fn write_line<W: AsyncWriteExt + Unpin>(w: &mut W, msg: &Outbound) -> Result<()> {
        let mut bytes = serde_json::to_vec(msg)?;
        bytes.push(NEWLINE);
        w.write_all(&bytes).await?;
        w.flush().await?;
        Ok(())
    }
}

#[cfg(windows)]
pub mod client {
    use super::*;
    use crate::config::pipe_name;
    use anyhow::{bail, Context, Result};
    use tokio::io::{AsyncWriteExt, BufReader, ReadHalf, WriteHalf};
    use tokio::net::windows::named_pipe::{ClientOptions, NamedPipeClient};
    use windows::Win32::Foundation::ERROR_PIPE_BUSY;

    pub struct Client {
        reader: BufReader<ReadHalf<NamedPipeClient>>,
        writer: WriteHalf<NamedPipeClient>,
        buf: Vec<u8>,
        next_id: u64,
    }

    impl Client {
        /// Connect to the pipe named by [`pipe_name`], retrying briefly if every
        /// instance is busy.
        pub async fn connect() -> Result<Self> {
            Self::connect_to(&pipe_name()).await
        }

        pub async fn connect_to(name: &str) -> Result<Self> {
            let pipe = loop {
                match ClientOptions::new().open(name) {
                    Ok(p) => break p,
                    Err(e) if e.raw_os_error() == Some(ERROR_PIPE_BUSY.0 as i32) => {
                        tokio::time::sleep(Duration::from_millis(50)).await;
                    }
                    Err(e) => {
                        return Err(e).context("connecting to relay-core (is the service running?)")
                    }
                }
            };
            let (rd, writer) = tokio::io::split(pipe);
            Ok(Self { reader: BufReader::new(rd), writer, buf: Vec::new(), next_id: 1 })
        }

        async fn next_line(&mut self) -> Result<Option<Outbound>> {
            match read_line_capped(&mut self.reader, &mut self.buf, IPC_MAX_LINE).await? {
                None => Ok(None),
                Some(()) => Ok(Some(serde_json::from_slice(&self.buf)?)),
            }
        }

        pub async fn call(&mut self, method: Method) -> Result<Reply> {
            let id = self.next_id;
            self.next_id += 1;
            let mut bytes = serde_json::to_vec(&Request { id, method })?;
            bytes.push(NEWLINE);
            self.writer.write_all(&bytes).await?;
            self.writer.flush().await?;
            loop {
                let Some(msg) = self.next_line().await? else {
                    bail!("relay-core closed the connection")
                };
                match msg {
                    Outbound::Response { id: rid, result } if rid == id => return Ok(result),
                    Outbound::Response { .. } => continue,
                    Outbound::Event(_) => continue, // caller uses `next_event` for those
                }
            }
        }

        /// After `Method::Subscribe`, await pushed events.
        pub async fn next_event(&mut self) -> Result<Option<Event>> {
            loop {
                let Some(msg) = self.next_line().await? else { return Ok(None) };
                if let Outbound::Event(ev) = msg {
                    return Ok(Some(ev));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_wire_shape_is_flat() {
        let r = Request { id: 7, method: Method::GetProfile { id: Uuid::nil() } };
        let v: serde_json::Value = serde_json::to_value(&r).unwrap();
        assert_eq!(v["id"], 7);
        assert_eq!(v["method"], "get_profile");
        assert_eq!(v["params"]["id"], Uuid::nil().to_string());
        let back: Request = serde_json::from_value(v).unwrap();
        assert!(matches!(back.method, Method::GetProfile { .. }));
    }

    #[test]
    fn outbound_distinguishes_response_from_event() {
        let resp = Outbound::Response { id: 1, result: Reply::Pong };
        let ev = Outbound::Event(Event::Notice { text: "hi".into() });
        let r: Outbound = serde_json::from_str(&serde_json::to_string(&resp).unwrap()).unwrap();
        let e: Outbound = serde_json::from_str(&serde_json::to_string(&ev).unwrap()).unwrap();
        assert!(matches!(r, Outbound::Response { id: 1, .. }));
        assert!(matches!(e, Outbound::Event(Event::Notice { .. })));
    }

    #[tokio::test]
    async fn capped_reader_splits_lines_and_rejects_oversize() {
        let data: &[u8] = b"one\ntwo\n";
        let mut rd = tokio::io::BufReader::new(data);
        let mut buf = Vec::new();
        assert!(read_line_capped(&mut rd, &mut buf, 16).await.unwrap().is_some());
        assert_eq!(buf, b"one");
        assert!(read_line_capped(&mut rd, &mut buf, 16).await.unwrap().is_some());
        assert_eq!(buf, b"two");
        assert!(read_line_capped(&mut rd, &mut buf, 16).await.unwrap().is_none());

        let big = [b'x'; 64];
        let mut rd = tokio::io::BufReader::new(&big[..]);
        let err = read_line_capped(&mut rd, &mut buf, 16).await.unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
    }

    #[cfg(windows)]
    #[test]
    fn sddl_names_current_user_only() {
        let sid = security::current_user_sid().unwrap();
        assert!(sid.starts_with("S-1-"), "got {sid}");
        assert_eq!(security::sddl_for(&sid), format!("D:P(A;;GA;;;{sid})"));
        let mut sec = security::PipeSecurity::current_user_only().unwrap();
        assert!(!sec.as_ptr().is_null());
    }
}
