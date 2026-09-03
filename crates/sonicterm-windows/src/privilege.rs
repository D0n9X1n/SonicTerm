use sonicterm_app::ProcessPrivilege;
use windows::Win32::{
    Foundation::{CloseHandle, HANDLE},
    Security::{GetTokenInformation, TokenElevation, TOKEN_ELEVATION, TOKEN_QUERY},
    System::Threading::{GetCurrentProcess, OpenProcessToken},
};

struct TokenHandle(HANDLE);

// Lifecycle: `TokenHandle` Drop releases its owned token `HANDLE` with `CloseHandle` exactly once.
impl Drop for TokenHandle {
    fn drop(&mut self) {
        // SAFETY: `self.0` is the live handle returned by one successful `OpenProcessToken` call.
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

pub(crate) const fn process_privilege_from_token_elevation(value: u32) -> ProcessPrivilege {
    if value == 0 {
        ProcessPrivilege::Unprivileged
    } else {
        // When: `value != 0`, Windows reports an elevated process token.
        ProcessPrivilege::Privileged
    }
}

pub(crate) fn detect_process_privilege() -> windows::core::Result<ProcessPrivilege> {
    let mut token = HANDLE::default();
    // SAFETY: the pseudo-handle identifies this live process; `token` receives one owned query handle.
    unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token)? };
    let token = TokenHandle(token);
    let mut elevation = TOKEN_ELEVATION::default();
    let mut returned = 0;
    // SAFETY: `elevation` is writable for its declared byte length and `token` remains live through this call.
    unsafe {
        GetTokenInformation(
            token.0,
            TokenElevation,
            Some(std::ptr::from_mut(&mut elevation).cast()),
            std::mem::size_of::<TOKEN_ELEVATION>() as u32,
            &mut returned,
        )?;
    }
    Ok(process_privilege_from_token_elevation(elevation.TokenIsElevated))
}

#[cfg(test)]
#[path = "privilege_tests.rs"]
mod privilege_tests;
