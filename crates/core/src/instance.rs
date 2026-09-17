//! Single-instance guard: a named mutex in the session-local namespace so a
//! second `relay-core run` in the same session notices the first and exits.

use anyhow::Result;

/// Held for the lifetime of the service. Dropping it releases the name.
pub struct InstanceLock {
    _held: imp::Held,
}

impl InstanceLock {
    /// `Ok(None)` means another instance already holds the name.
    pub fn acquire(name: &str) -> Result<Option<Self>> {
        Ok(imp::acquire(name)?.map(|held| Self { _held: held }))
    }
}

#[cfg(windows)]
mod imp {
    use anyhow::Result;
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::{CloseHandle, GetLastError, ERROR_ALREADY_EXISTS, HANDLE};
    use windows::Win32::System::Threading::CreateMutexW;

    pub struct Held(HANDLE);

    // SAFETY: a mutex handle is a plain kernel handle; it is only closed in Drop.
    unsafe impl Send for Held {}

    pub fn acquire(name: &str) -> Result<Option<Held>> {
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
        Ok(Some(Held(handle)))
    }

    impl Drop for Held {
        fn drop(&mut self) {
            // SAFETY: handle was returned by CreateMutexW and is closed exactly once.
            unsafe {
                let _ = CloseHandle(self.0);
            }
        }
    }
}

/// Stub: always acquires. Nothing that could run twice (the IPC server, the
/// event loop) exists on this platform yet; a port uses a lock file
/// (`flock`) in the data root.
#[cfg(not(windows))]
mod imp {
    pub struct Held;

    pub fn acquire(_name: &str) -> anyhow::Result<Option<Held>> {
        Ok(Some(Held))
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
