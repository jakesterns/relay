//! Global hotkey definitions. Registration happens on the Win32 message-loop
//! thread (see `winloop`), because `RegisterHotKey` binds to the calling thread.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HotkeyAction {
    ToggleShare,
    ToggleProfile,
    TogglePreview,
}

impl HotkeyAction {
    /// Stable numeric id passed to `RegisterHotKey` and echoed in `WM_HOTKEY`.
    pub fn id(self) -> i32 {
        match self {
            HotkeyAction::ToggleShare => 1,
            HotkeyAction::ToggleProfile => 2,
            HotkeyAction::TogglePreview => 3,
        }
    }

    pub fn from_id(id: i32) -> Option<Self> {
        match id {
            1 => Some(HotkeyAction::ToggleShare),
            2 => Some(HotkeyAction::ToggleProfile),
            3 => Some(HotkeyAction::TogglePreview),
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

/// Ctrl+Alt+S share, Ctrl+Alt+G profile, Ctrl+Alt+P preview. Chosen to stay out
/// of the way of common game binds; user-configurable later.
pub fn defaults() -> Vec<Hotkey> {
    let ca = Modifiers { ctrl: true, alt: true, ..Default::default() };
    vec![
        Hotkey { action: HotkeyAction::ToggleShare, modifiers: ca, vk: b'S' as u32 },
        Hotkey { action: HotkeyAction::ToggleProfile, modifiers: ca, vk: b'G' as u32 },
        Hotkey { action: HotkeyAction::TogglePreview, modifiers: ca, vk: b'P' as u32 },
    ]
}
