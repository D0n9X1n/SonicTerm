//! Windows foreground-process probe.
//!
//! Equivalent of the macOS `libproc` walk in `proc_info::macos`: given the
//! pid of the shell at the bottom of a pty, find the deepest descendant
//! that's currently running. That's the process the user actually has on-
//! screen (e.g. `nvim`, `cargo`, `ssh`) and is what the tab-title icon /
//! label want to display.
//!
//! Strategy:
//! 1. Snapshot the whole process table with
//!    `NtQuerySystemInformation(SystemProcessInformation, ...)`.
//!    This returns a packed linked-list of `SYSTEM_PROCESS_INFORMATION`
//!    records; each carries the pid, parent pid (`InheritedFrom...`), and
//!    a `CreateTime` we use to break ties between sibling leaves.
//! 2. Build a parent → children map.
//! 3. BFS from `pty_pid`. Track the deepest leaf (no children); on ties
//!    by depth, prefer the one with the **most recent** CreateTime — that
//!    matches what the user just launched.
//! 4. Resolve the chosen pid's image name via `QueryFullProcessImageNameW`
//!    and inspect its token elevation. A batch entry point reuses one process
//!    snapshot and ancestry index for every visible tab that needs a refresh.
//!
//! Returns typed process identity and privilege state; the preserved
//! `(pid, normalized_name)` API remains available to identity-only callers.
//!
//! Failures (process gone, ACL denies query, ntdll returns an unexpected
//! status, etc.) all collapse to `None`; the tab title just falls back to
//! the shell name in that case.

#![cfg(windows)]

use std::collections::HashMap;
use std::ffi::c_void;
use std::mem::MaybeUninit;

use windows::Wdk::System::SystemInformation::NtQuerySystemInformation;
use windows::Win32::Foundation::{CloseHandle, HANDLE, NTSTATUS, STATUS_INFO_LENGTH_MISMATCH};
use windows::Win32::Security::{GetTokenInformation, TokenElevation, TOKEN_ELEVATION, TOKEN_QUERY};
use windows::Win32::System::Threading::{
    OpenProcess, OpenProcessToken, QueryFullProcessImageNameW, PROCESS_NAME_FORMAT,
    PROCESS_QUERY_LIMITED_INFORMATION,
};

use crate::proc_info::normalize_proc_name;

/// SystemProcessInformation = 5; passed as the class argument to
/// `NtQuerySystemInformation`. Defined inline to keep our windows-rs
/// feature surface small (the `SYSTEM_INFORMATION_CLASS` newtype is in the
/// Wdk module too, but only this one value matters for us).
const SYSTEM_PROCESS_INFORMATION_CLASS: i32 = 5;

/// Subset of `SYSTEM_PROCESS_INFORMATION` we actually read. The real struct
/// is large and version-dependent but the prefix is stable since NT 4 and
/// the fields we touch all sit at fixed offsets. We rely on `NextEntryOffset`
/// to step over whatever trailing fields the current kernel adds.
#[repr(C)]
struct SystemProcessInformation {
    next_entry_offset: u32,
    number_of_threads: u32,
    _reserved1: [i64; 3],
    create_time: i64,
    _user_time: i64,
    _kernel_time: i64,
    // UNICODE_STRING ImageName — 16 bytes on 64-bit (u16 len, u16 max_len,
    // 4 bytes pad, *u16 buffer). We don't use it here (we resolve via
    // QueryFullProcessImageNameW for path normalization), so we just skip
    // the right number of bytes.
    _image_name_length: u16,
    _image_name_max_len: u16,
    _image_name_pad: u32,
    _image_name_buffer: *mut u16,
    _base_priority: i32,
    _pad_priority: u32,
    unique_process_id: usize,
    inherited_from_unique_process_id: usize,
    // remaining fields ignored — NextEntryOffset takes us to the next record
}

const _: () =
    assert!(std::mem::align_of::<u64>() >= std::mem::align_of::<SystemProcessInformation>());

/// Foreground Windows process identity and privilege presentation state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForegroundProcess {
    /// Process id selected by the descendant walk.
    pub pid: u32,
    /// Normalized executable basename used by tab-title icon matching.
    pub name: String,
    /// Whether this foreground process requires the privilege warning.
    pub privileged: bool,
}

