//! The receiver's window thread and its hosting modes (S29, S50).
//!
//! Four modes:
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
//!   came from. Close or Esc here does not end the receive: the window embeds
//!   itself again into the app window it came from (it remembers the owner),
//!   and tells the app so (`host_close`, then the `host` event). It used to
//!   only hide and ask the app to re-embed it; on the second PC that
//!   four-hop round trip took anywhere from 1.6 s to 47 s and the owner clicked
//!   the close button several times waiting. If the app window is gone,
//!   close ends the receive, as it does for a standalone window.
//! - **Clean** (S50) — a clean feed for call apps and OBS: borderless, no
//!   Relay chrome, unowned with a taskbar button, at a *fixed* client size
//!   (1920x1080 or 2560x1440) that never changes, so whatever captures it
//!   never rescales or crops. The stream is letterboxed into it by the render
//!   thread. Esc or close re-embeds it, as for a popped-out window; a drag
//!   anywhere on it moves it.
//!
//! Every mode has the same class (`placement::CLASS_NAME`) and a title that
//! names the sender ("Relay — from JAKE"), which is what call-app and OBS
//! window pickers list.
//!
//! Why a popup and not a `WS_CHILD`: `SetWindowDisplayAffinity` — the
//! guard against Relay capturing its own output (B9) — is honoured only on
//! top-level windows and only from the process that owns them. So the engine
//! keeps the window top-level in every mode and applies the affinity itself.
//!
//! Since S50 the guard is conditional (`placement::should_exclude`): the
//! window is hidden from capture only while this same PC is sharing an area
//! the window is on, and capturable otherwise, so a call app on this PC can
//! pick it. The decision is re-taken when the core reports a local share
//! starting, switching or stopping (`HostLink::set_local_share`), after every
//! move or resize of the window, on a display change, and after every style
//! change; the `host` event carries the value Windows reports back.
//!
//! This thread only pumps messages. Decode and present live on the render
//! thread and touch the window only through its HWND (the swapchain), so a
//! modal move/size loop here never stalls the picture. The two threads never
//! wait on each other while both are alive: see the `render` module docs.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

use anyhow::{Context, Result};
use tracing::{info, warn};
use windows::core::{HSTRING, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, GetStockObject, MonitorFromWindow, BLACK_BRUSH, HBRUSH, HMONITOR, MONITORINFO,
    MONITOR_DEFAULTTONEAREST,
};
use windows::Win32::UI::HiDpi::{
    SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};
use windows::Win32::UI::Input::KeyboardAndMouse::VK_ESCAPE;
use windows::Win32::UI::Shell::ExtractIconW;
use windows::Win32::UI::WindowsAndMessaging::*;

use super::placement::{self, Rect};
use super::HostLink;
use crate::command::{CleanFeed, HostMode};

/// Posted by the transport: change hosting mode. `wparam` = mode
/// (see [`encode_mode`]), `lparam` = owner HWND (embedded only).
const WM_APP_HOST: u32 = WM_APP + 1;
/// Posted by the render thread once its D3D objects are gone: destroy the
/// window and end the thread.
const WM_APP_SHUTDOWN: u32 = WM_APP + 2;
/// Posted when something the capture guard depends on changed (a local share
/// started, switched or stopped; the window moved; the displays changed).
const WM_APP_GUARD: u32 = WM_APP + 3;
/// While embedded, a once-a-second check that the owner still exists.
const OWNER_TIMER: usize = 1;

/// The clean feed's client size while in that mode, `w << 32 | h`, else 0.
/// Read by `WM_GETMINMAXINFO`, which Windows sends from inside the
/// `SetWindowPos` that `apply_mode` makes while holding the window state, so
/// it must not go through the state. One stream window per process.
static CLEAN_FEED_SIZE: AtomicU64 = AtomicU64::new(0);
/// A `WM_APP_GUARD` is already queued; moves during a drag do not pile up.
static GUARD_QUEUED: AtomicBool = AtomicBool::new(false);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Standalone,
    Embedded { owner: HWND },
    PoppedOut,
    Clean,
}

impl Mode {
    fn label(self) -> &'static str {
        match self {
            Mode::Standalone => "none",
            Mode::Embedded { .. } => "embedded",
            Mode::PoppedOut => "popout",
            Mode::Clean => "clean",
        }
    }
}

