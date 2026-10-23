//! The notification-area icon, owned by the core.
//!
//! Deliberately *not* owned by the UI. Closing the Relay window is supposed to
//! free the whole Tauri process — that is the point of the split, and it is
//! what a user does before starting a game. A UI-owned tray icon would
//! therefore vanish exactly when it is the only remaining way to see that
//! Relay is still applying an audio and display profile, and the only way to
//! put the machine back. So the icon lives on the winloop's hidden window,
//! which exists for as long as the core does.
//!
//! The menu is three items and no state: Open Relay, Restore everything, Quit
//! Relay. Quit goes out through the ordinary shutdown path, which already
//! restores on the way out (`Service::run`), so there is no way to leave a
//! game profile applied by quitting from here.
//!
//! The icon itself does carry state (owner 2026-10-23): while a profile is
//! changing something, it wears a green dot and its tooltip names the
//! profile and what is on — "EQ + display", "EQ" or "Display" — so a closed
//! window still tells you Relay is working on the game in front.

/// Sent to the winloop window when the icon is clicked. `WM_APP + 1`; the
/// window class is ours alone, so no other message can collide with it.
#[cfg(windows)]
pub const WM_TRAY: u32 = windows::Win32::UI::WindowsAndMessaging::WM_APP + 1;

/// Menu command ids. Non-zero because `TPM_RETURNCMD` reports 0 for "the menu
/// was dismissed without choosing anything".
pub const ID_OPEN: u32 = 1;
pub const ID_RESTORE: u32 = 2;
pub const ID_QUIT: u32 = 3;
/// S41: listening devices of the default output are `ID_LISTEN_BASE + index`.
pub const ID_LISTEN_BASE: u32 = 100;
/// More than this many listening devices on one output are not listed.
pub const MAX_LISTEN: usize = 16;

/// One listening-device line in the menu.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListenItem {
    pub label: String,
    pub active: bool,
}

/// The default output's listening devices, published by the service whenever
/// the hardware view changes and read by the menu when it opens. A snapshot,
/// so the menu never takes the service lock; the service re-checks the index
/// against the live list when a pick comes back.
static LISTENING: std::sync::Mutex<Vec<ListenItem>> = std::sync::Mutex::new(Vec::new());

pub fn set_listening_menu(items: Vec<ListenItem>) {
    if let Ok(mut g) = LISTENING.lock() {
        *g = items;
    }
}

pub fn listening_menu() -> Vec<ListenItem> {
    LISTENING.lock().map(|g| g.clone()).unwrap_or_default()
}

/// What the user picked from the icon.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayCommand {
    /// Show the Relay window: focus the running one, or start it.
    Open,
    /// Put audio and display back the way Windows had them.
    Restore,
    /// Stop the core (restoring on the way out) and close the window.
    Quit,
    /// Make the default output's n-th listening device active (S41).
    Listen(usize),
}

impl TrayCommand {
    /// Menu id → command. `None` for 0 (dismissed) and anything unknown.
    pub fn from_id(id: u32) -> Option<Self> {
        match id {
            ID_OPEN => Some(Self::Open),
            ID_RESTORE => Some(Self::Restore),
            ID_QUIT => Some(Self::Quit),
            n if (ID_LISTEN_BASE..ID_LISTEN_BASE + MAX_LISTEN as u32).contains(&n) => {
                Some(Self::Listen((n - ID_LISTEN_BASE) as usize))
            }
            _ => None,
        }
    }
}

/// Tooltip text, which is the only place the background story is told outside
/// the app window. Kept to the 127-char `szTip` limit.
pub const TOOLTIP: &str = "Relay is running in the background";

/// `NOTIFYICONDATAW::szTip` holds 128 wide chars including the terminator.
pub const TIP_MAX: usize = 127;
/// Longest profile name shown in the tooltip before it is shortened.
pub const TIP_NAME_MAX: usize = 60;