/// Best-effort foreground process at the deepest descendant of `pty_pid`.
///
/// Returns `None` if the snapshot cannot be taken or its selected process name
/// cannot be resolved. Token-query failure remains an unknown direct state;
/// `gsudo` itself still requires the warning because its regular client can
/// broker a high-integrity descendant that UIPI prevents the parent from
/// querying directly.
pub fn current_foreground_process(pty_pid: u32) -> Option<ForegroundProcess> {
    current_foreground_processes(&[pty_pid]).pop().flatten()
}

/// Best-effort foreground processes for several pty shell pids from one snapshot.
///
/// Results retain input order and contain `None` independently when one shell or
/// selected descendant disappears while the shared snapshot is being resolved.
pub fn current_foreground_processes(pty_pids: &[u32]) -> Vec<Option<ForegroundProcess>> {
    if pty_pids.is_empty() {
        // When: `pty_pids.is_empty()`, avoid taking a system-wide process snapshot.
        return Vec::new();
    }
    let Some(snapshot) = snapshot_processes() else {
        // When: the shared process snapshot fails, no input pid has an observable descendant.
        return vec![None; pty_pids.len()];
    };
    let index = ProcessIndex::new(&snapshot);
    pty_pids.iter().map(|pty_pid| foreground_process_from_index(&index, *pty_pid)).collect()
}

fn foreground_process_from_index(
    index: &ProcessIndex<'_>,
    pty_pid: u32,
) -> Option<ForegroundProcess> {
    let leaf = index.pick_deepest_leaf(pty_pid)?;
    let path = index.path_to_root(pty_pid, leaf).unwrap_or_else(|| vec![leaf]);
    let names: Vec<(u32, String)> = path
        .iter()
        .filter_map(|pid| resolve_process_name(*pid).map(|name| (*pid, normalize_proc_name(&name))))
        .collect();
    let name = names.first()?.1.clone();
    let gsudo_ancestor = names.iter().any(|(_, name)| name == "gsudo");
    let privileged =
        foreground_process_is_privileged(&name, process_token_is_elevated(leaf), gsudo_ancestor);
    Some(ForegroundProcess { pid: leaf, name, privileged })
}

/// Best-effort `(pid, normalized_name)` of the deepest descendant of `pty_pid`.
///
/// Preserved for callers that need process identity but not privilege state.
pub fn current_foreground_pid(pty_pid: u32) -> Option<(u32, String)> {
    let foreground = current_foreground_process(pty_pid)?;
    Some((foreground.pid, foreground.name))
}

fn foreground_process_is_privileged(
    name: &str,
    token_elevated: Option<bool>,
    gsudo_ancestor: bool,
) -> bool {
    token_elevated == Some(true) || gsudo_ancestor || normalize_proc_name(name) == "gsudo"
}

const fn token_elevation_value_is_privileged(value: u32) -> bool {
    value != 0
}

fn process_token_is_elevated(pid: u32) -> Option<bool> {
    let process =
        // SAFETY: `OpenProcess` receives only values and returns an owned query handle closed below.
        unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }.ok()?;
    let mut token = HANDLE::default();
    let opened =
        // SAFETY: `process` is live and `token` points to writable storage for one owned handle.
        unsafe { OpenProcessToken(process, TOKEN_QUERY, &mut token) };
    let _ =
        // SAFETY: `process` is still owned and has not been closed since `OpenProcess` returned it.
        unsafe { CloseHandle(process) };
    opened.ok()?;

    let mut elevation = TOKEN_ELEVATION::default();
    let mut returned = 0;
    let result =
        // SAFETY: `token` is live and `elevation` is writable for the declared byte length.
        unsafe {
            GetTokenInformation(
                token,
                TokenElevation,
                Some(std::ptr::from_mut(&mut elevation).cast()),
                std::mem::size_of::<TOKEN_ELEVATION>() as u32,
                &mut returned,
            )
        };
    let _ =
        // SAFETY: `token` is still owned and has not been closed since `OpenProcessToken` returned it.
        unsafe { CloseHandle(token) };
    result.ok()?;
    Some(token_elevation_value_is_privileged(elevation.TokenIsElevated))
}

struct ProcEntry {
    pid: u32,
    parent: u32,
    create_time: i64,
}

struct ProcessIndex<'a> {
    children: HashMap<u32, Vec<u32>>,
    by_pid: HashMap<u32, &'a ProcEntry>,
}

