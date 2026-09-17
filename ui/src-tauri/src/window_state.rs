//! Where the window was: size, position and whether it was maximised, so the
//! next launch opens where the user left it instead of at 1280×800 centred.
//!
//! This lives in the window's own process on purpose. The always-on core
//! never opens, reads or links any of it — a window that is not running has
//! no state worth holding in memory, and the core's footprint budget is for
//! applying profiles.
//!
//! The file is `data\window.json` under the data root, so "delete my data" on
//! uninstall takes it with everything else (`Paths::data_paths` covers
//! `data\`). Physical pixels and outer bounds throughout, as Windows reports
//! them.
//!
//! The bounds come from `GetWindowPlacement` at close rather than from
//! tracking move and resize events: that is the one call that reports the
//! *normal* rectangle whatever state the window is in. Tracking was tried
//! first and recorded the maximised rectangle, because the window only reports
//! itself maximised after the resize that maximises it has been delivered.
//!
//! Restoring is the part that needs care: a saved position can point at a
//! monitor that has since been unplugged, or a laptop that is now undocked.
//! `placement` is the pure rule for that, and anything it cannot place ends up
//! centred at the default size, exactly as before this existed.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// The saved state. `x`, `y`, `width` and `height` are always the *normal*
/// (un-maximised) bounds, so un-maximising after a restore lands somewhere
/// sensible rather than filling the screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct WindowState {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    #[serde(default)]
    pub maximized: bool,
}

/// A monitor's work area (the screen minus the taskbar), in physical pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Area {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

/// How much of the window's top strip — where the custom title bar is drawn —
/// must be on a work area for the window to count as reachable. Enough to
/// grab with a mouse; less than that and the user could not move it back.
const GRAB_HEIGHT: i64 = 32;
const GRAB_MIN_WIDTH: i64 = 120;

/// Decide where a saved window goes on the monitors that exist now.
///
/// `None` means "do not use it": the title bar would not be reachable on any
/// work area, or the saved size is nonsense. Otherwise the window keeps its
/// saved bounds, shrunk to fit and nudged inside the work area its title bar
/// is mostly on, so a window saved on a larger monitor still fits.
pub fn placement(saved: WindowState, areas: &[Area]) -> Option<WindowState> {
    if saved.width == 0 || saved.height == 0 {
        return None;
    }
    let (x, y, w) = (saved.x as i64, saved.y as i64, saved.width as i64);
    let grab = |a: &Area| {
        let (ax, ay) = (a.x as i64, a.y as i64);
        let across = (x + w).min(ax + a.width as i64) - x.max(ax);
        let down = (y + GRAB_HEIGHT).min(ay + a.height as i64) - y.max(ay);
        if across >= GRAB_MIN_WIDTH.min(w) && down >= GRAB_HEIGHT {
            across * down
        } else {
            0
        }
    };
    let area = areas.iter().filter(|a| grab(a) > 0).max_by_key(|a| grab(a))?;

    let width = saved.width.min(area.width);
    let height = saved.height.min(area.height);
    let x = saved.x.clamp(area.x, area.x + (area.width - width) as i32);
    let y = saved.y.clamp(area.y, area.y + (area.height - height) as i32);
    Some(WindowState { x, y, width, height, maximized: saved.maximized })
}

/// `data\window.json` under the default data root.
pub fn file() -> Option<PathBuf> {
    relay_core::config::Paths::default_for_user().ok().map(|p| p.window_file())
}

