//! `relay-core` binary.
//!
//! ```text
//! relay-core run              start the always-on service (foreground)
//! relay-core status           print current state from a running service
//! relay-core restore          ask a running service to restore everything
//! relay-core shutdown         stop a running service (restores first)
//! relay-core --data-dir DIR   override the data directory (any subcommand)
//! ```

use anyhow::Result;
use relay_core::config::Paths;
use relay_core::service::{Backends, Service};
use tracing_subscriber::EnvFilter;

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .with_target(false)
        .compact()
        .init();

    let mut args = std::env::args().skip(1);
    let mut cmd = String::from("run");
    let mut paths = Paths::default_for_user()?;
    while let Some(a) = args.next() {
        match a.as_str() {
            "--data-dir" => {
                let dir = args.next().ok_or_else(|| anyhow::anyhow!("--data-dir needs a path"))?;
                paths = Paths::at(dir);
            }
            "-h" | "--help" => {
                print!("{}", USAGE);
                return Ok(());
            }
            other => cmd = other.to_string(),
        }
    }

    match cmd.as_str() {
        "run" => Service::run(paths, Backends::default()),
        #[cfg(windows)]
        "status" | "restore" | "shutdown" => client_command(&cmd),
        other => anyhow::bail!("unknown command `{other}`\n{USAGE}"),
    }
}

const USAGE: &str = "\
relay-core [--data-dir DIR] [run|status|restore|shutdown]

  run       start the always-on service in the foreground (default)
  status    print the state of a running service as JSON
  restore   restore original audio/display state now
  shutdown  stop the running service (it restores first)
";

#[cfg(windows)]
fn client_command(cmd: &str) -> Result<()> {
    use relay_core::ipc::{client::Client, Method, Reply};
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    rt.block_on(async {
        let mut c = Client::connect().await?;
        let method = match cmd {
            "status" => Method::Status,
            "restore" => Method::RestoreAll,
            _ => Method::Shutdown,
        };
        match c.call(method).await? {
            Reply::Status { state } => println!("{}", serde_json::to_string_pretty(&*state)?),
            Reply::Error { message } => anyhow::bail!("{message}"),
            other => println!("{}", serde_json::to_string(&other)?),
        }
        Ok(())
    })
}
