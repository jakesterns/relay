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
    /// A monitor or audio endpoint came, went, or changed role: re-probe and
    /// re-select. Sent by the winloop window (`WM_DISPLAYCHANGE` /
    /// `WM_DEVICECHANGE`) and by the WASAPI notification client.
    HardwareChanged,
    /// The interactive session locked (`true`) or unlocked (`false`).
    /// Display profiles restore on lock and re-apply on unlock.
    SessionLock(bool),
    /// Console Ctrl-C / close / logoff / shutdown.
    Shutdown,
    /// The user picked something from the notification-area icon.
    Tray(crate::tray::TrayCommand),
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

/// Show a notification-area balloon from the tray icon (S38): the one way the
/// core can say something with no window open. Safe from any thread — the
/// text is handed to the loop thread, which owns the icon. A no-op before the
/// loop is up or where there is no tray.
pub fn balloon(title: &str, text: &str) {
    #[cfg(windows)]
    imp::balloon(title, text);
    #[cfg(not(windows))]
    let _ = (title, text);
}

impl Drop for WinLoop {
    fn drop(&mut self) {
        self.request_stop();
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}

/// Full image path of a process, or `None` if it cannot be queried.
#[cfg(windows)]
/// Whether a window still exists. A plain user32 query on the window
/// handle: nothing is opened in the owning process.
pub fn window_alive(hwnd: u64) -> bool {
    #[cfg(windows)]
    {
        use windows::Win32::Foundation::HWND;
        use windows::Win32::UI::WindowsAndMessaging::IsWindow;
        // SAFETY: IsWindow accepts any value, including stale handles.
        unsafe { IsWindow(Some(HWND(hwnd as *mut _))).as_bool() }
    }
    #[cfg(not(windows))]
    {
        let _ = hwnd;
        true
    }
}

/// The window Windows says is in front (0 = none). One user32 call; nothing
/// is opened in the owning process.
pub fn foreground_hwnd() -> u64 {
    #[cfg(windows)]
    {
        use windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow;
        // SAFETY: plain query.
        unsafe { GetForegroundWindow().0 as u64 }
    }
    #[cfg(not(windows))]
    {
        0
    }
}

/// A window's current title, from win32k's cached text (no message is sent
/// to the window and nothing is opened in its process).
pub fn window_title(hwnd: u64) -> Option<String> {
    #[cfg(windows)]
    {
        use windows::Win32::Foundation::HWND;
        use windows::Win32::UI::WindowsAndMessaging::InternalGetWindowText;
        let mut buf = [0u16; 512];
        // SAFETY: valid buffer; a stale handle just returns 0.
        let n = unsafe { InternalGetWindowText(HWND(hwnd as *mut _), &mut buf) } as usize;
        (n > 0).then(|| String::from_utf16_lossy(&buf[..n]))
    }
    #[cfg(not(windows))]
    {
        let _ = hwnd;
        None
    }
}

pub fn process_image_path(pid: u32) -> Option<String> {
    // SAFETY: only limited-information access is requested; see `imp::process_image`.
    unsafe { imp::process_image(pid) }
}

/// The current foreground window, described the same way the hook events are.
/// Used by the service's slow tick to notice a game moved monitors through a
/// path no hook covers (e.g. Win+Shift+Arrow).
#[cfg(windows)]
pub fn current_foreground() -> Option<Foreground> {
    // SAFETY: read-only queries on the current foreground window.
    unsafe { imp::describe(windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow()) }
}

/// `C:\Games\CoD\cod.exe` → `cod.exe`
pub fn exe_name(path: &str) -> String {
    PathBuf::from(path).file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default()
}

#[cfg(windows)]
mod imp {
    use super::*;
    use parking_lot::Mutex;
    use std::sync::atomic::{AtomicU32, Ordering};
    use tracing::{debug, warn};
    use windows::core::w;
    use windows::core::BOOL;
    use windows::core::PWSTR;
    use windows::Win32::Foundation::{CloseHandle, HANDLE, HWND, LPARAM, WPARAM};
    use windows::Win32::Graphics::Gdi::{MonitorFromWindow, MONITOR_DEFAULTTONEAREST};
    use windows::Win32::System::Console::{
        SetConsoleCtrlHandler, CTRL_BREAK_EVENT, CTRL_CLOSE_EVENT, CTRL_C_EVENT, CTRL_LOGOFF_EVENT,
        CTRL_SHUTDOWN_EVENT,
    };
    use windows::Win32::System::RemoteDesktop::{
        WTSRegisterSessionNotification, WTSUnRegisterSessionNotification, NOTIFY_FOR_THIS_SESSION,
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
        CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetForegroundWindow,
        GetMessageW, GetWindowTextW, GetWindowThreadProcessId, PostThreadMessageW, RegisterClassW,
        RegisterWindowMessageW, TranslateMessage, EVENT_SYSTEM_FOREGROUND,
        EVENT_SYSTEM_MOVESIZEEND, MSG, WINDOW_EX_STYLE, WINEVENT_OUTOFCONTEXT,
        WINEVENT_SKIPOWNPROCESS, WM_DEVICECHANGE, WM_DISPLAYCHANGE, WM_HOTKEY, WM_QUIT, WNDCLASSW,
        WS_OVERLAPPED,
    };

