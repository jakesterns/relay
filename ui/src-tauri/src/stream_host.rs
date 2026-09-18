//! Where the received stream's window sits (S29).
//!
//! The stream is a native D3D11 window owned by `relay-share`, not a DOM
//! element: a swapchain cannot go into a WebView2 page. The engine makes it a
//! frameless popup *owned by* this window and hidden; this module puts it
//! over the Receive screen's video area and keeps it there through moves,
//! resizes and DPI changes, and hides it when that screen is not showing.
//!
//! Division of labour, and why: the engine owns every *style* change (owner,
//! frame, capture exclusion) because `SetWindowDisplayAffinity` only works
//! from the process that owns the window, and the shell owns *position*
//! because it is the one that knows where its client area is and gets the
//! move/resize events. Position goes straight to `SetWindowPos` on the
//! engine's window rather than through the core: during a drag that is a
//! call per mouse move, and a round trip over the pipe for each would put
//! the picture visibly behind the frame it sits in.
//!
//! The shell never shows the window until the engine has confirmed the
//! embedded mode (the `host` event), so a framed window cannot flash at the
//! video area during the pop-in transition.

use std::sync::Mutex;

use serde::{Deserialize, Serialize};

/// The video area in CSS pixels, relative to the webview's viewport (which
/// is this window's whole client area: the chrome is drawn in the page).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Area {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

/// What the webview needs to know: whether there is a stream window, how it
/// is hosted, its size (for the placeholder's aspect) and the B9 guard —
/// plus the receive state the core last pushed, so a Receive screen that
/// mounts mid-receive (the user went to Settings and back) does not show
/// "Idle" and a Start button beside a picture that is visibly playing.
#[derive(Debug, Clone, Serialize)]
pub struct StreamStatus {
    pub live: bool,
    pub mode: String,
    pub width: u32,
    pub height: u32,
    pub excluded_from_capture: bool,
    pub receiving: bool,
    pub code: Option<String>,
    pub sender: Option<String>,
    pub codec: Option<String>,
}

#[derive(Default)]
struct Host {
    /// The engine's window; 0 = none.
    hwnd: u64,
    width: u32,
    height: u32,
    /// Engine-confirmed hosting mode: `embedded`, `popout`, or `none`.
    mode: String,
    /// Where the webview last said the video area is; `None` while the
    /// Receive screen is not showing.
    area: Option<Area>,
    excluded_from_capture: bool,
    /// The receive state, merged from `ReceiveStatus` events the way the
    /// page merges them: a field only changes when an event carries it,
    /// and everything clears when receiving stops.
    receiving: bool,
    code: Option<String>,
    sender: Option<String>,
    codec: Option<String>,
}

static HOST: Mutex<Host> = Mutex::new(Host {
    hwnd: 0,
    width: 0,
    height: 0,
    mode: String::new(),
    area: None,
    excluded_from_capture: true,
    receiving: false,
    code: None,
    sender: None,
    codec: None,
});

pub fn status() -> StreamStatus {
    let g = HOST.lock().unwrap();
    StreamStatus {
        live: g.hwnd != 0,
        mode: if g.hwnd != 0 { g.mode.clone() } else { "none".into() },
        width: g.width,
        height: g.height,
        excluded_from_capture: g.excluded_from_capture,
        receiving: g.receiving,
        code: g.code.clone(),
        sender: g.sender.clone(),
        codec: g.codec.clone(),
    }
}

/// A `ReceiveStatus` event passed through the shell.
pub fn on_receive_status(
    receiving: bool,
    code: Option<&str>,
    sender: Option<&str>,
    codec: Option<&str>,
) {
    let mut g = HOST.lock().unwrap();
    g.receiving = receiving;
    if let Some(c) = code {
        g.code = Some(c.to_string());
    }
    if let Some(s) = sender {
        g.sender = Some(s.to_string());
    }
    if let Some(c) = codec {
        g.codec = Some(c.to_string());
    }
    if !receiving {
        g.code = None;
        g.sender = None;
        g.codec = None;
        g.hwnd = 0;
        g.mode.clear();
    }
}

/// The engine reported its window (created, or changed mode).
pub fn on_window(hwnd: u64, width: u32, height: u32, mode: &str, excluded: bool) {
    tracing::info!(hwnd, width, height, mode, excluded, "stream window event");
    let popped_out_now = {
        let mut g = HOST.lock().unwrap();
        g.hwnd = hwnd;
        if width > 0 && height > 0 {
            g.width = width;
            g.height = height;
        }
        let was = std::mem::replace(&mut g.mode, mode.to_string());
        g.excluded_from_capture = excluded;
        mode == "popout" && was != "popout"
    };
    // The engine cannot bring its own window to the front: Windows refuses
    // SetForegroundWindow to a process that did not get the last input. This
    // process did (the user clicked Pop out here), so it can hand the
    // foreground over. Without this the popped-out window sat behind the
    // app and Esc went to the app instead of closing it.
    if popped_out_now {
        #[cfg(windows)]
        win::bring_to_front(hwnd);
    }
}

/// The webview measured the video area, or left the Receive screen (`None`).
pub fn set_area(area: Option<Area>) {
    tracing::info!(?area, "video area from the page");
    HOST.lock().unwrap().area = area;
}

