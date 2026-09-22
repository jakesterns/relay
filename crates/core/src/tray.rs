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

/// Sent to the winloop window when the icon is clicked. `WM_APP + 1`; the
/// window class is ours alone, so no other message can collide with it.
#[cfg(windows)]
pub const WM_TRAY: u32 = windows::Win32::UI::WindowsAndMessaging::WM_APP + 1;

/// Menu command ids. Non-zero because `TPM_RETURNCMD` reports 0 for "the menu
/// was dismissed without choosing anything".
pub const ID_OPEN: u32 = 1;
pub const ID_RESTORE: u32 = 2;
pub const ID_QUIT: u32 = 3;

/// What the user picked from the icon.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayCommand {
    /// Show the Relay window: focus the running one, or start it.
    Open,
    /// Put audio and display back the way Windows had them.
    Restore,
    /// Stop the core (restoring on the way out) and close the window.
    Quit,
}

impl TrayCommand {
    /// Menu id → command. `None` for 0 (dismissed) and anything unknown.
    pub fn from_id(id: u32) -> Option<Self> {
        match id {
            ID_OPEN => Some(Self::Open),
            ID_RESTORE => Some(Self::Restore),
            ID_QUIT => Some(Self::Quit),
            _ => None,
        }
    }
}

/// Tooltip text, which is the only place the background story is told outside
/// the app window. Kept to the 127-char `szTip` limit.
pub const TOOLTIP: &str = "Relay is running in the background";

#[cfg(windows)]
pub use imp::Tray;

#[cfg(windows)]
mod imp {
    use super::*;
    use tracing::warn;
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::{HWND, LPARAM, POINT, WPARAM};
    use windows::Win32::UI::Shell::{
        ExtractIconW, Shell_NotifyIconW, NIF_ICON, NIF_INFO, NIF_MESSAGE, NIF_TIP, NIIF_INFO,
        NIM_ADD, NIM_DELETE, NIM_MODIFY, NOTIFYICONDATAW,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        AppendMenuW, CreatePopupMenu, DestroyMenu, GetCursorPos, LoadIconW, PostMessageW,
        SetForegroundWindow, TrackPopupMenu, HICON, IDI_APPLICATION, MF_SEPARATOR, MF_STRING,
        TPM_BOTTOMALIGN, TPM_RETURNCMD, TPM_RIGHTBUTTON, WM_LBUTTONDBLCLK, WM_LBUTTONUP, WM_NULL,
        WM_RBUTTONUP,
    };

    /// One icon per process. Removing it on drop matters: an icon left behind
    /// by a dead process sticks in the notification area until the user hovers
    /// it, which looks exactly like the app that would not go away.
    pub struct Tray {
        hwnd: HWND,
        icon: HICON,
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
                let tray = Tray { hwnd, icon };
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
                    hIcon: self.icon,
                    ..Default::default()
                };
                let tip: Vec<u16> = TOOLTIP.encode_utf16().collect();
                let n = tip.len().min(data.szTip.len() - 1);
                data.szTip[..n].copy_from_slice(&tip[..n]);
                Shell_NotifyIconW(message, &data).as_bool()
            }
        }
    }

    impl Drop for Tray {
        fn drop(&mut self) {
            // SAFETY: the winloop thread owns both the window and this value,
            // and drops it before destroying the window.
            unsafe { self.remove() }
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
    unsafe fn app_icon() -> HICON {
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
    }

    #[test]
    fn tooltip_fits_the_tip_field() {
        // NOTIFYICONDATAW::szTip is 128 wide chars including the terminator.
        assert!(TOOLTIP.encode_utf16().count() < 128);
    }

    #[test]
    fn tooltip_names_no_command() {
        assert!(!TOOLTIP.contains("relay-core"));
    }
}
