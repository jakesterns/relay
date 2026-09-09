//! Service logging: `<root>/logs/core.log` with size-based rotation
//! (1 MB x 3 by default) plus stderr while a console is attached.
//!
//! Hand-rolled rather than `tracing-appender`, which only rotates by time.
//! The writer checks the file size after every line; at rest nothing is
//! written, so this costs nothing.

use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use parking_lot::Mutex;
use tracing_subscriber::filter::LevelFilter;
use tracing_subscriber::fmt::MakeWriter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::Layer;

pub const MAX_BYTES: u64 = 1024 * 1024;
pub const KEEP: usize = 3;

/// Append-only file that rotates itself: `core.log`, `core.log.1`, ..., `core.log.N`.
pub struct RotatingFile {
    path: PathBuf,
    max_bytes: u64,
    keep: usize,
    file: Option<File>,
    written: u64,
}

impl RotatingFile {
    pub fn open(path: impl Into<PathBuf>, max_bytes: u64, keep: usize) -> Result<Self> {
        let path = path.into();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut s = Self { path, max_bytes, keep, file: None, written: 0 };
        s.reopen()?;
        Ok(s)
    }

    fn reopen(&mut self) -> Result<()> {
        let f = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .with_context(|| format!("opening {}", self.path.display()))?;
        self.written = f.metadata().map(|m| m.len()).unwrap_or(0);
        self.file = Some(f);
        Ok(())
    }

    fn rotated_name(&self, n: usize) -> PathBuf {
        let mut os = self.path.clone().into_os_string();
        os.push(format!(".{n}"));
        PathBuf::from(os)
    }

    /// Shift every archive up by one, drop the oldest, start a fresh file.
    fn rotate(&mut self) -> Result<()> {
        self.file = None;
        let _ = std::fs::remove_file(self.rotated_name(self.keep));
        for n in (1..self.keep).rev() {
            let from = self.rotated_name(n);
            if from.exists() {
                let _ = std::fs::rename(&from, self.rotated_name(n + 1));
            }
        }
        if self.keep >= 1 {
            let _ = std::fs::rename(&self.path, self.rotated_name(1));
        }
        self.reopen()
    }
}

impl Write for RotatingFile {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if self.written > 0 && self.written + buf.len() as u64 > self.max_bytes {
            self.rotate().map_err(|e| io::Error::other(e.to_string()))?;
        }
        let f = self.file.as_mut().ok_or_else(|| io::Error::other("log file closed"))?;
        f.write_all(buf)?;
        self.written += buf.len() as u64;
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        if let Some(f) = self.file.as_mut() {
            f.flush()?;
        }
        Ok(())
    }
}

/// `MakeWriter` adapter: one shared file behind a mutex.
#[derive(Clone)]
pub struct SharedWriter(Arc<Mutex<RotatingFile>>);

pub struct SharedGuard<'a>(parking_lot::MutexGuard<'a, RotatingFile>);

impl Write for SharedGuard<'_> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.write(buf)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.0.flush()
    }
}

impl<'a> MakeWriter<'a> for SharedWriter {
    type Writer = SharedGuard<'a>;
    fn make_writer(&'a self) -> Self::Writer {
        SharedGuard(self.0.lock())
    }
}

/// `--verbose` selects debug; `RELAY_LOG=trace|debug|info|warn|error|off`
/// overrides both. Deliberately a plain level filter, not an env filter: the
/// regex engine behind the latter costs more code and memory than the
/// always-on core is allowed.
fn filter(verbose: bool) -> LevelFilter {
    let env = std::env::var("RELAY_LOG").ok().map(|v| v.to_ascii_lowercase());
    match env.as_deref() {
        Some("trace") => LevelFilter::TRACE,
        Some("debug") => LevelFilter::DEBUG,
        Some("info") => LevelFilter::INFO,
        Some("warn") => LevelFilter::WARN,
        Some("error") => LevelFilter::ERROR,
        Some("off") => LevelFilter::OFF,
        _ if verbose => LevelFilter::DEBUG,
        _ => LevelFilter::INFO,
    }
}

/// Stderr only. Used by the one-shot CLI subcommands.
pub fn init_console(verbose: bool) {
    let _ = tracing_subscriber::fmt()
        .with_max_level(filter(verbose))
        .with_target(false)
        .with_writer(io::stderr)
        .compact()
        .try_init();
}

/// Rotating file plus stderr (when a console is attached). Used by `run`.
pub fn init_service(log_file: &Path, verbose: bool) -> Result<()> {
    let file = RotatingFile::open(log_file, MAX_BYTES, KEEP)?;
    let file_layer = tracing_subscriber::fmt::layer()
        .with_ansi(false)
        .with_target(false)
        .compact()
        .with_writer(SharedWriter(Arc::new(Mutex::new(file))))
        .with_filter(filter(verbose));
    let stderr_layer = has_console().then(|| {
        tracing_subscriber::fmt::layer()
            .with_target(false)
            .with_writer(io::stderr)
            .compact()
            .with_filter(filter(verbose))
    });
    tracing_subscriber::registry().with(file_layer).with(stderr_layer).try_init()?;
    Ok(())
}

/// True when stderr goes somewhere (console, pipe or file).
#[cfg(windows)]
fn has_console() -> bool {
    use windows::Win32::System::Console::{GetStdHandle, STD_ERROR_HANDLE};
    // SAFETY: querying our own standard handle.
    unsafe { GetStdHandle(STD_ERROR_HANDLE).is_ok() }
}

#[cfg(not(windows))]
fn has_console() -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rotates_when_full_and_keeps_n_files() {
        let dir = std::env::temp_dir().join(format!("relay-log-{}", uuid::Uuid::new_v4()));
        let path = dir.join("core.log");
        let mut f = RotatingFile::open(&path, 100, 3).unwrap();
        let line = [b'x'; 40];
        for _ in 0..12 {
            f.write_all(&line).unwrap();
            f.write_all(b"\n").unwrap();
        }
        f.flush().unwrap();
        assert!(path.exists());
        assert!(dir.join("core.log.1").exists());
        assert!(dir.join("core.log.2").exists());
        assert!(dir.join("core.log.3").exists());
        assert!(!dir.join("core.log.4").exists(), "never more than `keep` archives");
        for n in 1..=3 {
            let len = std::fs::metadata(dir.join(format!("core.log.{n}"))).unwrap().len();
            assert!(len <= 100, "archive {n} is {len} bytes");
        }
        let _ = std::fs::remove_dir_all(dir);
    }
}
