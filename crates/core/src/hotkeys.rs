//! Global hotkey definitions. Registration happens on the Win32 message-loop
//! thread (see `winloop`), because `RegisterHotKey` binds to the calling thread.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HotkeyAction {
    ToggleShare,
    ToggleProfile,
    TogglePreview,
    SaveReplay,
    /// Step the default output to its next listening device (S41). Off by
    /// default; `UiPrefs::cycle_listening_hotkey` turns it on.
    CycleListening,
}

impl HotkeyAction {
    /// Stable numeric id passed to `RegisterHotKey` and echoed in `WM_HOTKEY`.
    pub fn id(self) -> i32 {
        match self {
            HotkeyAction::ToggleShare => 1,
            HotkeyAction::ToggleProfile => 2,
            HotkeyAction::TogglePreview => 3,
            HotkeyAction::SaveReplay => 4,
            HotkeyAction::CycleListening => 5,
        }
    }

    pub fn from_id(id: i32) -> Option<Self> {
        match id {
            1 => Some(HotkeyAction::ToggleShare),
            2 => Some(HotkeyAction::ToggleProfile),
            3 => Some(HotkeyAction::TogglePreview),
            4 => Some(HotkeyAction::SaveReplay),
            5 => Some(HotkeyAction::CycleListening),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Modifiers {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub win: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hotkey {
    pub action: HotkeyAction,
    pub modifiers: Modifiers,
    /// Win32 virtual-key code.
    pub vk: u32,
}

/// Ctrl+Alt+S share, Ctrl+Alt+G profile, Ctrl+Alt+P preview, Ctrl+Alt+R save
/// replay. Chosen to stay out of the way of common game binds;
/// user-configurable later.
pub fn defaults() -> Vec<Hotkey> {
    let ca = Modifiers { ctrl: true, alt: true, ..Default::default() };
    vec![
        Hotkey { action: HotkeyAction::ToggleShare, modifiers: ca, vk: b'S' as u32 },
        Hotkey { action: HotkeyAction::ToggleProfile, modifiers: ca, vk: b'G' as u32 },
        Hotkey { action: HotkeyAction::TogglePreview, modifiers: ca, vk: b'P' as u32 },
        Hotkey { action: HotkeyAction::SaveReplay, modifiers: ca, vk: b'R' as u32 },
    ]
}

/// The defaults, plus Ctrl+Alt+L to cycle listening devices when the user
/// turned it on (S41). Registered when the core starts.
pub fn with_prefs(cycle_listening: bool) -> Vec<Hotkey> {
    let mut out = defaults();
    if cycle_listening {
        let ca = Modifiers { ctrl: true, alt: true, ..Default::default() };
        out.push(Hotkey { action: HotkeyAction::CycleListening, modifiers: ca, vk: b'L' as u32 });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cycle_listening_is_off_by_default_and_round_trips_its_id() {
        assert!(!defaults().iter().any(|h| h.action == HotkeyAction::CycleListening));
        assert!(!with_prefs(false).iter().any(|h| h.action == HotkeyAction::CycleListening));
        assert!(with_prefs(true).iter().any(|h| h.action == HotkeyAction::CycleListening));
        let id = HotkeyAction::CycleListening.id();
        assert_eq!(HotkeyAction::from_id(id), Some(HotkeyAction::CycleListening));
        let mut ids: Vec<i32> = with_prefs(true).iter().map(|h| h.action.id()).collect();
        ids.dedup();
        assert_eq!(ids.len(), with_prefs(true).len());
    }
}