/// What the applied profile's audio is doing, for the icon.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum EqState {
    /// Not processing (pass-through, not installed, no profile).
    #[default]
    Off,
    Active,
    /// The game took the device in exclusive mode, which bypasses the APO.
    Paused,
}

/// What the icon shows. Derived from [`crate::types::CoreState`] only, so
/// it can never claim more than the state the UI also shows.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TrayStatus {
    /// The applied profile's name, if one is applied.
    pub profile: Option<String>,
    pub eq: EqState,
    pub display: bool,
}

impl TrayStatus {
    pub fn from_state(s: &crate::types::CoreState) -> Self {
        use crate::types::{AudioChainState, DisplayState};
        let Some(p) = s.active_profile.as_ref() else { return Self::default() };
        Self {
            profile: Some(p.name.clone()),
            eq: match s.audio_chain {
                AudioChainState::Active => EqState::Active,
                AudioChainState::ExclusiveBypassed => EqState::Paused,
                _ => EqState::Off,
            },
            display: s.display_state == DisplayState::Applied,
        }
    }

    /// The dot is on only while the profile actually changes something.
    pub fn lit(&self) -> bool {
        self.profile.is_some() && (self.eq == EqState::Active || self.display)
    }

    /// Two lines: the profile, then what is on. Within [`TIP_MAX`].
    pub fn tooltip(&self) -> String {
        let Some(name) = self.profile.as_deref() else { return TOOLTIP.to_string() };
        let name: String = if name.chars().count() > TIP_NAME_MAX {
            name.chars().take(TIP_NAME_MAX - 1).chain(std::iter::once('…')).collect()
        } else {
            name.to_string()
        };
        let what = match (self.eq, self.display) {
            (EqState::Active, true) => "EQ + display active",
            (EqState::Active, false) => "EQ active",
            (EqState::Paused, true) => "Display active · EQ paused (game took exclusive audio)",
            (EqState::Paused, false) => "EQ paused (game took exclusive audio)",
            (EqState::Off, true) => "Display active",
            (EqState::Off, false) => "Profile on · nothing to change",
        };
        format!("Relay · {name}\n{what}")
    }
}

/// Size of the status dot as a fraction of the icon's edge.
pub const DOT_FRACTION: f32 = 0.46;
/// The palette's "ok" green (#9FB98A), as BGRA. Not the champagne accent:
/// the app icon is already a champagne dot, and a second one read as a
/// smudge rather than a badge.
pub const DOT_BGRA: [u8; 4] = [0x8A, 0xB9, 0x9F, 0xFF];
/// Warm black ring (#0E0D0C) so the dot reads on light and dark taskbars.
pub const RING_BGRA: [u8; 4] = [0x0C, 0x0D, 0x0E, 0xFF];

/// Paint the "profile applied" dot into the bottom-right corner of a
/// top-down, straight-alpha BGRA icon image. Pure, so it is tested without
/// a desktop.
pub fn draw_status_dot(px: &mut [u8], w: usize, h: usize) {
    if w == 0 || h == 0 || px.len() < w * h * 4 {
        return;
    }
    let d = (w.min(h) as f32 * DOT_FRACTION).max(4.0);
    let r = d / 2.0;
    let ring = (r * 0.28).max(1.0);
    let (cx, cy) = (w as f32 - r, h as f32 - r);
    for y in 0..h {
        for x in 0..w {
            let (dx, dy) = (x as f32 + 0.5 - cx, y as f32 + 0.5 - cy);
            let dist = (dx * dx + dy * dy).sqrt();
            // Coverage with a one-pixel soft edge.
            let outer = (r - dist + 0.5).clamp(0.0, 1.0);
            if outer <= 0.0 {
                continue;
            }
            let inner = (r - ring - dist + 0.5).clamp(0.0, 1.0);
            let i = (y * w + x) * 4;
            for c in 0..3 {
                let paint = DOT_BGRA[c] as f32 * inner + RING_BGRA[c] as f32 * (1.0 - inner);
                let src = px[i + c] as f32;
                px[i + c] = (paint * outer + src * (1.0 - outer)).round() as u8;
            }
            let a = px[i + 3] as f32 / 255.0;
            px[i + 3] = ((outer + a * (1.0 - outer)) * 255.0).round() as u8;
        }
    }
}

