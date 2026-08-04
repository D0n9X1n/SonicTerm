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
//! 4. Resolve the chosen pid's image name via `QueryFullProcessImageNameW`.
//!
//! Returns `(pid, normalized_name)` so the caller has both the numeric id
//! (for follow-up calls like wait/signal) and a stable lowercase basename
//! to key the icon lookup off of.
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
use windows::Win32::System::Threading::{
    OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_FORMAT, PROCESS_QUERY_LIMITED_INFORMATION,
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

/// Best-effort `(pid, normalized_name)` of the deepest descendant of
/// `pty_pid`. Returns `None` if the snapshot can't be taken or no
/// descendant is found (in which case the caller should fall back to the
/// shell's own name).
pub fn current_foreground_pid(pty_pid: u32) -> Option<(u32, String)> {
    let snapshot = snapshot_processes()?;
    let leaf = pick_deepest_leaf(&snapshot, pty_pid)?;
    let name = resolve_process_name(leaf)?;
    Some((leaf, normalize_proc_name(&name)))
}

struct ProcEntry {
    pid: u32,
    parent: u32,
    create_time: i64,
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

fn pick_deepest_leaf(snapshot: &[ProcEntry], root: u32) -> Option<u32> {
    // children[parent] -> Vec<pid>
    let mut children: HashMap<u32, Vec<u32>> = HashMap::with_capacity(snapshot.len());
    let mut by_pid: HashMap<u32, &ProcEntry> = HashMap::with_capacity(snapshot.len());
    for entry in snapshot {
        children.entry(entry.parent).or_default().push(entry.pid);
        by_pid.insert(entry.pid, entry);
    }

    // BFS from `root`, tracking (deepest depth, most-recent create_time at
    // that depth, chosen pid). Includes `root` itself as a fallback so that
    // a shell with no children still resolves to its own pid — callers can
    // then decide whether to bother showing the shell name.
    let mut chosen = root;
    let mut chosen_depth: usize = 0;
    let mut chosen_ctime: i64 = by_pid.get(&root).map(|e| e.create_time).unwrap_or(0);

    let mut frontier: Vec<(u32, usize)> = vec![(root, 0)];
    while let Some((cur, depth)) = frontier.pop() {
        let kids = children.get(&cur).map(|v| v.as_slice()).unwrap_or(&[]);
        if kids.is_empty() {
            // leaf — candidate
            let ctime = by_pid.get(&cur).map(|e| e.create_time).unwrap_or(0);
            let better = depth > chosen_depth
                || (depth == chosen_depth && ctime > chosen_ctime && cur != root);
            if better {
                chosen = cur;
                chosen_depth = depth;
                chosen_ctime = ctime;
            }
        } else {
            // When: `kids` is nonempty, `cur` cannot be the leaf chosen as the foreground descendant.
            for &k in kids {
                if k == cur || k == 0 {
                    // When: `k` equal to `cur` would cycle; pid 0 is the idle process, never a runnable descendant.
                    continue;
                }
                frontier.push((k, depth + 1));
            }
        }
    }

    if chosen == root && chosen_depth == 0 {
        // When: `chosen` never moved off `root`, so report the shell pid rather than no process.
        return Some(root);
    }
    Some(chosen)
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