    /// `WM_WTSSESSION_CHANGE` and its lock/unlock reasons (wtsapi32.h; the
    /// message is not surfaced by the crate's messaging module).
    const WM_WTSSESSION_CHANGE: u32 = 0x02B1;
    const WTS_SESSION_LOCK: usize = 0x7;
    const WTS_SESSION_UNLOCK: usize = 0x8;

    /// The hook callback is a bare `extern "system"` fn and cannot capture, so the
    /// sender lives in a process-wide slot. One loop per process is the design.
    static SENDER: Mutex<Option<UnboundedSender<CoreEvent>>> = Mutex::new(None);

    thread_local! {
        /// The notification-area icon. Thread-local rather than static
        /// because `Tray` may only be touched from the thread that owns the
        /// window — which is this thread, and is also where the window
        /// procedure runs.
        static TRAY: std::cell::RefCell<Option<crate::tray::Tray>> =
            const { std::cell::RefCell::new(None) };
    }

    /// `RegisterWindowMessageW("TaskbarCreated")`. Registered message ids are
    /// process-wide and never 0, so 0 doubles as "not registered yet".
    static TASKBAR_CREATED: AtomicU32 = AtomicU32::new(0);

    /// The loop thread's id, for posting it work from other threads; 0 until
    /// the loop is up.
    static LOOP_THREAD: AtomicU32 = AtomicU32::new(0);

    /// A balloon waiting to be shown by the loop thread (S38). One slot: a
    /// second request before the first is shown replaces it, which is also
    /// what Windows does with the balloons themselves.
    static BALLOON: Mutex<Option<(String, String)>> = Mutex::new(None);

    /// Thread message: "show the balloon in `BALLOON`". `WM_APP + 2`;
    /// `WM_APP + 1` is the tray's callback.
    const WM_BALLOON: u32 = windows::Win32::UI::WindowsAndMessaging::WM_APP + 2;

    pub fn balloon(title: &str, text: &str) {
        let thread = LOOP_THREAD.load(Ordering::Relaxed);
        if thread == 0 {
            debug!(title, "balloon requested before the loop is up; dropped");
            return;
        }
        *BALLOON.lock() = Some((title.to_string(), text.to_string()));
        // SAFETY: posting a message to our own loop thread; no pointers cross.
        unsafe {
            let _ = PostThreadMessageW(thread, WM_BALLOON, WPARAM(0), LPARAM(0));
        }
    }

    /// Tiny wrapper so the registration site reads as a plain assignment.
    trait SetOnce {
        fn set(&self, v: u32);
        fn matches(&self, v: u32) -> bool;
    }
    impl SetOnce for AtomicU32 {
        fn set(&self, v: u32) {
            self.store(v, Ordering::Relaxed);
        }
        fn matches(&self, v: u32) -> bool {
            v != 0 && self.load(Ordering::Relaxed) == v
        }
    }

