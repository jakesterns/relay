//! The receiver's window thread and its hosting modes (S29).
//!
//! Three modes:
//!
//! - **Standalone** — an ordinary top-level window, the pre-S29 behaviour,
//!   used when `relay-share recv` runs from a console. Esc or the close
//!   button ends the receive.
//! - **Embedded** — a frameless `WS_POPUP` *owned by* the app window,
//!   `WS_EX_NOACTIVATE` so it never takes the keyboard, hidden until the app
//!   positions it over its video area. Owned windows stay above their owner,
//!   minimise and hide with it, and have no taskbar button, which is exactly
//!   the set of behaviours a piece of the app window should have.
//! - **PoppedOut** — an ordinary top-level window again, unowned so the app
//!   can be brought in front of it, sized to the work area of the monitor it
//!   came from. Close or Esc here does not end the receive: it hides the
//!   window and tells the app (`host_close`), which embeds it again.
//!
//! Why a popup and not a `WS_CHILD`: `SetWindowDisplayAffinity` — the
//! guard against Relay capturing its own output (B9) — is honoured only on
//! top-level windows and only from the process that owns them. A child of
//! the app window would need the *app* to exclude its whole window from
//! capture, which it cannot do for a window it does not own either. So the
//! engine keeps the window top-level in every mode, applies the affinity
//! itself, reasserts it after every style change, and reports the verified
//! value in the `host` event so a lapse is visible rather than silent.
//!
//! This thread only pumps messages. Decode and present live on the render
//! thread and touch the window only through its HWND (the swapchain), so a
//! modal move/size loop here never stalls the picture. The two threads never
//! wait on each other while both are alive: see the `render` module docs.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use anyhow::{Context, Result};
use tracing::{info, warn};
use windows::core::w;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, GetStockObject, MonitorFromWindow, BLACK_BRUSH, HBRUSH, MONITORINFO,
    MONITOR_DEFAULTTONEAREST,
};
use windows::Win32::UI::HiDpi::{
    SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};
use windows::Win32::UI::Input::KeyboardAndMouse::VK_ESCAPE;
use windows::Win32::UI::WindowsAndMessaging::*;

use super::HostLink;
use crate::command::HostMode;

/// Posted by the transport: change hosting mode. `wparam` = mode
/// (0 embedded, 1 popout), `lparam` = owner HWND (embedded only).
const WM_APP_HOST: u32 = WM_APP + 1;
/// Posted by the render thread once its D3D objects are gone: destroy the
/// window and end the thread.
const WM_APP_SHUTDOWN: u32 = WM_APP + 2;
/// While embedded, a once-a-second check that the owner still exists.
const OWNER_TIMER: usize = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Standalone,
    Embedded { owner: HWND },
    PoppedOut,
}

impl Mode {
    fn label(self) -> &'static str {
        match self {
            Mode::Standalone => "none",
            Mode::Embedded { .. } => "embedded",
            Mode::PoppedOut => "popout",
        }
    }
}

/// Per-window state, reached from the window procedure via `GWLP_USERDATA`.
struct WinState {
    mode: Mode,
    /// Ever hosted by the app. Decides what close and Esc mean: end the
    /// receive (standalone) or go back into the app (hosted).
    hosted: bool,
    quit: Arc<AtomicBool>,
    stream_w: u32,
    stream_h: u32,
}

/// The window thread's handle: the HWND for the swapchain, and the join.
pub struct WindowThread {
    pub hwnd: HWND,
    /// What Windows reported back for the capture exclusion at creation.
    pub excluded_from_capture: bool,
    join: Option<std::thread::JoinHandle<()>>,
    initial: &'static str,
}

