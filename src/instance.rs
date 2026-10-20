//! One OpenNC per login session. The first launch owns a named mutex; a later launch pokes a named event so the
//! running one shows its window, then exits. Nothing else travels between instances.

#[cfg(windows)]
mod imp {
    use windows_sys::Win32::Foundation::{GetLastError, SetLastError, ERROR_ALREADY_EXISTS, HANDLE, WAIT_OBJECT_0};
    use windows_sys::Win32::System::Threading::{CreateEventW, CreateMutexW, SetEvent, WaitForSingleObject};

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    /// Held for the life of the process; the handles are never closed on purpose.
    pub struct Guard {
        _mutex: HANDLE,
        event: HANDLE,
    }

    /// `Some` if this is the only instance. `None` means another one is running: it has been asked to show its
    /// window when `show` is set, and the caller should exit.
    pub fn acquire(show: bool) -> Option<Guard> {
        unsafe {
            SetLastError(0);
            let mutex = CreateMutexW(std::ptr::null(), 0, wide("Local\\OpenNC.instance").as_ptr());
            let second = !mutex.is_null() && GetLastError() == ERROR_ALREADY_EXISTS;
            let event = CreateEventW(std::ptr::null(), 0, 0, wide("Local\\OpenNC.show").as_ptr()); // auto-reset
            if second {
                if show && !event.is_null() {
                    SetEvent(event);
                }
                None
            } else {
                Some(Guard { _mutex: mutex, event })
            }
        }
    }

    impl Guard {
        /// True once for every later launch that asked for the window.
        pub fn show_requested(&self) -> bool {
            !self.event.is_null() && unsafe { WaitForSingleObject(self.event, 0) } == WAIT_OBJECT_0
        }
    }
}

#[cfg(not(windows))]
mod imp {
    // ponytail: no guard off Windows until there is a packaged build there; a lock file under the config dir then.
    pub struct Guard;
    pub fn acquire(_show: bool) -> Option<Guard> {
        Some(Guard)
    }
    impl Guard {
        pub fn show_requested(&self) -> bool {
            false
        }
    }
}

pub use imp::*;