#[cfg(windows)]
pub use imp::Tray;

#[cfg(windows)]
mod imp {
    use super::*;
    use tracing::warn;
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::{HWND, LPARAM, POINT, WPARAM};
    use windows::Win32::Graphics::Gdi::{
        CreateBitmap, DeleteObject, GetDC, GetDIBits, GetObjectW, ReleaseDC, BITMAP, BITMAPINFO,
        BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS,
    };
    use windows::Win32::UI::Shell::{
        ExtractIconW, Shell_NotifyIconW, NIF_ICON, NIF_INFO, NIF_MESSAGE, NIF_TIP, NIIF_INFO,
        NIM_ADD, NIM_DELETE, NIM_MODIFY, NOTIFYICONDATAW,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        AppendMenuW, CreateIconIndirect, CreatePopupMenu, DestroyIcon, DestroyMenu, GetCursorPos,
        GetIconInfo, LoadIconW, PostMessageW, SetForegroundWindow, TrackPopupMenu, HICON, ICONINFO,
        IDI_APPLICATION, MF_CHECKED, MF_GRAYED, MF_SEPARATOR, MF_STRING, TPM_BOTTOMALIGN,
        TPM_RETURNCMD, TPM_RIGHTBUTTON, WM_LBUTTONDBLCLK, WM_LBUTTONUP, WM_NULL, WM_RBUTTONUP,
    };

    /// One icon per process. Removing it on drop matters: an icon left behind
    /// by a dead process sticks in the notification area until the user hovers
    /// it, which looks exactly like the app that would not go away.
    pub struct Tray {
        hwnd: HWND,
        icon: HICON,
        /// The icon with the "profile applied" dot; `None` if it could not
        /// be made (then the tooltip alone carries the state).
        lit_icon: Option<HICON>,
        status: TrayStatus,
    }

    impl Tray {
        /// Add the icon to the notification area.
        ///
        /// # Safety
        /// `hwnd` must be a live window owned by the calling thread; it will
        /// receive [`WM_TRAY`] until [`Tray::remove`] or drop.
        pub unsafe fn add(hwnd: HWND) -> Option<Self> {
            // SAFETY: caller guarantees hwnd; the rest are plain Win32 calls.
            unsafe {
                let icon = app_icon();
                let lit_icon = with_dot(icon);
                let tray = Tray { hwnd, icon, lit_icon, status: TrayStatus::default() };
                if tray.notify(NIM_ADD) {
                    Some(tray)
                } else {
                    warn!("notification-area icon not added; Relay has no tray affordance");
                    None
                }
            }
        }

        /// Re-add the icon after Explorer restarted. Explorer broadcasts
        /// `TaskbarCreated` when it comes back, and every icon that was there
        /// before is gone; without this the tray disappears for the rest of
        /// the session after any Explorer crash.
        ///
        /// # Safety
        /// Same as [`Tray::add`].
        pub unsafe fn readd(&self) {
            // SAFETY: as documented on the method.
            unsafe {
                self.notify(NIM_ADD);
            }
        }

        /// Show what the applied profile is doing: the dot and the tooltip.
        ///
        /// # Safety
        /// Must be called on the thread that owns the window.
        pub unsafe fn set_status(&mut self, status: TrayStatus) {
            if self.status == status {
                return;
            }
            self.status = status;
            // SAFETY: as documented on the method.
            unsafe {
                self.notify(NIM_MODIFY);
            }
        }

