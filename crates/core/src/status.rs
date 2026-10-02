//! Human-readable rendering of [`CoreState`] for `relay-core status`.

use std::fmt::Write;

use crate::types::{AudioChainState, CoreState, DisplayState, ShareState};

pub fn summary(state: &CoreState, autostart: bool) -> String {
    let mut s = String::new();
    let _ = writeln!(s, "relay-core: running");
    // Executable and pid only, never the window title. `status` output is the
    // thing people paste into bug reports and logs, and a title is whatever
    // happens to be on screen -- a document name, a browser tab, a private
    // message. It carried a browser tab title during testing and only stayed
    // out of this repo because the person reading it redacted it by hand.
    //
    // The title is still in `CoreState` because profile matching uses it
    // (`GameMatch::title_contains`); it is this rendering that must not leak.
    match &state.foreground {
        Some(fg) => {
            let _ = writeln!(s, "  foreground   {} (pid {})", fg.exe, fg.pid);
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
        AudioChainState::NotInstalled => "not audible: audio effects not installed",
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
        ShareState::Reconnecting { peer, attempt } => {
            format!("reconnecting to {peer} (attempt {attempt})")
        }
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
            foreground: Some(Foreground {
                pid: 42,
                exe: "cod.exe".into(),
                title: "CoD".into(),
                hmonitor: 0,
                hwnd: 0,
                image: String::new(),
            }),
            ..Default::default()
        };
        st.footprint.rss_bytes = 9 * 1024 * 1024;
        let out = summary(&st, true);
        assert!(out.contains("cod.exe (pid 42)"));
        // The window title must never reach this output: it is whatever is on
        // screen, and `status` is what people paste into bug reports.
        assert!(!out.contains("CoD"), "status leaked the foreground window title");
        assert!(out.contains("profile      none"));
        assert!(out.contains("9.0 MB"));
        assert!(out.contains("autostart    on"));
    }
}
