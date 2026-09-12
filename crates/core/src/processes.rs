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

/// Every running process' exe name, whether or not it owns a window — the
/// uninstaller has to find `relay-ui.exe` even when its window is hidden.
#[cfg(windows)]
pub fn list_all() -> Vec<String> {
    pids_by_name(None).into_iter().map(|(_, exe)| exe).collect()
}

#[cfg(not(windows))]
pub fn list_all() -> Vec<String> {
    Vec::new()
}

/// Ask every process with this exe name to close, then terminate the ones
/// that are still up. The UI is a Tauri window with no state of its own, so
/// there is nothing to lose; the polite step is there so it can save window
/// geometry.
#[cfg(windows)]
pub fn terminate_by_name(exe: &str) -> anyhow::Result<()> {
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::System::Threading::{
        OpenProcess, TerminateProcess, WaitForSingleObject, PROCESS_TERMINATE,
    };

    let targets: Vec<u32> = pids_by_name(Some(exe)).into_iter().map(|(pid, _)| pid).collect();
    if targets.is_empty() {
        return Ok(());
    }
    close_windows_of(&targets);
    for pid in targets {
        // SAFETY: a pid and access flags in, a handle out; closed by `Owned`.
        unsafe {
            let Ok(h) = OpenProcess(PROCESS_TERMINATE, false, pid) else { continue };
            let h = windows::core::Owned::new(h);
            // 2 s for the WM_CLOSE to land before we insist.
            if WaitForSingleObject(*h, 2_000).0 == 0 {
                continue;
            }
            let _: HANDLE = *h;
            let _ = TerminateProcess(*h, 0);
        }
    }
    Ok(())
}

#[cfg(not(windows))]
pub fn terminate_by_name(_exe: &str) -> anyhow::Result<()> {
    Ok(())
}

/// Post `WM_CLOSE` to the top-level windows owned by `pids`.
#[cfg(windows)]
fn close_windows_of(pids: &[u32]) {
    use windows::core::BOOL;
    use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
    use windows::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GetWindowThreadProcessId, PostMessageW, WM_CLOSE,
    };

    unsafe extern "system" fn cb(hwnd: HWND, lparam: LPARAM) -> BOOL {
        // SAFETY: lparam is the pid slice we passed below.
        let pids = unsafe { &*(lparam.0 as *const &[u32]) };
        // SAFETY: plain window queries on a handle the OS handed us.
        unsafe {
            let mut pid = 0u32;
            GetWindowThreadProcessId(hwnd, Some(&mut pid));
            if pids.contains(&pid) {
                let _ = PostMessageW(Some(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0));
            }
        }
        BOOL(1)
    }

    let slice: &[u32] = pids;
    // SAFETY: `slice` outlives the synchronous enumeration.
    let _ = unsafe { EnumWindows(Some(cb), LPARAM(&slice as *const &[u32] as isize)) };
}

/// `(pid, exe name)` for every process, optionally filtered by name
/// (case-insensitive). Uses the toolhelp snapshot so it needs no per-process
/// access rights.
#[cfg(windows)]
fn pids_by_name(filter: Option<&str>) -> Vec<(u32, String)> {
    use windows::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
        TH32CS_SNAPPROCESS,
    };

    let mut out = Vec::new();
    // SAFETY: the snapshot handle is closed by `Owned`; PROCESSENTRY32W is
    // zeroed with its dwSize set as the API requires.
    unsafe {
        let Ok(snap) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) else { return out };
        let snap = windows::core::Owned::new(snap);
        let mut entry = PROCESSENTRY32W {
            dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
            ..Default::default()
        };
        if Process32FirstW(*snap, &mut entry).is_err() {
            return out;
        }
        loop {
            let n = entry.szExeFile.iter().position(|c| *c == 0).unwrap_or(entry.szExeFile.len());
            let exe = String::from_utf16_lossy(&entry.szExeFile[..n]);
            if filter.is_none_or(|f| exe.eq_ignore_ascii_case(f)) && entry.th32ProcessID != 0 {
                out.push((entry.th32ProcessID, exe));
            }
            if Process32NextW(*snap, &mut entry).is_err() {
                break;
            }
        }
    }
    out
}

/// This process can write HKLM (an HKLM write would not be refused by ACLs).
/// Display-only in the UI; the uninstaller uses it to decide whether it
/// needs a UAC round.
#[cfg(windows)]
pub fn is_elevated() -> bool {
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::Security::{
        GetTokenInformation, TokenElevation, TOKEN_ELEVATION, TOKEN_QUERY,
    };
    use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
    // SAFETY: standard token query on our own process; the handle is closed
    // by `Owned`'s drop.
    unsafe {
        let mut token = HANDLE::default();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token).is_err() {
            return false;
        }
        let token = windows::core::Owned::new(token);
        let mut elev = TOKEN_ELEVATION::default();
        let mut len = 0u32;
        GetTokenInformation(
            *token,
            TokenElevation,
            Some(&mut elev as *mut _ as *mut _),
            std::mem::size_of::<TOKEN_ELEVATION>() as u32,
            &mut len,
        )
        .map(|_| elev.TokenIsElevated != 0)
        .unwrap_or(false)
    }
}

#[cfg(not(windows))]
pub fn is_elevated() -> bool {
    false
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
