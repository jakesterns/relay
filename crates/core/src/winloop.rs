//! One dedicated Win32 thread: foreground-change hook + global hotkeys.
//!
//! Uses `SetWinEventHook(EVENT_SYSTEM_FOREGROUND)` out-of-context, which is a
//! plain OS notification — no DLL is loaded into any other process, nothing is
//! hooked inside the game. Sits idle in `GetMessageW` between events, so it
//! costs no CPU at rest.

use std::path::PathBuf;

use anyhow::Result;
use tokio::sync::mpsc::UnboundedSender;

use crate::hotkeys::{Hotkey, HotkeyAction};
use crate::types::Foreground;

#[derive(Debug, Clone)]
pub enum CoreEvent {
    ForegroundChanged(Foreground),
    Hotkey(HotkeyAction),
    /// Console Ctrl-C / close / logoff / shutdown.
    Shutdown,
}

/// Handle to the loop thread. Dropping it asks the loop to quit and joins it.
pub struct WinLoop {
    #[cfg(windows)]
    thread_id: u32,
    join: Option<std::thread::JoinHandle<()>>,
}

impl WinLoop {
    /// Start the loop thread. It emits the current foreground window immediately
    /// so the service starts from a known state.
    pub fn spawn(tx: UnboundedSender<CoreEvent>, hotkeys: Vec<Hotkey>) -> Result<Self> {
        #[cfg(windows)]
        {
            imp::spawn(tx, hotkeys)
        }
        #[cfg(not(windows))]
        {
            let _ = (tx, hotkeys);
            anyhow::bail!("the Relay core only runs on Windows")
        }
    }

    pub fn stop(mut self) {
        self.request_stop();
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }

    fn request_stop(&self) {
        #[cfg(windows)]
        imp::post_quit(self.thread_id);
    }
}

impl Drop for WinLoop {
    fn drop(&mut self) {
        self.request_stop();
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}

/// `C:\Games\CoD\cod.exe` → `cod.exe`
pub fn exe_name(path: &str) -> String {
    PathBuf::from(path).file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default()
}

#[cfg(windows)]
mod imp {
    use super::*;
    use parking_lot::Mutex;
    use tracing::{debug, warn};
    use windows::core::BOOL;
    use windows::core::PWSTR;
    use windows::Win32::Foundation::{CloseHandle, HANDLE, HWND, LPARAM, WPARAM};
    use windows::Win32::System::Console::{
        SetConsoleCtrlHandler, CTRL_BREAK_EVENT, CTRL_CLOSE_EVENT, CTRL_C_EVENT, CTRL_LOGOFF_EVENT,
        CTRL_SHUTDOWN_EVENT,
    };
    use windows::Win32::System::Threading::{
        GetCurrentThreadId, OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32,
        PROCESS_QUERY_LIMITED_INFORMATION,
    };
    use windows::Win32::UI::Accessibility::{SetWinEventHook, UnhookWinEvent, HWINEVENTHOOK};
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        RegisterHotKey, UnregisterHotKey, HOT_KEY_MODIFIERS, MOD_ALT, MOD_CONTROL, MOD_NOREPEAT,
        MOD_SHIFT, MOD_WIN,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        DispatchMessageW, GetForegroundWindow, GetMessageW, GetWindowTextW,
        GetWindowThreadProcessId, PostThreadMessageW, TranslateMessage, EVENT_SYSTEM_FOREGROUND,
        MSG, WINEVENT_OUTOFCONTEXT, WINEVENT_SKIPOWNPROCESS, WM_HOTKEY, WM_QUIT,
    };

    /// The hook callback is a bare `extern "system"` fn and cannot capture, so the
    /// sender lives in a process-wide slot. One loop per process is the design.
    static SENDER: Mutex<Option<UnboundedSender<CoreEvent>>> = Mutex::new(None);

