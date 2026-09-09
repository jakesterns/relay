//! `relay-share` — the per-share engine process, spawned by the core.
//!
//! ```text
//! relay-share probe                capability report (MFTEnumEx, WGC) as JSON
//! relay-share bench-capture [SECS] capture-only latency benchmark
//! relay-share bench-encode [SECS]  capture → NV12 → HEVC encode benchmark
//! relay-share send                 share the primary monitor to a paired peer
//! relay-share recv                 receive and render to a window
//! ```
//!
//! Stats and lifecycle messages go to stdout as NDJSON; the core relays them
//! to the UI. `stop\n` on stdin asks for a graceful teardown.

use anyhow::{bail, Result};

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cmd = args.first().map(String::as_str).unwrap_or("");

    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_max_level(if std::env::var("RELAY_LOG").is_ok() {
            tracing::Level::DEBUG
        } else {
            tracing::Level::INFO
        })
        .init();

    match cmd {
        #[cfg(windows)]
        "probe" => {
            let _mf = relay_capture::probe::MediaFoundation::start()?;
            let report = relay_capture::probe::report()?;
            println!("{}", serde_json::to_string_pretty(&report)?);
            Ok(())
        }
        "" | "-h" | "--help" => {
            print!("{USAGE}");
            Ok(())
        }
        other => bail!("unknown command `{other}`\n{USAGE}"),
    }
}

const USAGE: &str = "\
relay-share [probe|bench-capture [SECS]|bench-encode [SECS]|send|recv]

  probe          print the capability report (hardware HEVC MFTs, WGC) as JSON
  bench-capture  measure capture latency on the primary monitor
  bench-encode   measure capture -> NV12 -> HEVC hardware encode latency
  send           share to a paired peer (spawned by relay-core)
  recv           receive a share and render it to a window
";