        /// Handle one [`WM_TRAY`] message. Returns the chosen command, if any.
        ///
        /// # Safety
        /// Must be called on the thread that owns the window.
        pub unsafe fn on_message(&self, lparam: LPARAM) -> Option<TrayCommand> {
            // The mouse message is in the low word of lParam.
            match (lparam.0 as u32) & 0xFFFF {
                // Left click and double click both open the window: the
                // single click is what people try first, and there is no
                // other left-click meaning to conflict with.
                WM_LBUTTONUP | WM_LBUTTONDBLCLK => Some(TrayCommand::Open),
                // SAFETY: on the owning thread, as documented.
                WM_RBUTTONUP => unsafe { self.popup() },
                _ => None,
            }
        }

        unsafe fn popup(&self) -> Option<TrayCommand> {
            // SAFETY: menu is created, tracked and destroyed within this call.
            unsafe {
                // `AppendMenuW` copies each label, so last popup's buffers are
                // dead the moment that popup closed. Dropping them here keeps
                // the arena at three entries instead of three per right-click.
                WIDE.with(|a| a.borrow_mut().clear());

                let menu = CreatePopupMenu().ok()?;
                let _ = AppendMenuW(menu, MF_STRING, ID_OPEN as usize, w("Open Relay"));
                let listen = listening_menu();
                if listen.len() > 1 {
                    let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
                    let _ = AppendMenuW(menu, MF_STRING | MF_GRAYED, 0, w("Listening on"));
                    for (i, item) in listen.iter().take(MAX_LISTEN).enumerate() {
                        let flags = if item.active { MF_STRING | MF_CHECKED } else { MF_STRING };
                        let _ =
                            AppendMenuW(menu, flags, (ID_LISTEN_BASE as usize) + i, w(&item.label));
                    }
                    let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
                }
                let _ = AppendMenuW(menu, MF_STRING, ID_RESTORE as usize, w("Restore everything"));
                let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
                let _ = AppendMenuW(menu, MF_STRING, ID_QUIT as usize, w("Quit Relay"));

                let mut pt = POINT::default();
                let _ = GetCursorPos(&mut pt);

                // The documented dance: a popup owned by a window that is not
                // in the foreground never receives the click that dismisses
                // it, so it hangs around after the user clicks elsewhere.
                // Foreground first, and a null message afterwards to let the
                // menu close cleanly.
                let _ = SetForegroundWindow(self.hwnd);
                let chosen = TrackPopupMenu(
                    menu,
                    TPM_RETURNCMD | TPM_RIGHTBUTTON | TPM_BOTTOMALIGN,
                    pt.x,
                    pt.y,
                    None,
                    self.hwnd,
                    None,
                );
                let _ = PostMessageW(Some(self.hwnd), WM_NULL, WPARAM(0), LPARAM(0));
                let _ = DestroyMenu(menu);

                TrayCommand::from_id(chosen.0 as u32)
            }
        }

        /// Take the icon out of the notification area.
        ///
        /// # Safety
        /// Must be called on the thread that owns the window.
        pub unsafe fn remove(&self) {
            // SAFETY: as documented on the method.
            unsafe {
                self.notify(NIM_DELETE);
            }
        }

        /// A balloon from the icon (S38): the one way the core can say
        /// something with no window open. Windows shows it as a toast and
        /// keeps it in the notification centre; a second call replaces the
        /// first. Text is cut to the `szInfo` / `szInfoTitle` limits.
        ///
        /// # Safety
        /// Must be called on the thread that owns the window.
        pub unsafe fn balloon(&self, title: &str, text: &str) {
            // SAFETY: `data` is fully initialised and lives across the call.
            unsafe {
                let mut data = NOTIFYICONDATAW {
                    cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
                    hWnd: self.hwnd,
                    uID: 1,
                    uFlags: NIF_INFO,
                    ..Default::default()
                };
                data.Anonymous.uTimeout = 10_000;
                data.dwInfoFlags = NIIF_INFO;
                let put = |dst: &mut [u16], s: &str| {
                    let w: Vec<u16> = s.encode_utf16().collect();
                    let n = w.len().min(dst.len() - 1);
                    dst[..n].copy_from_slice(&w[..n]);
                };
                put(&mut data.szInfoTitle, title);
                put(&mut data.szInfo, text);
                if !Shell_NotifyIconW(NIM_MODIFY, &data).as_bool() {
                    warn!(title, "notification-area balloon was not shown");
                }
            }
        }

