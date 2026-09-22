//! Crash records (S38): what was lost when a Relay process died.
//!
//! `share.log`, `ui.log` and `core.log` survive a crash, but a panic's message
//! and location went to stderr, which nobody was reading, and an engine that
//! exited non-zero left only "stopped unexpectedly" behind. So every binary
//! installs a panic hook that writes one small file under `logs\crash\`, and
//! the core writes one for an engine exit it did not ask for — with the tail
//! of `share.log`, because that is where the reason usually is.
//!
//! The next time the core starts it names the newest unseen file once, then
//! marks the directory seen. Once: a crash is worth one sentence, not a nag.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};

/// Lines of the engine log kept with an exit record.
pub const LOG_TAIL_LINES: usize = 50;
const SEEN_MARKER: &str = ".seen";

/// `logs\crash\` under the data root.
pub fn dir(paths: &crate::config::Paths) -> PathBuf {
    paths.log_dir().join("crash")
}

fn now_unix() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// Write one record. Returns its path. Never panics — this runs inside a
/// panic hook, where a second panic aborts the process before anything is
/// written at all.
fn write_record(dir: &Path, binary: &str, kind: &str, body: &str) -> Option<PathBuf> {
    std::fs::create_dir_all(dir).ok()?;
    let path = dir.join(format!("{}-{binary}-{kind}.txt", now_unix()));
    let text = format!("binary: {binary}\nkind: {kind}\nunix: {}\n\n{body}\n", now_unix());
    std::fs::write(&path, text).ok()?;
    Some(path)
}

/// Install a panic hook for this process that records the panic to `dir`
/// and then behaves as before (the default hook still prints to stderr).
///
/// Chained, not replaced: whatever the process had — a test harness's hook,
/// or the default — still runs, so nothing that used to be visible goes
/// quiet because of this.
pub fn install_panic_hook(dir: PathBuf, binary: &'static str) {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let message = info
            .payload()
            .downcast_ref::<&str>()
            .map(|s| s.to_string())
            .or_else(|| info.payload().downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "(non-string panic payload)".to_string());
        let location = info
            .location()
            .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()))
            .unwrap_or_else(|| "unknown".to_string());
        let thread = std::thread::current().name().unwrap_or("unnamed").to_string();
        let body = format!("panic: {message}\nat: {location}\nthread: {thread}");
        let _ = write_record(&dir, binary, "panic", &body);
        previous(info);
    }));
}

/// The core asked for nothing and the engine exited anyway. `log` is the
/// engine's log file, whose tail is kept with the record.
pub fn record_engine_exit(
    dir: &Path,
    binary: &str,
    code: Option<i32>,
    log: Option<&Path>,
) -> Option<PathBuf> {
    let code_text = code.map(|c| c.to_string()).unwrap_or_else(|| "signal".into());
    let mut body = format!("exit: {code_text}");
    if let Some(log) = log {
        body.push_str(&format!("\n\n--- last {LOG_TAIL_LINES} lines of {} ---\n", log.display()));
        body.push_str(&tail_lines(log, LOG_TAIL_LINES).unwrap_or_default());
    }
    write_record(dir, binary, &format!("exit{code_text}"), &body)
}

/// The last `n` lines of a text file. Reads the whole file: these logs
/// rotate at 1 MB, so that is the ceiling.
pub fn tail_lines(path: &Path, n: usize) -> Result<String> {
    let text = std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    let lines: Vec<&str> = text.lines().collect();
    let start = lines.len().saturating_sub(n);
    Ok(lines[start..].join("\n"))
}

/// The newest record written since the directory was last marked seen.
pub fn unseen(dir: &Path) -> Option<PathBuf> {
    let seen_at = std::fs::metadata(dir.join(SEEN_MARKER)).and_then(|m| m.modified()).ok();
    let mut newest: Option<(SystemTime, PathBuf)> = None;
    for entry in std::fs::read_dir(dir).ok()?.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("txt") {
            continue;
        }
        let Ok(modified) = entry.metadata().and_then(|m| m.modified()) else { continue };
        if seen_at.is_some_and(|s| modified <= s) {
            continue;
        }
        if newest.as_ref().is_none_or(|(t, _)| modified > *t) {
            newest = Some((modified, path));
        }
    }
    newest.map(|(_, p)| p)
}

/// Everything currently in the directory has been reported.
pub fn mark_seen(dir: &Path) {
    let _ = std::fs::create_dir_all(dir);
    let _ = std::fs::write(dir.join(SEEN_MARKER), now_unix().to_string());
}

/// One sentence for the user, from a record's file name and first lines.
pub fn summary(path: &Path) -> String {
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("a crash record");
    let head = std::fs::read_to_string(path)
        .ok()
        .and_then(|t| {
            t.lines()
                .find(|l| l.starts_with("panic: ") || l.starts_with("exit: "))
                .map(str::to_string)
        })
        .unwrap_or_default();
    if head.is_empty() {
        format!("Relay did not shut down cleanly last time ({name}).")
    } else {
        format!("Relay did not shut down cleanly last time: {head} ({name}).")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("relay-crash-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn an_engine_exit_is_recorded_with_the_log_tail() {
        let dir = tmp("exit");
        let log = std::env::temp_dir().join(format!("relay-crash-log-{}.txt", std::process::id()));
        let lines: Vec<String> = (1..=80).map(|i| format!("line {i}")).collect();
        std::fs::write(&log, lines.join("\n")).unwrap();

        let p = record_engine_exit(&dir, "relay-share", Some(3), Some(&log)).unwrap();
        let text = std::fs::read_to_string(&p).unwrap();
        assert!(text.contains("exit: 3"));
        assert!(text.contains("line 80"));
        assert!(text.contains("line 31"), "50 lines kept");
        assert!(!text.contains("line 30\n"), "not more than 50");
        assert!(p.file_name().unwrap().to_str().unwrap().contains("relay-share-exit3"));
    }

    #[test]
    fn unseen_reports_once_and_then_stays_quiet() {
        let dir = tmp("seen");
        assert!(unseen(&dir).is_none(), "no directory, nothing to report");
        let first = record_engine_exit(&dir, "relay-core", None, None).unwrap();
        assert_eq!(unseen(&dir).as_deref(), Some(first.as_path()));
        mark_seen(&dir);
        // The marker's mtime has second resolution on some filesystems, so
        // give the clock a moment before checking nothing is newer.
        std::thread::sleep(std::time::Duration::from_millis(1100));
        assert!(unseen(&dir).is_none());
        let second = record_engine_exit(&dir, "relay-core", Some(1), None).unwrap();
        assert_eq!(unseen(&dir).as_deref(), Some(second.as_path()));
    }

    #[test]
    fn a_summary_says_what_happened_in_one_line() {
        let dir = tmp("summary");
        let p = record_engine_exit(&dir, "relay-share", Some(-1073741819), None).unwrap();
        let s = summary(&p);
        assert!(
            s.starts_with("Relay did not shut down cleanly last time: exit: -1073741819"),
            "{s}"
        );
        assert!(!s.contains('\n'));
    }

    #[test]
    fn tail_of_a_short_file_is_the_whole_file() {
        let log =
            std::env::temp_dir().join(format!("relay-crash-short-{}.txt", std::process::id()));
        std::fs::write(&log, "a\nb\nc").unwrap();
        assert_eq!(tail_lines(&log, 50).unwrap(), "a\nb\nc");
    }
}