/// Put the stream window where the video area is, or hide it. Cheap and
/// idempotent: called on every move, resize and DPI change of this window
/// and on every measurement from the page.
pub fn apply(window: &tauri::Window) {
    let (hwnd, size, mode, area) = {
        let g = HOST.lock().unwrap();
        (g.hwnd, (g.width, g.height), g.mode.clone(), g.area)
    };
    if hwnd == 0 || mode != "embedded" {
        // Popped out: the engine owns its placement. None: nothing to place.
        tracing::info!(hwnd, mode, "apply: nothing to place");
        return;
    }
    #[cfg(windows)]
    win::place(window, hwnd, size, area);
    #[cfg(not(windows))]
    let _ = (window, size, area);
}

/// The rectangle a `w`×`h` stream fills inside an `aw`×`ah` box, keeping its
/// aspect and centred: the same letterboxing a `<video>` with
/// `object-fit: contain` would do. Integer pixels, so edges are crisp.
pub fn fit(aw: i32, ah: i32, (w, h): (u32, u32)) -> (i32, i32, i32, i32) {
    if aw <= 0 || ah <= 0 {
        return (0, 0, 0, 0);
    }
    if w == 0 || h == 0 {
        return (0, 0, aw, ah);
    }
    let scale = (aw as f64 / w as f64).min(ah as f64 / h as f64);
    let vw = ((w as f64 * scale).round() as i32).clamp(1, aw);
    let vh = ((h as f64 * scale).round() as i32).clamp(1, ah);
    ((aw - vw) / 2, (ah - vh) / 2, vw, vh)
}

#[cfg(windows)]
mod win {
    use windows::Win32::Foundation::{HWND, POINT};
    use windows::Win32::Graphics::Gdi::ClientToScreen;
    use windows::Win32::UI::WindowsAndMessaging::{
        IsIconic, IsWindow, IsWindowVisible, SetForegroundWindow, SetWindowPos, ShowWindow,
        SWP_NOACTIVATE, SWP_NOZORDER, SWP_SHOWWINDOW, SW_HIDE,
    };

    use super::{fit, Area};

    pub fn bring_to_front(hwnd: u64) {
        let stream = HWND(hwnd as *mut _);
        // SAFETY: a window handle the engine reported; a stale one fails.
        unsafe {
            if IsWindow(Some(stream)).as_bool() {
                let _ = SetForegroundWindow(stream);
            }
        }
    }

    pub fn place(window: &tauri::Window, hwnd: u64, size: (u32, u32), area: Option<Area>) {
        let stream = HWND(hwnd as *mut _);
        let Ok(host) = window.hwnd() else { return };
        let host = HWND(host.0);
        // SAFETY: plain window queries and a move of a window that the
        // engine created for exactly this purpose. A stale handle fails
        // harmlessly.
        unsafe {
            if !IsWindow(Some(stream)).as_bool() {
                tracing::warn!(hwnd, "apply: the stream window handle is not a window");
                return;
            }
            // Owned windows hide with a minimised owner; showing one now
            // would put it back on screen with the app gone.
            let Some(a) = area.filter(|_| !IsIconic(host).as_bool()) else {
                let _ = ShowWindow(stream, SW_HIDE);
                tracing::info!(
                    hwnd,
                    minimised = IsIconic(host).as_bool(),
                    has_area = area.is_some(),
                    "apply: hidden"
                );
                return;
            };
            let scale = window.scale_factor().unwrap_or(1.0);
            let ax = (a.x * scale).round() as i32;
            let ay = (a.y * scale).round() as i32;
            let aw = (a.w * scale).round() as i32;
            let ah = (a.h * scale).round() as i32;
            if aw <= 0 || ah <= 0 {
                let _ = ShowWindow(stream, SW_HIDE);
                tracing::info!(hwnd, aw, ah, "apply: hidden, empty area");
                return;
            }
            let (dx, dy, vw, vh) = fit(aw, ah, size);
            let mut origin = POINT { x: 0, y: 0 };
            let _ = ClientToScreen(host, &mut origin);
            let (x, y) = (origin.x + ax + dx, origin.y + ay + dy);
            let placed = SetWindowPos(
                stream,
                None,
                x,
                y,
                vw,
                vh,
                SWP_NOZORDER | SWP_NOACTIVATE | SWP_SHOWWINDOW,
            );
            tracing::info!(
                hwnd,
                x,
                y,
                w = vw,
                h = vh,
                scale,
                ok = placed.is_ok(),
                visible = IsWindowVisible(stream).as_bool(),
                "apply: placed"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fit;

    #[test]
    fn a_wider_box_letterboxes_left_and_right() {
        // 16:9 stream in a 2:1 box of 1000x500: 889x500, centred.
        assert_eq!(fit(1000, 500, (1920, 1080)), (55, 0, 889, 500));
    }

    #[test]
    fn a_taller_box_letterboxes_top_and_bottom() {
        assert_eq!(fit(800, 800, (1920, 1080)), (0, 175, 800, 450));
    }

    #[test]
    fn an_exact_fit_fills_the_box() {
        assert_eq!(fit(1920, 1080, (1920, 1080)), (0, 0, 1920, 1080));
        assert_eq!(fit(960, 540, (1920, 1080)), (0, 0, 960, 540));
    }

    #[test]
    fn an_unknown_stream_size_fills_the_box() {
        assert_eq!(fit(640, 400, (0, 0)), (0, 0, 640, 400));
    }

    #[test]
    fn an_empty_box_places_nothing() {
        assert_eq!(fit(0, 300, (1920, 1080)), (0, 0, 0, 0));
        assert_eq!(fit(300, -1, (1920, 1080)), (0, 0, 0, 0));
    }
}
