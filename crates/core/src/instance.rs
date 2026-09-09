//! Single-instance guard: a named mutex in the session-local namespace so a
//! second `relay-core run` in the same session notices the first and exits.

use anyhow::Result;

/// Held for the lifetime of the service. Dropping it releases the name.
pub struct InstanceLock {
    #[cfg(windows)]
    handle: windows::Win32::Foundation::HANDLE,
}

// SAFETY: a mutex handle is a plain kernel handle; it is only closed in Drop.
unsafe impl Send for InstanceLock {}

impl InstanceLock {
    /// `Ok(None)` means another instance already holds the name.
    #[cfg(windows)]
    pub fn acquire(name: &str) -> Result<Option<Self>> {
        use windows::core::PCWSTR;
        use windows::Win32::Foundation::{CloseHandle, GetLastError, ERROR_ALREADY_EXISTS};
        use windows::Win32::System::Threading::CreateMutexW;

        let wide: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
        // SAFETY: `wide` is NUL-terminated and outlives the call.
        let handle = unsafe { CreateMutexW(None, false, PCWSTR(wide.as_ptr()))? };
        // SAFETY: reading the thread-local last-error right after the call.
        if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
            // SAFETY: we own this handle and never use it again.
            unsafe {
                let _ = CloseHandle(handle);
            }
            return Ok(None);
        }
        Ok(Some(Self { handle }))
    }

    #[cfg(not(windows))]
    pub fn acquire(_name: &str) -> Result<Option<Self>> {
        Ok(Some(Self {}))
    }
}

impl Drop for InstanceLock {
    fn drop(&mut self) {
        #[cfg(windows)]
        // SAFETY: handle was returned by CreateMutexW and is closed exactly once.
        unsafe {
            let _ = windows::Win32::Foundation::CloseHandle(self.handle);
        }
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    #[test]
    fn second_acquire_fails_while_first_is_held() {
        let name = format!(r"Local\RelayCoreTest-{}", uuid::Uuid::new_v4());
        let first = InstanceLock::acquire(&name).unwrap();
        assert!(first.is_some());
        assert!(InstanceLock::acquire(&name).unwrap().is_none(), "second must see the first");
        drop(first);
        assert!(InstanceLock::acquire(&name).unwrap().is_some(), "free again after drop");
    }
}
