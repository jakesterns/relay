//! Human-readable rendering of [`CoreState`] for `relay-core status`.

use std::fmt::Write;

use crate::types::{AudioChainState, CoreState, DisplayState, ShareState};

pub fn summary(state: &CoreState, autostart: bool) -> String {
    let mut s = String::new();
    let _ = writeln!(s, "relay-core: running");
    match &state.foreground {
        Some(fg) if fg.title.is_empty() => {
            let _ = writeln!(s, "  foreground   {} (pid {})", fg.exe, fg.pid);
        }
        Some(fg) => {
            let _ = writeln!(s, "  foreground   {} (pid {}) \"{}\"", fg.exe, fg.pid, fg.title);
        }
        None => {
            let _ = writeln!(s, "  foreground   none");
        }
    }
    match &state.active_profile {
        Some(p) => {
            let _ = writeln!(s, "  profile      {} ({})", p.name, p.exe);
        }
        None => {
            let _ = writeln!(s, "  profile      none");
        }
    }
    let audio = match state.audio_chain {
        AudioChainState::Bypass => "bypass (pass-through)",
        AudioChainState::Active => "active",
        AudioChainState::ExclusiveBypassed => "bypassed by game (WASAPI exclusive)",
    };
    let _ = writeln!(s, "  audio chain  {audio}");
    let display = match state.display_state {
        DisplayState::Default => "default (Windows settings)",
        DisplayState::Applied => "applied (backup on disk)",
    };
    let _ = writeln!(s, "  display      {display}");
    let share = match &state.sharing {
        ShareState::Off => "off".to_string(),
        ShareState::Sharing { peer } => format!("sending to {peer}"),
    };
    let _ = writeln!(s, "  share        {share}");
    let _ = writeln!(
        s,
        "  footprint    {:.1} MB, {:.1} % CPU",
        state.footprint.rss_bytes as f64 / (1024.0 * 1024.0),
        state.footprint.cpu_percent
    );
    let _ = writeln!(s, "  autostart    {}", if autostart { "on" } else { "off" });
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Foreground;

    #[test]
    fn renders_every_line() {
        let mut st = CoreState {
            foreground: Some(Foreground { pid: 42, exe: "cod.exe".into(), title: "CoD".into() }),
            ..Default::default()
        };
        st.footprint.rss_bytes = 9 * 1024 * 1024;
        let out = summary(&st, true);
        assert!(out.contains("cod.exe (pid 42) \"CoD\""));
        assert!(out.contains("profile      none"));
        assert!(out.contains("9.0 MB"));
        assert!(out.contains("autostart    on"));
    }
}