    pub fn spawn(tx: UnboundedSender<CoreEvent>, hotkeys: Vec<Hotkey>) -> Result<WinLoop> {
        *SENDER.lock() = Some(tx);
        let (id_tx, id_rx) = std::sync::mpsc::channel::<u32>();

        let join = std::thread::Builder::new().name("relay-winloop".into()).spawn(move || {
            // SAFETY: plain Win32 calls on our own thread; handles are unhooked below.
            unsafe {
                LOOP_THREAD.store(GetCurrentThreadId(), Ordering::Relaxed);
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

                // Drag-end events, to notice the game window landing on a
                // different monitor. Rare events; costs nothing at rest.
                let move_hook = SetWinEventHook(
                    EVENT_SYSTEM_MOVESIZEEND,
                    EVENT_SYSTEM_MOVESIZEEND,
                    None,
                    Some(win_event_proc),
                    0,
                    0,
                    WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS,
                );

                let mut registered = Vec::new();
                for hk in &hotkeys {
                    match RegisterHotKey(None, hk.action.id(), modifiers(hk), hk.vk) {
                        Ok(()) => registered.push(hk.action.id()),
                        Err(e) => warn!(action = ?hk.action, error = %e, "hotkey not registered"),
                    }
                }

                // Hidden top-level window: the only way to receive the
                // WM_DISPLAYCHANGE / WM_DEVICECHANGE broadcasts (thread
                // message loops and message-only windows do not get them).
                // It also owns the notification-area icon, so the tray lives
                // exactly as long as the core does.
                let hw_window = create_hardware_window();
                if let Some(w) = hw_window {
                    // Lock/unlock notifications for restore-on-lock.
                    if WTSRegisterSessionNotification(w, NOTIFY_FOR_THIS_SESSION).is_err() {
                        warn!("session notifications unavailable; no restore-on-lock");
                    }
                    // Explorer's "I just restarted, re-add your icons"
                    // broadcast. Registered once, before the icon goes up.
                    TASKBAR_CREATED.set(RegisterWindowMessageW(w!("TaskbarCreated")));
                    TRAY.with(|t| *t.borrow_mut() = crate::tray::Tray::add(w));
                }

                // Seed with whatever is in front right now.
                if let Some(fg) = describe(GetForegroundWindow()) {
                    emit(CoreEvent::ForegroundChanged(fg));
                }

                let mut msg = MSG::default();
                while GetMessageW(&mut msg, None, 0, 0).as_bool() {
                    if msg.message == WM_BALLOON {
                        // A thread message, not a window one, so it is handled
                        // here rather than in the window procedure.
                        if let Some((title, text)) = BALLOON.lock().take() {
                            // Inside the loop's `unsafe` block: this thread
                            // owns the window and the icon.
                            TRAY.with(|t| {
                                if let Some(tray) = t.borrow().as_ref() {
                                    tray.balloon(&title, &text);
                                }
                            });
                        }
                        continue;
                    }
                    if msg.message == WM_HOTKEY {
                        if let Some(action) = HotkeyAction::from_id(msg.wParam.0 as i32) {
                            emit(CoreEvent::Hotkey(action));
                        }
                        continue;
                    }
                    let _ = TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }

                // Drop the icon before the window it hangs off goes away, or
                // it lingers in the notification area as a dead entry.
                LOOP_THREAD.store(0, Ordering::Relaxed);
                TRAY.with(|t| t.borrow_mut().take());
                if let Some(w) = hw_window {
                    let _ = WTSUnRegisterSessionNotification(w);
                    let _ = DestroyWindow(w);
                }
                for id in registered {
                    let _ = UnregisterHotKey(None, id);
                }
                if !hook.is_invalid() {
                    let _ = UnhookWinEvent(hook);
                }
                if !move_hook.is_invalid() {
                    let _ = UnhookWinEvent(move_hook);
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

    /// Never-shown window whose only job is to catch hardware broadcasts.
    unsafe fn create_hardware_window() -> Option<HWND> {
        unsafe {
            let class_name = windows::core::w!("RelayCoreHardware");
            let hinstance =
                windows::Win32::System::LibraryLoader::GetModuleHandleW(None).unwrap_or_default();
            let class = WNDCLASSW {
                lpfnWndProc: Some(hardware_wnd_proc),
                lpszClassName: class_name,
                hInstance: hinstance.into(),
                ..Default::default()
            };
            if RegisterClassW(&class) == 0 {
                warn!("hardware window class not registered; device-change events disabled");
                return None;
            }
            match CreateWindowExW(
                WINDOW_EX_STYLE(0),
                class_name,
                class_name,
                WS_OVERLAPPED,
                0,
                0,
                0,
                0,
                None,
                None,
                None,
                None,
            ) {
                Ok(w) => Some(w),
                Err(e) => {
                    warn!(error = %e, "hardware window not created");
                    None
                }
            }
        }
    }

    unsafe extern "system" fn hardware_wnd_proc(
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> windows::Win32::Foundation::LRESULT {
        match msg {
            // Coalescing happens in the service; here every signal counts.
            WM_DISPLAYCHANGE => emit(CoreEvent::HardwareChanged),
            WM_WTSSESSION_CHANGE => match wparam.0 {
                WTS_SESSION_LOCK => emit(CoreEvent::SessionLock(true)),
                WTS_SESSION_UNLOCK => emit(CoreEvent::SessionLock(false)),
                _ => {}
            },
            WM_DEVICECHANGE => {
                // 0x0007 = DBT_DEVNODES_CHANGED (the catch-all broadcast),
                // 0x8000/0x8004 = DBT_DEVICEARRIVAL / REMOVECOMPLETE.
                if matches!(wparam.0, 0x0007 | 0x8000 | 0x8004) {
                    emit(CoreEvent::HardwareChanged);
                }
            }
            // A click on the notification-area icon. Tracking the menu blocks
            // inside this call, which is fine: the window exists only for
            // these broadcasts and the service runs on another thread.
            crate::tray::WM_TRAY => {
                // SAFETY: the window procedure runs on the thread that owns
                // both the window and the `Tray`.
                let cmd = TRAY.with(|t| unsafe {
                    t.borrow().as_ref().and_then(|tray| tray.on_message(lparam))
                });
                if let Some(cmd) = cmd {
                    emit(CoreEvent::Tray(cmd));
                }
            }
            other if TASKBAR_CREATED.matches(other) => {
                // Explorer restarted and dropped every icon. Put ours back,
                // or Relay goes invisible for the rest of the session.
                // SAFETY: owning thread, as above.
                TRAY.with(|t| unsafe {
                    if let Some(tray) = t.borrow().as_ref() {
                        tray.readd();
                    }
                });
            }
            _ => {}
        }
        // SAFETY: default handling for everything else.
        unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
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
        let relevant = match event {
            EVENT_SYSTEM_FOREGROUND => true,
            // A drag ended: only interesting for the window that has focus
            // (its monitor may have changed).
            // SAFETY: plain query.
            EVENT_SYSTEM_MOVESIZEEND => (unsafe { GetForegroundWindow() }) == hwnd,
            _ => false,
        };
        if !relevant {
            return;
        }
        if let Some(fg) = unsafe { describe(hwnd) } {
            debug!(exe = %fg.exe, pid = fg.pid, hmonitor = fg.hmonitor, "foreground changed");
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

    /// Read pid, image path, title and hosting monitor for a window. Only
    /// limited-information process access is requested; never memory or handles.
    pub(super) unsafe fn describe(hwnd: HWND) -> Option<Foreground> {
        if hwnd.is_invalid() {
            return None;
        }
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        if pid == 0 {
            return None;
        }

        let image = process_image(pid).unwrap_or_default();
        let exe = exe_name(&image);

        let mut title_buf = [0u16; 512];
        let n = GetWindowTextW(hwnd, &mut title_buf) as usize;
        let title = String::from_utf16_lossy(&title_buf[..n]);

        let hmonitor = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST).0 as i64;

        Some(Foreground { pid, exe, title, hmonitor, hwnd: hwnd.0 as u64, image })
    }

    pub(super) unsafe fn process_image(pid: u32) -> Option<String> {
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