pub fn load() -> Option<WindowState> {
    let bytes = std::fs::read(file()?).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// Best effort: a window position is not worth an error dialog on exit.
pub fn save(state: WindowState) {
    let Some(path) = file() else { return };
    let Ok(json) = serde_json::to_vec_pretty(&state) else { return };
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let tmp = path.with_extension("tmp");
    if std::fs::write(&tmp, json).is_ok() {
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::rename(&tmp, &path);
    }
}

fn areas(window: &tauri::Window) -> Vec<Area> {
    window
        .available_monitors()
        .unwrap_or_default()
        .iter()
        .map(|m| {
            let r = m.work_area();
            Area { x: r.position.x, y: r.position.y, width: r.size.width, height: r.size.height }
        })
        .collect()
}

/// Put the window back where it was, then show it. The window is created
/// hidden (`"visible": false`) so this never shows a jump from the centre;
/// every path ends in `show`, so a failure here still opens a window.
pub fn restore(window: &tauri::Window) {
    if let Some(state) = load().and_then(|s| placement(s, &areas(window))) {
        win::set_bounds(window, state);
        if state.maximized {
            // The normal bounds are already in place, so un-maximising later
            // lands where the window was, on the monitor it was maximised on.
            let _ = window.maximize();
        }
    }
    let _ = window.show();
    let _ = window.set_focus();
}

/// Write the state as the window goes away.
pub fn persist(window: &tauri::Window) {
    if let Some(state) = win::placement_of(window) {
        save(state);
    }
}

#[cfg(windows)]
mod win {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::Graphics::Gdi::{
        GetMonitorInfoW, MonitorFromRect, MONITORINFO, MONITOR_DEFAULTTONEAREST,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        GetWindowPlacement, SetWindowPos, SWP_NOACTIVATE, SWP_NOZORDER, SW_SHOWMAXIMIZED,
        SW_SHOWMINIMIZED, WINDOWPLACEMENT, WPF_RESTORETOMAXIMIZED,
    };

    use super::WindowState;

    /// The normal bounds in screen coordinates, and whether the window is (or
    /// would un-minimise to) maximised.
    pub fn placement_of(window: &tauri::Window) -> Option<WindowState> {
        let hwnd = HWND(window.hwnd().ok()?.0);
        let mut wp = WINDOWPLACEMENT {
            length: std::mem::size_of::<WINDOWPLACEMENT>() as u32,
            ..Default::default()
        };
        // SAFETY: a live window handle from Tauri and a correctly sized struct.
        unsafe { GetWindowPlacement(hwnd, &mut wp) }.ok()?;
        let r = wp.rcNormalPosition;
        // `rcNormalPosition` is in *workspace* coordinates, which differ from
        // screen coordinates by the taskbar when it sits on the top or left
        // of that monitor.
        // SAFETY: plain monitor queries with a correctly sized struct.
        let (dx, dy) = unsafe {
            let mon = MonitorFromRect(&r, MONITOR_DEFAULTTONEAREST);
            let mut mi = MONITORINFO {
                cbSize: std::mem::size_of::<MONITORINFO>() as u32,
                ..Default::default()
            };
            if GetMonitorInfoW(mon, &mut mi).as_bool() {
                (mi.rcWork.left - mi.rcMonitor.left, mi.rcWork.top - mi.rcMonitor.top)
            } else {
                (0, 0)
            }
        };
        let show = wp.showCmd as i32;
        let maximized = show == SW_SHOWMAXIMIZED.0
            || (show == SW_SHOWMINIMIZED.0 && wp.flags.contains(WPF_RESTORETOMAXIMIZED));
        Some(WindowState {
            x: r.left + dx,
            y: r.top + dy,
            width: u32::try_from(r.right - r.left).ok()?,
            height: u32::try_from(r.bottom - r.top).ok()?,
            maximized,
        })
    }

    /// Outer bounds, so through `SetWindowPos` rather than Tauri's
    /// `set_size`, which sizes the client area.
    pub fn set_bounds(window: &tauri::Window, s: WindowState) {
        let Ok(hwnd) = window.hwnd() else { return };
        // SAFETY: a live window handle from Tauri. A failure leaves the
        // window where it was created: centred, the old behaviour.
        let _ = unsafe {
            SetWindowPos(
                HWND(hwnd.0),
                None,
                s.x,
                s.y,
                s.width as i32,
                s.height as i32,
                SWP_NOZORDER | SWP_NOACTIVATE,
            )
        };
    }
}

/// Stub: nothing is read or restored, so the window opens at Tauri's default
/// (centred) every launch. A port reads the frame from Tauri's own
/// `outer_position` / `outer_size`, which on macOS are the window frame.
#[cfg(not(windows))]
mod win {
    use super::WindowState;

    pub fn placement_of(_window: &tauri::Window) -> Option<WindowState> {
        None
    }

    pub fn set_bounds(_window: &tauri::Window, _s: WindowState) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    const PRIMARY: Area = Area { x: 0, y: 0, width: 2560, height: 1392 };
    const RIGHT: Area = Area { x: 2560, y: 0, width: 1920, height: 1040 };

    fn at(x: i32, y: i32, width: u32, height: u32) -> WindowState {
        WindowState { x, y, width, height, maximized: false }
    }

    #[test]
    fn a_window_on_screen_comes_back_exactly() {
        let s = at(200, 120, 1400, 900);
        assert_eq!(placement(s, &[PRIMARY]), Some(s));
        let m = WindowState { maximized: true, ..s };
        assert_eq!(placement(m, &[PRIMARY]), Some(m));
    }

    #[test]
    fn a_window_on_an_unplugged_monitor_is_not_placed() {
        // Saved on the right-hand monitor, which is gone now.
        assert_eq!(placement(at(2800, 100, 1280, 800), &[PRIMARY]), None);
        // And it is fine while that monitor is still there.
        let s = at(2800, 100, 1280, 800);
        assert_eq!(placement(s, &[PRIMARY, RIGHT]), Some(s));
    }

    #[test]
    fn a_title_bar_off_the_top_is_not_reachable() {
        // Most of the window is visible but its title bar is above the
        // screen, so it could never be dragged back.
        assert_eq!(placement(at(100, -200, 1280, 800), &[PRIMARY]), None);
    }

    #[test]
    fn a_sliver_of_title_bar_is_not_enough() {
        // Only 40 px of the top strip is on the screen.
        assert_eq!(placement(at(2520, 100, 1280, 800), &[PRIMARY]), None);
        // 400 px of it is.
        assert!(placement(at(2160, 100, 1280, 800), &[PRIMARY]).is_some());
    }

    #[test]
    fn a_window_hanging_off_an_edge_is_pulled_inside() {
        let placed = placement(at(2160, 900, 1280, 800), &[PRIMARY]).unwrap();
        assert_eq!(placed, at(2560 - 1280, 1392 - 800, 1280, 800));
    }

    #[test]
    fn a_window_saved_on_a_bigger_monitor_shrinks_to_fit() {
        let laptop = Area { x: 0, y: 0, width: 1536, height: 816 };
        let placed = placement(at(40, 40, 2400, 1300), &[laptop]).unwrap();
        assert_eq!(placed, at(0, 0, 1536, 816));
    }

    #[test]
    fn it_goes_to_the_monitor_holding_most_of_the_title_bar() {
        // Straddles the two; more of the top strip is on the right one.
        let placed = placement(at(2360, 50, 1280, 800), &[PRIMARY, RIGHT]).unwrap();
        assert_eq!(placed, at(2560, 50, 1280, 800));
    }

    #[test]
    fn monitors_left_of_and_above_the_primary_work() {
        let left = Area { x: -1920, y: -200, width: 1920, height: 1040 };
        let s = at(-1700, -150, 1280, 800);
        assert_eq!(placement(s, &[PRIMARY, left]), Some(s));
    }

    #[test]
    fn nonsense_is_not_placed() {
        assert_eq!(placement(at(0, 0, 0, 800), &[PRIMARY]), None);
        assert_eq!(placement(at(0, 0, 1280, 800), &[]), None);
    }

    #[test]
    fn old_files_without_maximized_still_parse() {
        let s: WindowState = serde_json::from_str(r#"{"x":1,"y":2,"width":3,"height":4}"#).unwrap();
        assert!(!s.maximized);
    }
}
