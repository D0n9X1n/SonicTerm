//! Windows path probes and direct-open. Classification blocks reparse points and filenames with
//! alternate-stream or normalization-sensitive syntax; files are selected in Explorer and
//! directories open through `ShellExecuteExW`. The module is declared as `windows_os` so the
//! `windows::` paths here keep naming the `windows` crate.

use super::*;

#[cfg(target_os = "windows")]
pub(super) fn classify_local_target(path: &Path) -> PathOpenDecision {
    classify_windows_target(path)
}

#[cfg(any(target_os = "windows", test))]
fn windows_file_name(path: &Path) -> Option<&str> {
    path.to_str()?.rsplit(['/', '\\']).next().filter(|name| !name.is_empty())
}

#[cfg(any(target_os = "windows", test))]
pub(super) fn windows_path_policy(path: &Path) -> PathOpenDecision {
    let Some(name) = windows_file_name(path) else {
        // When: `windows_file_name` cannot produce one nonempty component, reject an unclassifiable ShellExecute target.
        return PathOpenDecision::Blocked;
    };
    if name.contains(':')
        || name.ends_with(['.', ' '])
        || name.chars().any(|ch| ch.is_control() || matches!(ch, '<' | '>' | '"' | '|' | '?' | '*'))
    {
        // When: `name` contains ADS or reserved syntax, block Windows normalization and alternate-stream ambiguity.
        return PathOpenDecision::Blocked;
    }
    PathOpenDecision::Openable(PathKind::File)
}

#[cfg(target_os = "windows")]
fn classify_windows_target(path: &Path) -> PathOpenDecision {
    use std::os::windows::fs::MetadataExt;

    if let Err(decision) = validate_local_ancestors(path) {
        // When: validate_local_ancestors detects reparse or missing identity, do not dispatch through that path.
        return decision;
    }

    let (metadata, kind) = match classify_nonsymlink_metadata(path) {
        Ok(classified) => classified,
        Err(decision) => {
            // When: `classify_nonsymlink_metadata` returns `Err`, retain its missing-or-blocked decision unchanged.
            return decision;
        }
    };
    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
    if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        // When: `metadata.file_attributes()` contains `FILE_ATTRIBUTE_REPARSE_POINT`, block redirected identity.
        return PathOpenDecision::Blocked;
    }
    if kind == PathKind::Directory && path.parent().is_none() {
        // When: kind is Directory at the drive root, no final filename exists for extension policy.
        return PathOpenDecision::Openable(PathKind::Directory);
    }
    if windows_path_policy(path).is_blocked() {
        PathOpenDecision::Blocked
    } else {
        // When: `windows_path_policy` does not block `path`, restore the metadata-derived file-or-directory `kind`.
        PathOpenDecision::Openable(kind)
    }
}

#[cfg(target_os = "windows")]
pub(super) fn reveal_native_file(path: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows::core::PCWSTR;
    use windows::Win32::System::Com::{
        CoInitializeEx, CoTaskMemFree, CoUninitialize, COINIT_APARTMENTTHREADED,
    };
    use windows::Win32::UI::Shell::{SHOpenFolderAndSelectItems, SHParseDisplayName};
    let target = path.as_os_str().encode_wide().chain(Some(0)).collect::<Vec<_>>();
    // SAFETY: this worker owns its COM apartment; target and PIDL remain live until selection returns, then are released once.
    unsafe {
        CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok().map_err(io::Error::other)?;
        let mut pidl = std::ptr::null_mut();
        let result = SHParseDisplayName(PCWSTR(target.as_ptr()), None, &mut pidl, 0, None)
            .and_then(|()| SHOpenFolderAndSelectItems(pidl, None, 0));
        CoTaskMemFree(Some(pidl.cast()));
        CoUninitialize();
        result.map_err(io::Error::other)
    }
}

#[cfg(target_os = "windows")]
pub(super) fn open_native_path(path: &Path, expected_decision: PathOpenDecision) -> io::Result<()> {
    let PathOpenDecision::Openable(expected_kind @ PathKind::Directory) = expected_decision else {
        // When: expected_decision is not a directory, the navigation dispatcher must never launch a file.
        return Err(io::Error::new(io::ErrorKind::PermissionDenied, "unsupported Windows action"));
    };
    use std::os::windows::ffi::OsStrExt;
    use windows::core::PCWSTR;
    use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_APARTMENTTHREADED};
    use windows::Win32::UI::Shell::{ShellExecuteExW, SEE_MASK_NOASYNC, SHELLEXECUTEINFOW};
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

    if classify_windows_target(path) != PathOpenDecision::Openable(expected_kind) {
        // When: `classify_windows_target` no longer returns `expected_kind`, reject a changed or newly blocked target.
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "changed or blocked Windows target",
        ));
    }
    let verb = "open\0".encode_utf16().collect::<Vec<_>>();
    let target = path.as_os_str().encode_wide().chain(Some(0)).collect::<Vec<_>>();
    // SAFETY: this dedicated worker owns its COM apartment; all UTF-16 buffers
    // remain live through the synchronous SEE_MASK_NOASYNC call.
    unsafe {
        CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok().map_err(io::Error::other)?;
        let mut info = SHELLEXECUTEINFOW {
            cbSize: u32::try_from(std::mem::size_of::<SHELLEXECUTEINFOW>()).unwrap_or(u32::MAX),
            fMask: SEE_MASK_NOASYNC,
            lpVerb: PCWSTR(verb.as_ptr()),
            lpFile: PCWSTR(target.as_ptr()),
            nShow: SW_SHOWNORMAL.0,
            ..Default::default()
        };
        let result = ShellExecuteExW(&mut info).map_err(io::Error::other);
        CoUninitialize();
        result
    }
}

#[cfg(test)]
#[path = "windows_tests.rs"]
mod windows_tests;