impl WindowThread {
    /// Create the window on a new thread and pump it until shutdown. Returns
    /// once the window exists (or creation failed).
    pub fn start(
        w: u32,
        h: u32,
        owner: Option<u64>,
        link: Arc<HostLink>,
        quit: Arc<AtomicBool>,
    ) -> Result<Self> {
        let (tx, rx) = std::sync::mpsc::channel::<Result<(isize, bool)>>();
        let quit2 = quit.clone();
        let join =
            std::thread::Builder::new().name("relay-render-win".into()).spawn(move || {
                let state = Box::new(WinState {
                    mode: Mode::Standalone,
                    hosted: owner.is_some(),
                    quit: quit2.clone(),
                    stream_w: w,
                    stream_h: h,
                });
                let raw = Box::into_raw(state);
                match create(w, h, owner, raw) {
                    Ok((hwnd, excluded)) => {
                        link.set_hwnd(hwnd);
                        let _ = tx.send(Ok((hwnd.0 as isize, excluded)));
                        // A host command that arrived before the window existed.
                        if let Some((mode, owner)) = link.take_pending() {
                            post_mode(hwnd, mode, owner);
                        }
                        pump();
                    }
                    Err(e) => {
                        let _ = tx.send(Err(e));
                    }
                }
                // Whatever ended the pump, the render thread must stop.
                quit2.store(true, Ordering::Release);
                // SAFETY: the window is destroyed (or was never created), so no
                // message can reach the state again.
                drop(unsafe { Box::from_raw(raw) });
            })?;
        let (hwnd, excluded_from_capture) = rx.recv().context("window thread died")??;
        Ok(Self {
            hwnd: HWND(hwnd as *mut _),
            excluded_from_capture,
            join: Some(join),
            initial: if owner.is_some() { "embedded" } else { "none" },
        })
    }

    /// The mode the window was created in, for `render_up`.
    pub fn mode_label(&self) -> &'static str {
        self.initial
    }

    /// Destroy the window and wait (briefly) for the thread. Call only after
    /// every D3D object targeting the window has been released.
    pub fn shutdown(mut self) {
        self.finish();
    }

    fn finish(&mut self) {
        let Some(join) = self.join.take() else { return };
        // SAFETY: posting to our own window; a dead handle just fails.
        let _ = unsafe { PostMessageW(Some(self.hwnd), WM_APP_SHUTDOWN, WPARAM(0), LPARAM(0)) };
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = join.join();
            let _ = tx.send(());
        });
        if rx.recv_timeout(std::time::Duration::from_secs(2)).is_err() {
            warn!("window thread did not stop within 2 s; exiting anyway");
        }
    }
}

impl Drop for WindowThread {
    fn drop(&mut self) {
        self.finish();
    }
}

/// Ask the window thread to change mode (any thread).
pub fn post_mode(hwnd: HWND, mode: HostMode, owner: u64) {
    let wp = match mode {
        HostMode::Embedded => 0,
        HostMode::Popout => 1,
    };
    // SAFETY: posting to a window we created; a dead handle just fails.
    let _ = unsafe { PostMessageW(Some(hwnd), WM_APP_HOST, WPARAM(wp), LPARAM(owner as isize)) };
}