/// Mode and feed into one `wparam`: 0 embedded, 1 popout, 2/3 clean at
/// 1080p/1440p.
fn encode_mode(mode: HostMode, feed: CleanFeed) -> usize {
    match (mode, feed) {
        (HostMode::Embedded, _) => 0,
        (HostMode::Popout, _) => 1,
        (HostMode::Clean, CleanFeed::Fhd) => 2,
        (HostMode::Clean, CleanFeed::Qhd) => 3,
    }
}

fn decode_mode(wp: usize) -> (HostMode, CleanFeed) {
    match wp {
        0 => (HostMode::Embedded, CleanFeed::Fhd),
        2 => (HostMode::Clean, CleanFeed::Fhd),
        3 => (HostMode::Clean, CleanFeed::Qhd),
        _ => (HostMode::Popout, CleanFeed::Fhd),
    }
}

/// Per-window state, reached from the window procedure via `GWLP_USERDATA`.
struct WinState {
    mode: Mode,
    /// Ever hosted by the app. Decides what close and Esc mean: end the
    /// receive (standalone) or go back into the app (hosted).
    hosted: bool,
    /// The app window this was last embedded in, so a popped-out window can
    /// put itself back without a round trip through the app.
    last_owner: Option<HWND>,
    quit: Arc<AtomicBool>,
    /// To tell the render thread the swapchain needs rebuilding, and where
    /// the local share (the capture guard's input) is kept.
    link: Arc<HostLink>,
    stream_w: u32,
    stream_h: u32,
    /// What Windows last confirmed for the capture exclusion.
    excluded: bool,
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
    /// once the window exists (or creation failed). `title` is what window
    /// pickers list it as (`placement::window_title`).
    pub fn start(
        w: u32,
        h: u32,
        owner: Option<u64>,
        title: String,
        link: Arc<HostLink>,
        quit: Arc<AtomicBool>,
    ) -> Result<Self> {
        // An owner that has since closed (the app window was closed and
        // reopened, and a restart still carries the old handle) must not be
        // fatal: come up unhosted and let the app embed it again.
        // SAFETY: IsWindow accepts any handle value.
        let owner = owner.filter(|&o| unsafe { IsWindow(Some(HWND(o as *mut _))) }.as_bool());
        let (tx, rx) = std::sync::mpsc::channel::<Result<(isize, bool)>>();
        let quit2 = quit.clone();
        let join =
            std::thread::Builder::new().name("relay-render-win".into()).spawn(move || {
                let state = Box::new(WinState {
                    mode: Mode::Standalone,
                    hosted: owner.is_some(),
                    last_owner: owner.map(|o| HWND(o as *mut _)),
                    quit: quit2.clone(),
                    link: link.clone(),
                    stream_w: w,
                    stream_h: h,
                    excluded: false,
                });
                let raw = Box::into_raw(state);
                match create(w, h, owner, &title, raw) {
                    Ok((hwnd, excluded)) => {
                        link.set_hwnd(hwnd);
                        let _ = tx.send(Ok((hwnd.0 as isize, excluded)));
                        // A host command that arrived before the window existed.
                        if let Some((mode, owner, feed)) = link.take_pending() {
                            post_mode(hwnd, mode, owner, feed);
                        }
                        // A local share reported between the decision above
                        // and `set_hwnd` would otherwise not be looked at.
                        post_guard(hwnd);
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
pub fn post_mode(hwnd: HWND, mode: HostMode, owner: u64, feed: CleanFeed) {
    let wp = encode_mode(mode, feed);
    // SAFETY: posting to a window we created; a dead handle just fails.
    let _ = unsafe { PostMessageW(Some(hwnd), WM_APP_HOST, WPARAM(wp), LPARAM(owner as isize)) };
}

/// Ask the window thread to re-take the capture decision (any thread).
pub fn post_guard(hwnd: HWND) {
    if GUARD_QUEUED.swap(true, Ordering::AcqRel) {
        return;
    }
    // SAFETY: posting to a window we created; a dead handle just fails.
    if unsafe { PostMessageW(Some(hwnd), WM_APP_GUARD, WPARAM(0), LPARAM(0)) }.is_err() {
        GUARD_QUEUED.store(false, Ordering::Release);
    }
}

fn wide(s: &str) -> HSTRING {
    HSTRING::from(s)
}

fn create(
    w: u32,
    h: u32,
    owner: Option<u64>,
    title: &str,
    state: *mut WinState,
) -> Result<(HWND, bool)> {
    // SAFETY: standard window-class registration + creation on this thread.
    unsafe {
        // Physical pixels everywhere, or the app (per-monitor aware) and this
        // window (which would be virtualised) disagree about where the video
        // area is, and DWM stretches the swapchain on a scaled display.
        // Process-wide, so it has to happen before the first window. A
        // failure means it was already set, which is fine.
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);

        let hinstance = windows::Win32::System::LibraryLoader::GetModuleHandleW(None)?;
        let icon = app_icon();
        let class_name = wide(placement::CLASS_NAME);
        let class = WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            hInstance: hinstance.into(),
            lpszClassName: PCWSTR(class_name.as_ptr()),
            hCursor: LoadCursorW(None, IDC_ARROW)?,
            hIcon: icon,
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
        let title = wide(title);
        let hwnd = CreateWindowExW(
            exstyle,
            PCWSTR(class_name.as_ptr()),
            PCWSTR(title.as_ptr()),
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

        // The class icon covers a window created *after* registration; a
        // class registered by an earlier window in this process keeps its
        // own. Set it on the window too, so the popped-out title bar, the
        // taskbar and Alt+Tab all show the Relay icon, the same one the tray
        // and the app window use.
        if !icon.is_invalid() {
            let _ = SendMessageW(
                hwnd,
                WM_SETICON,
                Some(WPARAM(ICON_BIG as usize)),
                Some(LPARAM(icon.0 as isize)),
            );
            let _ = SendMessageW(
                hwnd,
                WM_SETICON,
                Some(WPARAM(ICON_SMALL as usize)),
                Some(LPARAM(icon.0 as isize)),
            );
        }
        if let Some(o) = owner {
            (*state).mode = Mode::Embedded { owner: HWND(o as *mut _) };
            SetTimer(Some(hwnd), OWNER_TIMER, 1000, None);
        }
        reassess_capture(hwnd, &mut *state);
        let excluded = (*state).excluded;
        info!(mode = (*state).mode.label(), excluded, "receiver window up");
        Ok((hwnd, excluded))
    }
}

/// The Relay icon, taken from `relay-ui.exe` next to this binary — the one
/// artwork the installer ships, and what the core's tray icon uses too, so
/// the popped-out window matches the tray and the app. A tree without a
/// built UI gets the generic icon.
fn app_icon() -> HICON {
    use std::os::windows::ffi::OsStrExt;
    // SAFETY: a path in, a shared icon handle out; null/1 mean "none".
    unsafe {
        std::env::current_exe()
            .ok()
            .and_then(|me| relay_core::launcher::sibling_of(&me, relay_core::launcher::UI_EXE))
            .filter(|p| p.exists())
            .and_then(|p| {
                let wide: Vec<u16> =
                    p.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
                let h = ExtractIconW(None, PCWSTR(wide.as_ptr()), 0);
                (!h.is_invalid() && h.0 as usize != 1).then_some(h)
            })
            .unwrap_or_else(|| LoadIconW(None, IDI_APPLICATION).unwrap_or_default())
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

fn clean_style() -> WINDOW_STYLE {
    // Borderless, so the client area *is* the window and a capture of it
    // has no frame. A fixed size is the point here, unlike `popout_style`:
    // it can always be put back into the app with Esc.
    WS_POPUP | WS_CLIPSIBLINGS | WS_CLIPCHILDREN
}

/// Every monitor's rectangle in `d3d::monitors()` order — the order a
/// `SourceTarget` index refers to.
fn monitor_rects() -> Vec<Rect> {
    crate::d3d::monitors()
        .into_iter()
        .map(|m: HMONITOR| {
            let mut mi = MONITORINFO {
                cbSize: std::mem::size_of::<MONITORINFO>() as u32,
                ..Default::default()
            };
            // SAFETY: a monitor handle from enumeration and a sized struct.
            if unsafe { GetMonitorInfoW(m, &mut mi) }.as_bool() {
                let r = mi.rcMonitor;
                Rect { left: r.left, top: r.top, right: r.right, bottom: r.bottom }
            } else {
                Rect::default()
            }
        })
        .collect()
}

/// Set the display affinity Windows should have and return what it reports
/// back, not what was asked, so the `host` event carries a verified value.
///
/// `WDA_EXCLUDEFROMCAPTURE` hides the window from WGC and Desktop
/// Duplication while leaving it fully visible on screen — unlike
/// `WDA_MONITOR`, which blacks it out for the user too. Windows 10 2004+.
/// Best-effort: on an older build the exclusion fails and the window still
/// works, it is just capturable.
fn set_capture_exclusion(hwnd: HWND, want: bool) -> bool {
    // Test only (B16 lip-sync): a meter on the receiving PC reads the
    // picture's pixels, and the exclusion makes every capture path see black.
    // Never set by the core or the UI. The window is never excluded.
    if std::env::var_os("RELAY_NO_CAPTURE_EXCLUDE").is_some() {
        if want {
            static ONCE: std::sync::Once = std::sync::Once::new();
            ONCE.call_once(|| {
                warn!("capture exclusion DISABLED for testing (RELAY_NO_CAPTURE_EXCLUDE)")
            });
        }
        // SAFETY: our own window.
        let _ = unsafe { SetWindowDisplayAffinity(hwnd, WDA_NONE) };
        return false;
    }
    // SAFETY: our own window.
    unsafe {
        let affinity = if want { WDA_EXCLUDEFROMCAPTURE } else { WDA_NONE };
        if SetWindowDisplayAffinity(hwnd, affinity).is_err() && want {
            warn!(
                "could not exclude the receiver window from capture; \
                 sharing this PC's screen while receiving on it will feed back"
            );
        }
        let mut got: u32 = 0;
        GetWindowDisplayAffinity(hwnd, &mut got).is_ok() && got == WDA_EXCLUDEFROMCAPTURE.0
    }
}

/// Re-take the capture decision (`placement::should_exclude`) and apply it.
/// Returns whether the confirmed state changed.
///
/// Excluded only while this PC is sharing an area the window is on: that is
/// the B9 recursion, a capture that contains the window showing the capture,
/// which on this project's dev machine was "an infinite loop of whatever is
/// on my screen, like smearing a painting repeatedly". Any other time the
/// window stays capturable, so Discord, Zoom, Teams, Meet and OBS on this PC
/// can pick it (S50).
unsafe fn reassess_capture(hwnd: HWND, state: &mut WinState) -> bool {
    let local = state.link.local_share();
    let area = local.as_ref().map(|t| placement::shared_area(t, &monitor_rects()));
    let mut wr = RECT::default();
    let _ = GetWindowRect(hwnd, &mut wr);
    let window = Rect { left: wr.left, top: wr.top, right: wr.right, bottom: wr.bottom };
    let want = placement::should_exclude(area.as_ref(), window, hwnd.0 as isize as u64);
    let got = set_capture_exclusion(hwnd, want);
    let changed = got != state.excluded;
    if changed {
        info!(excluded = got, wanted = want, local_share = ?local, "capture exclusion changed");
    }
    state.excluded = got;
    changed
}

/// The work area (the desktop minus the taskbar) of the monitor `near` is
/// on, or the primary's.
fn work_area(near: Option<HWND>) -> RECT {
    // SAFETY: monitor queries with a correctly sized struct.
    unsafe {
        let mon = MonitorFromWindow(near.unwrap_or_default(), MONITOR_DEFAULTTONEAREST);
        let mut mi =
            MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
        if GetMonitorInfoW(mon, &mut mi).as_bool() {
            mi.rcWork
        } else {
            RECT { left: 0, top: 0, right: 1280, bottom: 720 }
        }
    }
}

/// A rect for a top-level window: the stream's aspect at up to 90 % of the
/// work area of the monitor `near` is on, or the primary. Client size, not
/// window size.
fn fit_work_area(near: Option<HWND>, w: u32, h: u32) -> RECT {
    let work = work_area(near);
    let avail_w = ((work.right - work.left) as f64 * 0.9).max(320.0);
    let avail_h = ((work.bottom - work.top) as f64 * 0.9).max(180.0);
    let scale = (avail_w / w.max(1) as f64).min(avail_h / h.max(1) as f64).min(1.0);
    let win_w = ((w as f64 * scale).round() as i32).max(320);
    let win_h = ((h as f64 * scale).round() as i32).max(180);
    let left = work.left + ((work.right - work.left) - win_w) / 2;
    let top = work.top + ((work.bottom - work.top) - win_h) / 2;
    RECT { left, top, right: left + win_w, bottom: top + win_h }
}

/// Print the `host` event with the current mode and confirmed exclusion.
fn emit_host(hwnd: HWND, state: &WinState) {
    println!(
        "{}",
        serde_json::json!({
            "event": "host",
            "mode": state.mode.label(),
            "hwnd": hwnd.0 as isize as u64,
            "excluded_from_capture": state.excluded,
        })
    );
}

/// Apply a hosting mode on the window thread and report the result.
unsafe fn apply_mode(
    hwnd: HWND,
    state: &mut WinState,
    mode: HostMode,
    owner: u64,
    feed: CleanFeed,
) {
    // Keep WS_VISIBLE as it is; the SetWindowPos flags below decide it.
    let visible = WINDOW_STYLE(GetWindowLongPtrW(hwnd, GWL_STYLE) as u32) & WS_VISIBLE;
    match mode {
        HostMode::Embedded => {
            let owner = HWND(owner as *mut _);
            if !IsWindow(Some(owner)).as_bool() {
                warn!(?owner, "embed requested into a window that does not exist; ignoring");
                return;
            }
            CLEAN_FEED_SIZE.store(0, Ordering::Release);
            state.link.set_surface_size(None);
            // Do NOT hide it here. It used to be hidden so that the app could
            // show it once positioned, and the app did so 2-3 ms later — and
            // on the second PC that hide-then-show inside one DWM frame left
            // the window composed black until something (minimise/restore)
            // made DWM rebuild it. So the window keeps whatever visibility it
            // has: a popped-out window is restyled in place and the app moves
            // it into the video area a moment later; a window created hidden
            // stays hidden until the app shows it.
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
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_FRAMECHANGED,
            );
            SetTimer(Some(hwnd), OWNER_TIMER, 1000, None);
            state.mode = Mode::Embedded { owner };
            state.last_owner = Some(owner);
        }
        HostMode::Popout => {
            let _ = KillTimer(Some(hwnd), OWNER_TIMER);
            CLEAN_FEED_SIZE.store(0, Ordering::Release);
            state.link.set_surface_size(None);
            let from = match state.mode {
                Mode::Embedded { owner } if IsWindow(Some(owner)).as_bool() => Some(owner),
                Mode::Clean => Some(hwnd),
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
        HostMode::Clean => {
            let _ = KillTimer(Some(hwnd), OWNER_TIMER);
            let from = match state.mode {
                Mode::Embedded { owner } if IsWindow(Some(owner)).as_bool() => Some(owner),
                Mode::PoppedOut | Mode::Clean => Some(hwnd),
                _ => None,
            };
            let (fw, fh) = feed.size();
            // Before the SetWindowPos below: it sends WM_GETMINMAXINFO, and
            // a 1440p feed on a 1080p monitor must not be clamped to it.
            CLEAN_FEED_SIZE.store(((fw as u64) << 32) | fh as u64, Ordering::Release);
            // The render thread letterboxes into a back buffer of this size.
            state.link.set_surface_size(Some((fw, fh)));
            state.mode = Mode::Clean;
            SetWindowLongPtrW(hwnd, GWLP_HWNDPARENT, 0);
            SetWindowLongPtrW(hwnd, GWL_STYLE, (clean_style() | visible).0 as isize);
            SetWindowLongPtrW(hwnd, GWL_EXSTYLE, WS_EX_APPWINDOW.0 as isize);
            let w = work_area(from);
            let r = placement::clean_feed_rect(
                Rect { left: w.left, top: w.top, right: w.right, bottom: w.bottom },
                feed,
            );
            let _ = SetWindowPos(
                hwnd,
                Some(HWND_TOP),
                r.left,
                r.top,
                r.width(),
                r.height(),
                SWP_FRAMECHANGED | SWP_SHOWWINDOW,
            );
            let _ = SetForegroundWindow(hwnd);
            info!(w = fw, h = fh, x = r.left, y = r.top, "clean feed up");
        }
    }
    state.hosted = true;
    reassess_capture(hwnd, state);
    state.link.bump_surface();
    info!(mode = state.mode.label(), excluded = state.excluded, "receiver window mode changed");
    emit_host(hwnd, state);
}

/// Close or Esc. Standalone: end the receive. Popped out or a clean feed:
/// put the stream back into the app window it came from — closing it is
/// "put it back", never "stop" — unless that window is gone, in which case
/// there is nowhere to go back to and close means stop.
unsafe fn close_requested(hwnd: HWND, state: &mut WinState, why: &str) {
    let back_to = state.last_owner.filter(|o| IsWindow(Some(*o)).as_bool());
    info!(
        why,
        mode = state.mode.label(),
        hosted = state.hosted,
        app_window_alive = back_to.is_some(),
        "close requested"
    );
    match (state.hosted, back_to) {
        (true, Some(owner)) => {
            println!("{}", serde_json::json!({ "event": "host_close", "why": why }));
            apply_mode(hwnd, state, HostMode::Embedded, owner.0 as isize as u64, CleanFeed::Fhd);
        }
        _ => state.quit.store(true, Ordering::Release),
    }
}

unsafe fn state_of<'a>(hwnd: HWND) -> Option<&'a mut WinState> {
    let p = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut WinState;
    p.as_mut()
}

fn is_clean() -> bool {
    CLEAN_FEED_SIZE.load(Ordering::Acquire) != 0
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
                    let (mode, feed) = decode_mode(wp.0);
                    info!(?mode, ?feed, owner = lp.0, from = state.mode.label(), "host command");
                    apply_mode(hwnd, state, mode, lp.0 as u64, feed);
                }
                LRESULT(0)
            }
            WM_APP_GUARD => {
                GUARD_QUEUED.store(false, Ordering::Release);
                if let Some(state) = state_of(hwnd) {
                    if reassess_capture(hwnd, state) {
                        emit_host(hwnd, state);
                    }
                }
                LRESULT(0)
            }
            // Moved or resized (by the app, by a drag, by Windows): it may
            // now be on, or off, a monitor this PC is sharing. Deferred, not
            // decided here: this arrives from inside `apply_mode`'s own
            // SetWindowPos, which is holding the window state.
            WM_WINDOWPOSCHANGED | WM_DISPLAYCHANGE => {
                post_guard(hwnd);
                DefWindowProcW(hwnd, msg, wp, lp)
            }
            // A clean feed keeps its exact size even where it is larger than
            // the monitor; Windows would otherwise clamp it to the screen.
            WM_GETMINMAXINFO => {
                let packed = CLEAN_FEED_SIZE.load(Ordering::Acquire);
                if packed != 0 {
                    let mmi = &mut *(lp.0 as *mut MINMAXINFO);
                    let (w, h) = ((packed >> 32) as i32, (packed & 0xFFFF_FFFF) as i32);
                    mmi.ptMaxTrackSize =
                        POINT { x: mmi.ptMaxTrackSize.x.max(w), y: mmi.ptMaxTrackSize.y.max(h) };
                    mmi.ptMaxSize = POINT { x: mmi.ptMaxSize.x.max(w), y: mmi.ptMaxSize.y.max(h) };
                    return LRESULT(0);
                }
                DefWindowProcW(hwnd, msg, wp, lp)
            }
            // A clean feed has no title bar; a drag anywhere on it moves it.
            WM_NCHITTEST if is_clean() => LRESULT(HTCAPTION as isize),
            // ...and a double-click on that "caption" must not maximise it.
            WM_NCLBUTTONDBLCLK if is_clean() => LRESULT(0),
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
                CLEAN_FEED_SIZE.store(0, Ordering::Release);
                PostQuitMessage(0);
                LRESULT(0)
            }
            WM_CLOSE => {
                if let Some(state) = state_of(hwnd) {
                    close_requested(hwnd, state, "close");
                }
                LRESULT(0)
            }
            // Escape closes it. A receive window that cannot be dismissed is a
            // trap, and the close button is the first thing to go out of reach
            // if the window is ever mis-sized again.
            WM_KEYDOWN if wp.0 as u32 == VK_ESCAPE.0 as u32 => {
                if let Some(state) = state_of(hwnd) {
                    close_requested(hwnd, state, "escape");
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
                            apply_mode(hwnd, state, HostMode::Popout, 0, CleanFeed::Fhd);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mode_and_feed_survive_the_wparam() {
        for (m, f) in [
            (HostMode::Embedded, CleanFeed::Fhd),
            (HostMode::Popout, CleanFeed::Fhd),
            (HostMode::Clean, CleanFeed::Fhd),
            (HostMode::Clean, CleanFeed::Qhd),
        ] {
            assert_eq!(decode_mode(encode_mode(m, f)), (m, f));
        }
        assert_eq!(decode_mode(99).0, HostMode::Popout, "junk pops out, never embeds");
    }
}