impl<'a> ProcessIndex<'a> {
    fn new(snapshot: &'a [ProcEntry]) -> Self {
        let mut children = HashMap::with_capacity(snapshot.len());
        let mut by_pid = HashMap::with_capacity(snapshot.len());
        for entry in snapshot {
            children.entry(entry.parent).or_insert_with(Vec::new).push(entry.pid);
            by_pid.insert(entry.pid, entry);
        }
        Self { children, by_pid }
    }

    fn path_to_root(&self, root: u32, leaf: u32) -> Option<Vec<u32>> {
        let mut path = Vec::new();
        let mut current = leaf;
        for _ in 0..=self.by_pid.len() {
            path.push(current);
            if current == root {
                // When: `current == root`, the selected ancestry reached this pane's PTY shell.
                return Some(path);
            }
            let parent = self.by_pid.get(&current)?.parent;
            if parent == current || parent == 0 {
                // When: `parent == current || parent == 0`, this ancestry cannot reach the PTY root.
                return None;
            }
            current = parent;
        }
        None
    }

    fn pick_deepest_leaf(&self, root: u32) -> Option<u32> {
        let mut chosen = root;
        let mut chosen_depth = 0usize;
        let mut chosen_ctime = self.by_pid.get(&root).map(|entry| entry.create_time).unwrap_or(0);

        let mut frontier = vec![(root, 0usize)];
        while let Some((current, depth)) = frontier.pop() {
            let children = self.children.get(&current).map(Vec::as_slice).unwrap_or(&[]);
            if children.is_empty() {
                let create_time =
                    self.by_pid.get(&current).map(|entry| entry.create_time).unwrap_or(0);
                let better = depth > chosen_depth
                    || (depth == chosen_depth && create_time > chosen_ctime && current != root);
                if better {
                    chosen = current;
                    chosen_depth = depth;
                    chosen_ctime = create_time;
                }
            } else {
                // When: `children` is nonempty, `current` cannot be the selected leaf.
                for &child in children {
                    if child == current || child == 0 {
                        // When: `child` equal to `current` would cycle; pid 0 is never runnable.
                        continue;
                    }
                    frontier.push((child, depth + 1));
                }
            }
        }

        Some(chosen)
    }
}

/// Bound the STATUS_INFO_LENGTH_MISMATCH retry loop so a pathologically
/// racing process table (or a buggy kernel) can't keep us spinning forever.
/// 8 doublings from 1 MiB caps growth at 128 MiB before we'd give up; the
/// explicit byte cap below (`MAX_BUFFER_BYTES`) clamps individual grows
/// earlier than that.
const MAX_RETRIES: u32 = 8;
/// Hard ceiling on the snapshot buffer. 64 MiB comfortably fits the largest
/// real-world Windows process tables (~10k procs × ~1 KiB record) with
/// headroom; anything bigger is almost certainly a runaway.
const MAX_BUFFER_BYTES: usize = 64 * 1024 * 1024;

fn snapshot_processes() -> Option<Vec<ProcEntry>> {
    // Grow the buffer until ntdll stops complaining. Start at 1 MiB which is
    // enough for typical workstations (~600 procs × ~1 KiB record).
    let mut buf: Vec<u64> = vec![0u64; (1024 * 1024) / std::mem::size_of::<u64>()];
    for _attempt in 0..MAX_RETRIES {
        let mut return_length: u32 = 0;
        let status: NTSTATUS =
            // SAFETY: `buf` and `return_length` are writable for their supplied byte sizes; class 5 accepts these pointers for this call.
            unsafe {
                NtQuerySystemInformation(
                    windows::Wdk::System::SystemInformation::SYSTEM_INFORMATION_CLASS(
                        SYSTEM_PROCESS_INFORMATION_CLASS,
                    ),
                    buf.as_mut_ptr().cast::<c_void>(),
                    (buf.len() * std::mem::size_of::<u64>()) as u32,
                    &mut return_length as *mut u32,
                )
            };
        if status == STATUS_INFO_LENGTH_MISMATCH {
            // When: `status` is STATUS_INFO_LENGTH_MISMATCH; grow past the hint because the process table can race larger between calls.
            let current_bytes = buf.len().saturating_mul(std::mem::size_of::<u64>());
            let requested = (return_length as usize).max(current_bytes.saturating_mul(2));
            let new_size = requested.min(MAX_BUFFER_BYTES);
            if new_size <= current_bytes {
                // When: `new_size` is capped and cannot exceed `current_bytes`, so retrying would repeat the same mismatch.
                return None;
            }
            buf.resize(new_size.div_ceil(std::mem::size_of::<u64>()), 0);
            continue;
        }
        if status.is_ok() {
            // When: only a successful status makes the kernel-filled snapshot valid to parse.
            return Some(parse_snapshot(&buf));
        }
        return None;
    }
    // Retry budget exhausted without ever getting a successful snapshot.
    None
}