fn create(w: u32, h: u32, owner: Option<u64>, state: *mut WinState) -> Result<(HWND, bool)> {
    // SAFETY: standard window-class registration + creation on this thread.
    unsafe {
        // Physical pixels everywhere, or the app (per-monitor aware) and this
        // window (which would be virtualised) disagree about where the video
        // area is, and DWM stretches the swapchain on a scaled display.
        // Process-wide, so it has to happen before the first window. A
        // failure means it was already set, which is fine.
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);

        let hinstance = windows::Win32::System::LibraryLoader::GetModuleHandleW(None)?;
        let class = WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            hInstance: hinstance.into(),
            lpszClassName: w!("RelayReceiver"),
            hCursor: LoadCursorW(None, IDC_ARROW)?,
            // Black until the first frame: a white flash inside a dark app
            // is the kind of thing that reads as a fault.
            hbrBackground: HBRUSH(GetStockObject(BLACK_BRUSH).0),
            ..Default::default()
        };
        // Fails harmlessly if the class already exists in this process.
        RegisterClassW(&class);

        let (style, exstyle, parent, rect) = match owner {
            Some(o) => (
                embedded_style(),
                embedded_exstyle(),
                Some(HWND(o as *mut _)),
                // The app positions it; until then it is hidden anyway.
                RECT { left: 0, top: 0, right: 16, bottom: 9 },
            ),
            None => {
                let mut r = fit_work_area(None, w, h);
                let _ = AdjustWindowRect(&mut r, popout_style(), false);
                (popout_style() | WS_VISIBLE, WINDOW_EX_STYLE(0), None, r)
            }
        };
        let hwnd = CreateWindowExW(
            exstyle,
            w!("RelayReceiver"),
            w!("Relay — receiving"),
            style,
            if owner.is_some() { 0 } else { CW_USEDEFAULT },
            if owner.is_some() { 0 } else { CW_USEDEFAULT },
            rect.right - rect.left,
            rect.bottom - rect.top,
            parent,
            None,
            Some(hinstance.into()),
            Some(state as *const core::ffi::c_void),
        )?;

        if let Some(o) = owner {
            (*state).mode = Mode::Embedded { owner: HWND(o as *mut _) };
            SetTimer(Some(hwnd), OWNER_TIMER, 1000, None);
        }
        let excluded = exclude_from_capture(hwnd);
        info!(mode = (*state).mode.label(), excluded, "receiver window up");
        Ok((hwnd, excluded))
    }
}