        unsafe fn notify(&self, message: windows::Win32::UI::Shell::NOTIFY_ICON_MESSAGE) -> bool {
            // SAFETY: `data` is fully initialised below and lives across the call.
            unsafe {
                let mut data = NOTIFYICONDATAW {
                    cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
                    hWnd: self.hwnd,
                    uID: 1,
                    uFlags: NIF_ICON | NIF_MESSAGE | NIF_TIP,
                    uCallbackMessage: WM_TRAY,
                    hIcon: match self.lit_icon {
                        Some(lit) if self.status.lit() => lit,
                        _ => self.icon,
                    },
                    ..Default::default()
                };
                let tip: Vec<u16> = self.status.tooltip().encode_utf16().collect();
                let n = tip.len().min(data.szTip.len() - 1);
                data.szTip[..n].copy_from_slice(&tip[..n]);
                Shell_NotifyIconW(message, &data).as_bool()
            }
        }
    }

    impl Drop for Tray {
        fn drop(&mut self) {
            // SAFETY: the winloop thread owns both the window and this value,
            // and drops it before destroying the window. The lit icon is ours
            // (CreateIconIndirect), so it is destroyed here.
            unsafe {
                self.remove();
                if let Some(lit) = self.lit_icon.take() {
                    let _ = DestroyIcon(lit);
                }
            }
        }
    }

    /// Relay's own icon, taken from the UI binary beside us.
    ///
    /// The core is a console binary with no icon resource of its own and no
    /// build script; pulling the icon out of `relay-ui.exe` reuses exactly the
    /// artwork the installer already ships, at no cost to the always-on
    /// binary's size. If that is not there (a dev tree that has never built
    /// the UI), the generic application icon is a poor tray icon but a fine
    /// fallback — the menu behind it is what matters.
    pub(super) unsafe fn app_icon() -> HICON {
        // SAFETY: both calls take a path/atom and return a shared icon handle.
        unsafe {
            let from_ui = std::env::current_exe()
                .ok()
                .and_then(|me| crate::launcher::sibling_of(&me, crate::launcher::UI_EXE))
                .filter(|p| p.exists())
                .and_then(|p| {
                    let wide: Vec<u16> =
                        p.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
                    let h = ExtractIconW(None, PCWSTR(wide.as_ptr()), 0);
                    // ExtractIcon returns 1 for "the file has no icons" and
                    // null for failure; neither is usable.
                    (!h.is_invalid() && h.0 as usize != 1).then_some(h)
                });
            match from_ui {
                Some(h) => h,
                None => LoadIconW(None, IDI_APPLICATION).unwrap_or_default(),
            }
        }
    }

