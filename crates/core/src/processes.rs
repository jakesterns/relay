//! Processes that own a visible top-level window, for the exe picker in the
//! profile form. Only `PROCESS_QUERY_LIMITED_INFORMATION` is requested; no
//! memory or handle access.

use crate::types::ProcessInfo;

#[cfg(windows)]
pub fn list_windowed() -> Vec<ProcessInfo> {
    use std::collections::BTreeMap;

    use windows::core::BOOL;
    use windows::Win32::Foundation::{HWND, LPARAM};
    use windows::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GetWindow, GetWindowTextW, GetWindowThreadProcessId, IsWindowVisible, GW_OWNER,
    };

    struct Acc {
        seen: BTreeMap<u32, ProcessInfo>,
        own_pid: u32,
    }

    unsafe extern "system" fn cb(hwnd: HWND, lparam: LPARAM) -> BOOL {
        // SAFETY: lparam is the `Acc` we passed to EnumWindows below.
        let acc = unsafe { &mut *(lparam.0 as *mut Acc) };
        // SAFETY: plain window queries on a handle the OS just handed us.
        unsafe {
            if !IsWindowVisible(hwnd).as_bool() || GetWindow(hwnd, GW_OWNER).is_ok() {
                return BOOL(1);
            }
            let mut pid = 0u32;
            GetWindowThreadProcessId(hwnd, Some(&mut pid));
            if pid == 0 || pid == acc.own_pid || acc.seen.contains_key(&pid) {
                return BOOL(1);
            }
            let mut buf = [0u16; 256];
            let n = GetWindowTextW(hwnd, &mut buf) as usize;
            let title = String::from_utf16_lossy(&buf[..n]);
            if title.is_empty() {
                return BOOL(1);
            }
            let Some(path) = crate::winloop::process_image_path(pid) else { return BOOL(1) };
            let exe = crate::winloop::exe_name(&path);
            if exe.is_empty() {
                return BOOL(1);
            }
            acc.seen.insert(pid, ProcessInfo { pid, exe, title, hwnd: hwnd.0 as usize as u64 });
        }
        BOOL(1)
    }

    let mut acc = Acc { seen: BTreeMap::new(), own_pid: std::process::id() };
    // SAFETY: `acc` outlives the synchronous enumeration.
    let _ = unsafe { EnumWindows(Some(cb), LPARAM(&mut acc as *mut Acc as isize)) };
    let mut out: Vec<ProcessInfo> = acc.seen.into_values().collect();
    out.sort_by(|a, b| a.exe.to_lowercase().cmp(&b.exe.to_lowercase()).then(a.pid.cmp(&b.pid)));
    out
}

#[cfg(not(windows))]
pub fn list_windowed() -> Vec<ProcessInfo> {
    Vec::new()
}

#[cfg(all(test, windows))]
mod tests {
    #[test]
    fn enumeration_does_not_panic_and_excludes_self() {
        let me = std::process::id();
        let list = super::list_windowed();
        assert!(list.iter().all(|p| p.pid != me));
        assert!(list.iter().all(|p| !p.exe.is_empty()));
    }
}