fn parse_snapshot(buf: &[u64]) -> Vec<ProcEntry> {
    let mut out = Vec::with_capacity(512);
    let mut offset: usize = 0;
    let byte_len = std::mem::size_of_val(buf);
    while offset + std::mem::size_of::<SystemProcessInformation>() <= byte_len {
        let record_ptr =
            // SAFETY: the loop bound keeps `offset` and the declared prefix inside `buf`; field loads below are unaligned.
            unsafe { buf.as_ptr().cast::<u8>().add(offset).cast::<SystemProcessInformation>() };
        let next =
            // SAFETY: `record_ptr` contains this prefix field; `read_unaligned` handles the kernel record offset.
            unsafe { std::ptr::addr_of!((*record_ptr).next_entry_offset).read_unaligned() } as usize;
        let pid =
            // SAFETY: `record_ptr` contains this prefix field; `read_unaligned` handles the kernel record offset.
            unsafe { std::ptr::addr_of!((*record_ptr).unique_process_id).read_unaligned() } as u32;
        let parent =
            // SAFETY: `record_ptr` contains this prefix field; `read_unaligned` handles the kernel record offset.
            unsafe {
                std::ptr::addr_of!((*record_ptr).inherited_from_unique_process_id).read_unaligned()
            } as u32;
        let create_time =
            // SAFETY: `record_ptr` contains this prefix field; `read_unaligned` handles the kernel record offset.
            unsafe { std::ptr::addr_of!((*record_ptr).create_time).read_unaligned() };
        out.push(ProcEntry { pid, parent, create_time });
        if next == 0 {
            // When: `next` zero marks the final packed record, so advancing would revisit this entry.
            break;
        }
        offset = offset.saturating_add(next);
    }
    out
}

#[cfg(test)]
fn path_to_root(snapshot: &[ProcEntry], root: u32, leaf: u32) -> Option<Vec<u32>> {
    ProcessIndex::new(snapshot).path_to_root(root, leaf)
}

#[cfg(test)]
fn pick_deepest_leaf(snapshot: &[ProcEntry], root: u32) -> Option<u32> {
    ProcessIndex::new(snapshot).pick_deepest_leaf(root)
}

fn resolve_process_name(pid: u32) -> Option<String> {
    // PROCESS_QUERY_LIMITED_INFORMATION works against protected processes
    // and across UAC boundaries where the heavier query-information right
    // would be denied.
    let handle: HANDLE =
        // SAFETY: `OpenProcess` takes only values here and returns an owned handle that this function closes.
        unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }.ok()?;
    let mut buf: [MaybeUninit<u16>; 1024] = [MaybeUninit::uninit(); 1024];
    let mut size: u32 = buf.len() as u32;
    let result =
        // SAFETY: `handle` is live, and `buf` is writable for the `size` units supplied to the API.
        unsafe {
            QueryFullProcessImageNameW(
                handle,
                PROCESS_NAME_FORMAT(0),
                windows::core::PWSTR(buf.as_mut_ptr() as *mut u16),
                &mut size as *mut u32,
            )
        };
    let _ =
        // SAFETY: `handle` is still owned and has not been closed since `OpenProcess` returned it.
        unsafe { CloseHandle(handle) };
    if result.is_err() || size == 0 {
        // When: `result` failed or `size` is zero, so no initialized image path can be normalized.
        return None;
    }
    let slice =
        // SAFETY: success wrote `size` code units plus a NUL; `size` excludes the NUL, so this slice is initialized.
        unsafe { std::slice::from_raw_parts(buf.as_ptr() as *const u16, size as usize) };
    Some(String::from_utf16_lossy(slice))
}

#[cfg(test)]
#[path = "foreground_proc_tests.rs"]
mod foreground_proc_tests;
