//! Resolve the current process user's SID into SDDL string form
//! (e.g. `"S-1-5-21-..."`).
//!
//! Issue #510: the harness named-pipe was using the SDDL alias `OW`
//! ("owner of object") in `O:OW...`, which Windows rejects with
//! `ERROR_INVALID_OWNER (1307)` at pipe-creation time. Per Step-2
//! APPROVED-DIAG we must substitute a *concrete* user SID resolved
//! from the current process token before passing the SDDL to
//! `ConvertStringSecurityDescriptorToSecurityDescriptorW`.
//!
//! The resolved string is cached for process lifetime via `OnceLock`
//! — the SID can't change without spawning a different process, and
//! we don't want to re-syscall every pipe rebuild.

#![cfg(target_os = "windows")]

use std::sync::OnceLock;

use windows::core::PWSTR;
use windows::Win32::Foundation::{CloseHandle, LocalFree, HLOCAL};
use windows::Win32::Security::Authorization::ConvertSidToStringSidW;
use windows::Win32::Security::{GetTokenInformation, TokenOwner, TOKEN_OWNER, TOKEN_QUERY};
use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

/// Resolve the current process user's SID to its SDDL string form
/// (e.g. `"S-1-5-21-..."`). Allocates a transient `TOKEN_OWNER`
/// buffer and frees the `LocalAlloc`-owned string returned by
/// `ConvertSidToStringSidW` before returning.
pub fn current_user_sid_string() -> windows::core::Result<String> {
    unsafe {
        // 1. Open this process's access token for query.
        let mut token = windows::Win32::Foundation::HANDLE::default();
        OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token)?;

        // 2. First call: discover the buffer size for TokenOwner.
        let mut needed: u32 = 0;
        let _ = GetTokenInformation(token, TokenOwner, None, 0, &mut needed);
        if needed == 0 {
            let _ = CloseHandle(token);
            return Err(windows::core::Error::from_thread());
        }

        // 3. Second call: fill the TOKEN_OWNER buffer.
        let mut buf = vec![0u8; needed as usize];
        let res = GetTokenInformation(
            token,
            TokenOwner,
            Some(buf.as_mut_ptr() as *mut _),
            needed,
            &mut needed,
        );
        if let Err(e) = res {
            let _ = CloseHandle(token);
            return Err(e);
        }

        // TOKEN_OWNER is `{ Owner: PSID }`.
        let token_owner = &*(buf.as_ptr() as *const TOKEN_OWNER);

        // 4. Convert the PSID to its SDDL string form. Windows
        // allocates the buffer via LocalAlloc; we own freeing it.
        let mut sid_string = PWSTR::null();
        let convert = ConvertSidToStringSidW(token_owner.Owner, &mut sid_string);
        // Token no longer needed after we've copied data out.
        let _ = CloseHandle(token);
        convert?;

        // Copy the wide string into a Rust String.
        let mut len = 0usize;
        while *sid_string.0.add(len) != 0 {
            len += 1;
        }
        let slice = std::slice::from_raw_parts(sid_string.0, len);
        let s = String::from_utf16(slice).map_err(|_| windows::core::Error::from_thread());

        // Free the LocalAlloc'd buffer regardless of UTF-16 outcome.
        let _ = LocalFree(Some(HLOCAL(sid_string.0 as *mut _)));

        s
    }
}

/// Cached, process-lifetime SID string for the current user. Panics
/// at first call only if the token query fails — at which point the
/// harness pipe is unusable anyway, and a clear panic is preferable
/// to producing an invalid SDDL silently.
pub fn cached_current_user_sid() -> &'static str {
    static CACHED: OnceLock<String> = OnceLock::new();
    CACHED.get_or_init(|| {
        current_user_sid_string().expect("failed to resolve current user SID for harness pipe")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sid_string_format() {
        let sid = current_user_sid_string().unwrap();
        assert!(sid.starts_with("S-1-"), "got {sid}");
    }

    #[test]
    fn cached_matches_fresh() {
        let fresh = current_user_sid_string().unwrap();
        assert_eq!(cached_current_user_sid(), fresh);
    }
}