fn pump() {
    let mut msg = MSG::default();
    // SAFETY: a blocking message loop on the thread that owns the window.
    unsafe {
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

fn embedded_style() -> WINDOW_STYLE {
    WS_POPUP | WS_CLIPSIBLINGS | WS_CLIPCHILDREN
}

fn embedded_exstyle() -> WINDOW_EX_STYLE {
    // NOACTIVATE: a click on the picture must not take the keyboard from
    // the app. TOOLWINDOW: no taskbar button and no Alt+Tab entry of its own.
    WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW
}

fn popout_style() -> WINDOW_STYLE {
    // Resizable on purpose: a fixed-size window that does not fit is the
    // borderless-fullscreen trap described in the r4 sizing fix.
    WS_OVERLAPPEDWINDOW | WS_CLIPSIBLINGS | WS_CLIPCHILDREN
}

/// Make this window invisible to screen capture (B9).
///
/// Without it Relay will happily capture its own output: run a sender and a
/// receiver on one PC and the capture contains the window showing the
/// capture, which contains the window showing the capture. On this project's
/// dev machine that produced an unbounded feedback loop the user described
/// as "an infinite loop of whatever is on my screen, like smearing a
/// painting repeatedly", and it did not stop on its own.
///
/// `WDA_EXCLUDEFROMCAPTURE` hides the window from WGC and Desktop
/// Duplication while leaving it fully visible on screen — unlike
/// `WDA_MONITOR`, which blacks it out for the user too. Windows 10 2004+.
/// Best-effort: on an older build this fails and the window still works, it
/// is just capturable again. Returns what Windows reports back, not what was
/// asked for, so the `host` event carries a verified value.
fn exclude_from_capture(hwnd: HWND) -> bool {
    // SAFETY: our own window.
    unsafe {
        if SetWindowDisplayAffinity(hwnd, WDA_EXCLUDEFROMCAPTURE).is_err() {
            warn!(
                "could not exclude the receiver window from capture; \
                 sharing this PC's screen while receiving on it will feed back"
            );
            return false;
        }
        let mut affinity: u32 = 0;
        GetWindowDisplayAffinity(hwnd, &mut affinity).is_ok()
            && affinity == WDA_EXCLUDEFROMCAPTURE.0
    }
}

/// A rect for a top-level window: the stream's aspect at up to 90 % of the
/// work area (the desktop minus the taskbar) of the monitor `near` is on, or
/// the primary. Client size, not window size.
fn fit_work_area(near: Option<HWND>, w: u32, h: u32) -> RECT {
    // SAFETY: monitor queries with a correctly sized struct.
    let work = unsafe {
        let mon = MonitorFromWindow(near.unwrap_or_default(), MONITOR_DEFAULTTONEAREST);
        let mut mi =
            MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
        if GetMonitorInfoW(mon, &mut mi).as_bool() {
            mi.rcWork
        } else {
            RECT { left: 0, top: 0, right: 1280, bottom: 720 }
        }
    };
    let avail_w = ((work.right - work.left) as f64 * 0.9).max(320.0);
    let avail_h = ((work.bottom - work.top) as f64 * 0.9).max(180.0);
    let scale = (avail_w / w.max(1) as f64).min(avail_h / h.max(1) as f64).min(1.0);
    let win_w = ((w as f64 * scale).round() as i32).max(320);
    let win_h = ((h as f64 * scale).round() as i32).max(180);
    let left = work.left + ((work.right - work.left) - win_w) / 2;
    let top = work.top + ((work.bottom - work.top) - win_h) / 2;
    RECT { left, top, right: left + win_w, bottom: top + win_h }
}

/// Apply a hosting mode on the window thread and report the result.
unsafe fn apply_mode(hwnd: HWND, state: &mut WinState, mode: HostMode, owner: u64) {
    // Keep WS_VISIBLE as it is; the SetWindowPos flags below decide it.
    let visible = WINDOW_STYLE(GetWindowLongPtrW(hwnd, GWL_STYLE) as u32) & WS_VISIBLE;
    match mode {
        HostMode::Embedded => {
            let owner = HWND(owner as *mut _);
            if !IsWindow(Some(owner)).as_bool() {
                warn!(?owner, "embed requested into a window that does not exist; ignoring");
                return;
            }
            // Hide first: the app shows it again once it has positioned it,
            // so a framed window never flashes at its old place.
            let _ = ShowWindow(hwnd, SW_HIDE);
            SetWindowLongPtrW(hwnd, GWLP_HWNDPARENT, owner.0 as isize);
            SetWindowLongPtrW(hwnd, GWL_STYLE, (embedded_style() | visible).0 as isize);
            SetWindowLongPtrW(hwnd, GWL_EXSTYLE, embedded_exstyle().0 as isize);
            let _ = SetWindowPos(
                hwnd,
                None,
                0,
                0,
                0,
                0,
                SWP_NOMOVE
                    | SWP_NOSIZE
                    | SWP_NOZORDER
                    | SWP_NOACTIVATE
                    | SWP_FRAMECHANGED
                    | SWP_HIDEWINDOW,
            );
            SetTimer(Some(hwnd), OWNER_TIMER, 1000, None);
            state.mode = Mode::Embedded { owner };
        }
        HostMode::Popout => {
            let _ = KillTimer(Some(hwnd), OWNER_TIMER);
            let from = match state.mode {
                Mode::Embedded { owner } if IsWindow(Some(owner)).as_bool() => Some(owner),
                _ => None,
            };
            SetWindowLongPtrW(hwnd, GWLP_HWNDPARENT, 0);
            SetWindowLongPtrW(hwnd, GWL_STYLE, (popout_style() | visible).0 as isize);
            SetWindowLongPtrW(hwnd, GWL_EXSTYLE, WS_EX_APPWINDOW.0 as isize);
            let mut r = fit_work_area(from, state.stream_w, state.stream_h);
            let _ = AdjustWindowRect(&mut r, popout_style(), false);
            let _ = SetWindowPos(
                hwnd,
                Some(HWND_TOP),
                r.left,
                r.top,
                r.right - r.left,
                r.bottom - r.top,
                SWP_FRAMECHANGED | SWP_SHOWWINDOW,
            );
            // May be refused (foreground lock); then it simply appears
            // behind the app, which is still where the user can find it.
            let _ = SetForegroundWindow(hwnd);
            state.mode = Mode::PoppedOut;
        }
    }
    state.hosted = true;
    let excluded = exclude_from_capture(hwnd);
    info!(mode = state.mode.label(), excluded, "receiver window mode changed");
    println!(
        "{}",
        serde_json::json!({
            "event": "host",
            "mode": state.mode.label(),
            "hwnd": hwnd.0 as isize as u64,
            "excluded_from_capture": excluded,
        })
    );
}

/// Close or Esc. Standalone: end the receive. Hosted: hide and tell the
/// app, which embeds the stream again — closing the popped-out window is
/// "put it back", never "stop".
unsafe fn close_requested(hwnd: HWND, state: &mut WinState) {
    if state.hosted {
        let _ = ShowWindow(hwnd, SW_HIDE);
        println!("{}", serde_json::json!({ "event": "host_close" }));
    } else {
        state.quit.store(true, Ordering::Release);
    }
}

unsafe fn state_of<'a>(hwnd: HWND) -> Option<&'a mut WinState> {
    let p = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut WinState;
    p.as_mut()
}

extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    // SAFETY: standard window procedure; the state pointer was set from the
    // create parameters and outlives the window.
    unsafe {
        match msg {
            WM_NCCREATE => {
                let cs = &*(lp.0 as *const CREATESTRUCTW);
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, cs.lpCreateParams as isize);
                DefWindowProcW(hwnd, msg, wp, lp)
            }
            WM_APP_HOST => {
                if let Some(state) = state_of(hwnd) {
                    let mode = if wp.0 == 0 { HostMode::Embedded } else { HostMode::Popout };
                    apply_mode(hwnd, state, mode, lp.0 as u64);
                }
                LRESULT(0)
            }
            WM_APP_SHUTDOWN => {
                let _ = DestroyWindow(hwnd);
                LRESULT(0)
            }
            WM_DESTROY => {
                // Also how the window goes when its owner (the app) is
                // destroyed: Windows takes owned windows with it.
                if let Some(state) = state_of(hwnd) {
                    state.quit.store(true, Ordering::Release);
                }
                PostQuitMessage(0);
                LRESULT(0)
            }
            WM_CLOSE => {
                if let Some(state) = state_of(hwnd) {
                    close_requested(hwnd, state);
                }
                LRESULT(0)
            }
            // Escape closes it. A receive window that cannot be dismissed is a
            // trap, and the close button is the first thing to go out of reach
            // if the window is ever mis-sized again.
            WM_KEYDOWN if wp.0 as u32 == VK_ESCAPE.0 as u32 => {
                if let Some(state) = state_of(hwnd) {
                    close_requested(hwnd, state);
                }
                LRESULT(0)
            }
            // Embedded: never take activation from the app on a click...
            WM_MOUSEACTIVATE
                if matches!(state_of(hwnd).map(|s| s.mode), Some(Mode::Embedded { .. })) =>
            {
                LRESULT(MA_NOACTIVATE as isize)
            }
            // ...but a click on the picture should still bring the app
            // forward, as a click anywhere else in it would.
            WM_LBUTTONDOWN | WM_RBUTTONDOWN | WM_MBUTTONDOWN => {
                if let Some(Mode::Embedded { owner }) = state_of(hwnd).map(|s| s.mode) {
                    let _ = SetForegroundWindow(owner);
                }
                LRESULT(0)
            }
            // The app closed while the stream was embedded in it. If Windows
            // did not destroy this window along with its owner, do not leave
            // a frameless stream floating over the desktop: give it a frame.
            WM_TIMER if wp.0 == OWNER_TIMER => {
                if let Some(state) = state_of(hwnd) {
                    if let Mode::Embedded { owner } = state.mode {
                        if !IsWindow(Some(owner)).as_bool() {
                            warn!("app window gone while embedded; popping the stream out");
                            apply_mode(hwnd, state, HostMode::Popout, 0);
                        }
                    }
                }
                LRESULT(0)
            }
            // The swapchain paints everything; erasing would only flicker.
            WM_ERASEBKGND => LRESULT(1),
            _ => DefWindowProcW(hwnd, msg, wp, lp),
        }
    }
}
