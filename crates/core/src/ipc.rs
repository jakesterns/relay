//! IPC between the core and the Tauri shell (or the CLI).
//!
//! Transport: a Windows named pipe, newline-delimited JSON, one message per
//! line. Requests carry an `id` that is echoed on the response. After a
//! `subscribe` request the server also pushes `event` lines on that connection.
//!
//! The TypeScript mirror of these shapes is `ui/src/lib/ipc.ts`.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::types::{CoreState, Profile, ProfileSummary};

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
    Ok,
    Error { message: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum Event {
    StateChanged { state: Box<CoreState> },
    Notice { text: String },
}

/// One line on the wire is exactly one of these.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Outbound {
    Response { id: u64, result: Reply },
    Event(Event),
}

#[cfg(windows)]
pub mod server {
    use super::*;
    use crate::config::PIPE_NAME;
    use anyhow::Result;
    use std::future::Future;
    use std::sync::Arc;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::net::windows::named_pipe::{NamedPipeServer, ServerOptions};
    use tokio::sync::broadcast;
    use tracing::{debug, warn};

    /// Handles one decoded request. Implemented by the service.
    pub trait Handler: Send + Sync + 'static {
        fn handle(&self, method: Method) -> impl Future<Output = Reply> + Send;
    }

    pub async fn serve<H: Handler>(
        handler: Arc<H>,
        events: broadcast::Sender<Event>,
    ) -> Result<()> {
        let mut server = ServerOptions::new().first_pipe_instance(true).create(PIPE_NAME)?;
        loop {
            server.connect().await?;
            let connected = server;
            server = ServerOptions::new().create(PIPE_NAME)?;
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
        let mut lines = BufReader::new(rd).lines();
        let mut subscribed = false;
        loop {
            tokio::select! {
                line = lines.next_line() => {
                    let Some(line) = line? else { return Ok(()) };
                    if line.trim().is_empty() { continue; }
                    let out = match serde_json::from_str::<Request>(&line) {
                        Ok(req) => {
                            if matches!(req.method, Method::Subscribe) { subscribed = true; }
                            let result = handler.handle(req.method).await;
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
            }
        }
    }

    async fn write_line<W: AsyncWriteExt + Unpin>(w: &mut W, msg: &Outbound) -> Result<()> {
        let mut bytes = serde_json::to_vec(msg)?;
        bytes.push(b'\n');
        w.write_all(&bytes).await?;
        w.flush().await?;
        Ok(())
    }
}

#[cfg(windows)]
pub mod client {
    use super::*;
    use crate::config::PIPE_NAME;
    use anyhow::{bail, Context, Result};
    use std::time::Duration;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, ReadHalf, WriteHalf};
    use tokio::net::windows::named_pipe::{ClientOptions, NamedPipeClient};
    use windows::Win32::Foundation::ERROR_PIPE_BUSY;

    pub struct Client {
        reader: tokio::io::Lines<BufReader<ReadHalf<NamedPipeClient>>>,
        writer: WriteHalf<NamedPipeClient>,
        next_id: u64,
    }

    impl Client {
        /// Connect, retrying briefly if every pipe instance is busy.
        pub async fn connect() -> Result<Self> {
            let pipe = loop {
                match ClientOptions::new().open(PIPE_NAME) {
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
            Ok(Self { reader: BufReader::new(rd).lines(), writer, next_id: 1 })
        }

        pub async fn call(&mut self, method: Method) -> Result<Reply> {
            let id = self.next_id;
            self.next_id += 1;
            let mut bytes = serde_json::to_vec(&Request { id, method })?;
            bytes.push(b'\n');
            self.writer.write_all(&bytes).await?;
            self.writer.flush().await?;
            loop {
                let Some(line) = self.reader.next_line().await? else {
                    bail!("relay-core closed the connection")
                };
                match serde_json::from_str::<Outbound>(&line)? {
                    Outbound::Response { id: rid, result } if rid == id => return Ok(result),
                    Outbound::Response { .. } => continue,
                    Outbound::Event(_) => continue, // caller uses `next_event` for those
                }
            }
        }

        /// After `Method::Subscribe`, await pushed events.
        pub async fn next_event(&mut self) -> Result<Option<Event>> {
            loop {
                let Some(line) = self.reader.next_line().await? else { return Ok(None) };
                if let Outbound::Event(ev) = serde_json::from_str::<Outbound>(&line)? {
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
}