    /// A copy of `icon` with [`draw_status_dot`] painted on. Read back as
    /// 32-bit BGRA, painted, and rebuilt with the original mask; `None` on
    /// any failure.
    pub(super) unsafe fn with_dot(icon: HICON) -> Option<HICON> {
        // SAFETY: every handle made here is released before returning; the
        // buffers outlive the calls that read them.
        unsafe {
            let mut info = ICONINFO::default();
            GetIconInfo(icon, &mut info).ok()?;
            let release = |info: &ICONINFO| {
                let _ = DeleteObject(info.hbmColor.into());
                let _ = DeleteObject(info.hbmMask.into());
            };
            if info.hbmColor.is_invalid() {
                release(&info);
                return None; // a monochrome icon: leave it alone
            }
            let mut bm = BITMAP::default();
            if GetObjectW(
                info.hbmColor.into(),
                std::mem::size_of::<BITMAP>() as i32,
                Some(&mut bm as *mut _ as *mut _),
            ) == 0
            {
                release(&info);
                return None;
            }
            let (w, h) = (bm.bmWidth.max(0) as usize, bm.bmHeight.max(0) as usize);
            let mut bmi = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: w as i32,
                    biHeight: -(h as i32), // top-down
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB.0,
                    ..Default::default()
                },
                ..Default::default()
            };
            let mut px = vec![0u8; w * h * 4];
            let dc = GetDC(None);
            let rows = GetDIBits(
                dc,
                info.hbmColor,
                0,
                h as u32,
                Some(px.as_mut_ptr() as *mut _),
                &mut bmi,
                DIB_RGB_COLORS,
            );
            ReleaseDC(None, dc);
            if rows != h as i32 {
                release(&info);
                return None;
            }
            // An icon without per-pixel alpha is opaque wherever it draws.
            if px.chunks_exact(4).all(|p| p[3] == 0) {
                px.chunks_exact_mut(4).for_each(|p| p[3] = 0xFF);
            }
            draw_status_dot(&mut px, w, h);
            let color = CreateBitmap(w as i32, h as i32, 1, 32, Some(px.as_ptr() as *const _));
            let lit = ICONINFO {
                fIcon: true.into(),
                xHotspot: 0,
                yHotspot: 0,
                hbmMask: info.hbmMask,
                hbmColor: color,
            };
            let made = CreateIconIndirect(&lit).ok();
            let _ = DeleteObject(color.into());
            release(&info);
            made
        }
    }

    use std::os::windows::ffi::OsStrExt;

    /// A NUL-terminated wide string that stays alive across the `AppendMenuW`
    /// call that reads it.
    ///
    /// A plain temporary would dangle: `PCWSTR` borrows nothing, so the `Vec`
    /// would be dropped at the end of the argument expression. The buffers go
    /// into a thread-local arena that [`Tray::popup`] clears each time it
    /// builds a menu.
    fn w(s: &str) -> PCWSTR {
        WIDE.with(|arena| {
            let mut a = arena.borrow_mut();
            let buf: Vec<u16> = s.encode_utf16().chain(std::iter::once(0)).collect();
            a.push(buf);
            PCWSTR(a.last().unwrap().as_ptr())
        })
    }

    thread_local! {
        /// Wide buffers backing the menu labels. Only the winloop thread ever
        /// touches it, and it holds at most the handful of menu strings.
        static WIDE: std::cell::RefCell<Vec<Vec<u16>>> = const { std::cell::RefCell::new(Vec::new()) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn menu_ids_map_to_commands_and_zero_means_dismissed() {
        assert_eq!(TrayCommand::from_id(ID_OPEN), Some(TrayCommand::Open));
        assert_eq!(TrayCommand::from_id(ID_RESTORE), Some(TrayCommand::Restore));
        assert_eq!(TrayCommand::from_id(ID_QUIT), Some(TrayCommand::Quit));
        // TrackPopupMenu reports 0 when the menu is dismissed without a pick.
        assert_eq!(TrayCommand::from_id(0), None);
        assert_eq!(TrayCommand::from_id(99), None);
        assert_eq!(TrayCommand::from_id(ID_LISTEN_BASE), Some(TrayCommand::Listen(0)));
        assert_eq!(TrayCommand::from_id(ID_LISTEN_BASE + 2), Some(TrayCommand::Listen(2)));
        assert_eq!(TrayCommand::from_id(ID_LISTEN_BASE + MAX_LISTEN as u32), None);
    }

    #[test]
    fn tooltip_fits_the_tip_field() {
        // NOTIFYICONDATAW::szTip is 128 wide chars including the terminator.
        assert!(TOOLTIP.encode_utf16().count() < 128);
    }

    fn status(eq: EqState, display: bool) -> TrayStatus {
        TrayStatus { profile: Some("Call of Duty MW4".into()), eq, display }
    }

    #[test]
    fn the_tooltip_says_which_profile_and_what_is_on() {
        assert_eq!(TrayStatus::default().tooltip(), TOOLTIP);
        assert_eq!(
            status(EqState::Active, true).tooltip(),
            "Relay · Call of Duty MW4\nEQ + display active"
        );
        assert_eq!(status(EqState::Active, false).tooltip(), "Relay · Call of Duty MW4\nEQ active");
        assert_eq!(
            status(EqState::Off, true).tooltip(),
            "Relay · Call of Duty MW4\nDisplay active"
        );
        assert!(status(EqState::Paused, false).tooltip().contains("EQ paused"));
        assert!(status(EqState::Off, false).tooltip().ends_with("nothing to change"));
    }

    #[test]
    fn the_dot_is_on_only_while_something_changes() {
        assert!(!TrayStatus::default().lit());
        assert!(status(EqState::Active, false).lit());
        assert!(status(EqState::Off, true).lit());
        assert!(!status(EqState::Off, false).lit());
        assert!(!status(EqState::Paused, false).lit());
    }

    #[test]
    fn every_tooltip_fits_even_with_a_long_name() {
        let long =
            "Call of Duty®: Modern Warfare® IV — Multiplayer — Ranked Play — Season 4 Reloaded";
        for eq in [EqState::Off, EqState::Active, EqState::Paused] {
            for display in [false, true] {
                let t = TrayStatus { profile: Some(long.into()), eq, display }.tooltip();
                assert!(t.encode_utf16().count() <= TIP_MAX, "{t}");
                assert!(t.contains('…'));
            }
        }
    }

    #[test]
    fn status_comes_from_the_core_state() {
        use crate::types::{AudioChainState, CoreState, DisplayState};
        let mut s = CoreState::default();
        assert_eq!(TrayStatus::from_state(&s), TrayStatus::default());
        s.active_profile = Some(
            crate::types::Profile::new("MW4", crate::types::GameMatch::exe("cod.exe")).summary(),
        );
        s.audio_chain = AudioChainState::Active;
        s.display_state = DisplayState::Applied;
        let t = TrayStatus::from_state(&s);
        assert_eq!((t.profile.as_deref(), t.eq, t.display), (Some("MW4"), EqState::Active, true));
        s.audio_chain = AudioChainState::ExclusiveBypassed;
        assert_eq!(TrayStatus::from_state(&s).eq, EqState::Paused);
        s.audio_chain = AudioChainState::Bypass;
        assert_eq!(TrayStatus::from_state(&s).eq, EqState::Off);
    }

    #[test]
    fn the_dot_sits_bottom_right_and_leaves_the_rest() {
        let (w, h) = (32usize, 32usize);
        let mut px = vec![0u8; w * h * 4];
        draw_status_dot(&mut px, w, h);
        let at = |x: usize, y: usize| &px[(y * w + x) * 4..(y * w + x) * 4 + 4];
        assert_eq!(at(2, 2), &[0, 0, 0, 0], "top-left untouched");
        assert_eq!(at(w / 2, 2), &[0, 0, 0, 0]);
        let r = (w as f32 * DOT_FRACTION / 2.0) as usize;
        assert_eq!(at(w - r, h - r), &DOT_BGRA, "centre is the accent");
        assert_eq!(at(w - r, h - 2)[3], 0xFF, "ring is opaque");
        assert_eq!(&at(w - r, h - 2)[..3], &RING_BGRA[..3], "with the dark ring colour");
        // Too-small buffers are ignored, not indexed out of bounds.
        draw_status_dot(&mut [0u8; 8], 32, 32);
    }

    /// The GDI path end to end on a real icon (the test binary has no
    /// relay-ui.exe beside it, so this is Windows' generic application icon).
    #[cfg(windows)]
    #[test]
    fn the_lit_icon_is_made_from_the_real_one() {
        // SAFETY: icon handles made and destroyed on this thread.
        unsafe {
            let base = super::imp::app_icon();
            let lit = super::imp::with_dot(base).expect("lit icon");
            assert!(!lit.is_invalid());
            let _ = windows::Win32::UI::WindowsAndMessaging::DestroyIcon(lit);
        }
    }

    #[test]
    fn tooltip_names_no_command() {
        assert!(!TOOLTIP.contains("relay-core"));
    }
}