    pub fn spawn(tx: UnboundedSender<CoreEvent>, hotkeys: Vec<Hotkey>) -> Result<WinLoop> {
        *SENDER.lock() = Some(tx);
        let (id_tx, id_rx) = std::sync::mpsc::channel::<u32>();

        let join = std::thread::Builder::new().name("relay-winloop".into()).spawn(move || {
            // SAFETY: plain Win32 calls on our own thread; handles are unhooked below.
            unsafe {
                let _ = id_tx.send(GetCurrentThreadId());

                let _ = SetConsoleCtrlHandler(Some(ctrl_handler), true);

                let hook = SetWinEventHook(
                    EVENT_SYSTEM_FOREGROUND,
                    EVENT_SYSTEM_FOREGROUND,
                    None,
                    Some(win_event_proc),
                    0,
                    0,
                    WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS,
                );
                if hook.is_invalid() {
                    warn!("SetWinEventHook failed; focus tracking disabled");
                }

                let mut registered = Vec::new();
                for hk in &hotkeys {
                    match RegisterHotKey(None, hk.action.id(), modifiers(hk), hk.vk) {
                        Ok(()) => registered.push(hk.action.id()),
                        Err(e) => warn!(action = ?hk.action, error = %e, "hotkey not registered"),
                    }
                }

                // Seed with whatever is in front right now.
                if let Some(fg) = describe(GetForegroundWindow()) {
                    emit(CoreEvent::ForegroundChanged(fg));
                }

                let mut msg = MSG::default();
                while GetMessageW(&mut msg, None, 0, 0).as_bool() {
                    if msg.message == WM_HOTKEY {
                        if let Some(action) = HotkeyAction::from_id(msg.wParam.0 as i32) {
                            emit(CoreEvent::Hotkey(action));
                        }
                        continue;
                    }
                    let _ = TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }

                for id in registered {
                    let _ = UnregisterHotKey(None, id);
                }
                if !hook.is_invalid() {
                    let _ = UnhookWinEvent(hook);
                }
                let _ = SetConsoleCtrlHandler(Some(ctrl_handler), false);
            }
            *SENDER.lock() = None;
        })?;

        let thread_id = id_rx.recv()?;
        Ok(WinLoop { thread_id, join: Some(join) })
    }

    pub fn post_quit(thread_id: u32) {
        // SAFETY: posting WM_QUIT to our own loop thread.
        unsafe {
            let _ = PostThreadMessageW(thread_id, WM_QUIT, WPARAM(0), LPARAM(0));
        }
    }

    fn emit(ev: CoreEvent) {
        if let Some(tx) = SENDER.lock().as_ref() {
            let _ = tx.send(ev);
        }
    }

    fn modifiers(hk: &Hotkey) -> HOT_KEY_MODIFIERS {
        let mut m = MOD_NOREPEAT;
        if hk.modifiers.ctrl {
            m |= MOD_CONTROL;
        }
        if hk.modifiers.alt {
            m |= MOD_ALT;
        }
        if hk.modifiers.shift {
            m |= MOD_SHIFT;
        }
        if hk.modifiers.win {
            m |= MOD_WIN;
        }
        m
    }

    unsafe extern "system" fn win_event_proc(
        _hook: HWINEVENTHOOK,
        event: u32,
        hwnd: HWND,
        _id_object: i32,
        _id_child: i32,
        _thread: u32,
        _time: u32,
    ) {
        if event != EVENT_SYSTEM_FOREGROUND {
            return;
        }
        if let Some(fg) = describe(hwnd) {
            debug!(exe = %fg.exe, pid = fg.pid, "foreground changed");
            emit(CoreEvent::ForegroundChanged(fg));
        }
    }

    unsafe extern "system" fn ctrl_handler(kind: u32) -> BOOL {
        match kind {
            CTRL_C_EVENT | CTRL_BREAK_EVENT | CTRL_CLOSE_EVENT | CTRL_LOGOFF_EVENT
            | CTRL_SHUTDOWN_EVENT => {
                emit(CoreEvent::Shutdown);
                // Returning TRUE tells Windows we handled it; the service loop
                // restores state and exits on its own.
                BOOL(1)
            }
            _ => BOOL(0),
        }
    }

    /// Read pid, image path and title for a window. Only limited-information
    /// process access is requested; never memory or handles.
    unsafe fn describe(hwnd: HWND) -> Option<Foreground> {
        if hwnd.is_invalid() {
            return None;
        }
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        if pid == 0 {
            return None;
        }

        let exe = process_image(pid).map(|p| exe_name(&p)).unwrap_or_default();

        let mut title_buf = [0u16; 512];
        let n = GetWindowTextW(hwnd, &mut title_buf) as usize;
        let title = String::from_utf16_lossy(&title_buf[..n]);

        Some(Foreground { pid, exe, title })
    }

    unsafe fn process_image(pid: u32) -> Option<String> {
        let h: HANDLE = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buf = [0u16; 1024];
        let mut len = buf.len() as u32;
        let r =
            QueryFullProcessImageNameW(h, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len);
        let _ = CloseHandle(h);
        r.ok()?;
        Some(String::from_utf16_lossy(&buf[..len as usize]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exe_name_strips_directories() {
        assert_eq!(exe_name(r"C:\Games\CoD\cod.exe"), "cod.exe");
        assert_eq!(exe_name("cod.exe"), "cod.exe");
        assert_eq!(exe_name(""), "");
    }
}
