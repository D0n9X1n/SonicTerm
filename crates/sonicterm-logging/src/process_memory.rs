//! What the operating system says this process is holding.
//!
//! Every other memory figure SonicTerm reports is something it counted itself:
//! a pane sums its seams, a renderer reads its atlas capacities. Those answer
//! "what did SonicTerm allocate on purpose", which is a different question
//! from "how big is this process", and the gap between the two is where a
//! reported multi-gigabyte session lives. Allocator fragmentation, retired
//! pages the allocator has not returned, mapped files, GPU driver mappings,
//! and thread stacks are all in the process and in none of the seams.
//!
//! ## Three figures, because they fail differently
//!
//! - **private/committed** — memory that cannot be shared or reclaimed by
//!   dropping a clean page. This is what an out-of-memory kill is usually
//!   applied against.
//! - **resident / working set** — pages actually in physical memory now.
//!   Falls under memory pressure without anything being freed, so it drops
//!   for reasons that are not fixes.
//! - **virtual** — reserved address space. Routinely enormous and routinely
//!   harmless; a reader who mistakes it for consumption concludes a 400 GB
//!   leak from a healthy process.
//!
//! Reporting one of them alone invites exactly the wrong conclusion, which is
//! why all three are carried even where a platform can only produce some.
//!
//! ## Unsupported is a value, not a zero
//!
//! A figure this platform cannot produce is reported as
//! [`MemoryMetric::Unsupported`] and never as `0`. The two are not
//! interchangeable in the report a user reads: zero private bytes is a
//! measurement that would be alarming, while "unsupported" is a statement
//! about the API SonicTerm builds against. Collapsing them would put an
//! invented measurement in a diagnostic whose only purpose is to be trusted.

use std::fmt;

/// One process-memory figure, or an explicit statement that it is unavailable.
///
/// Deliberately not `Option<u64>` with a `None` that prints as `0`, and
/// deliberately not a bare `u64`. The distinction between "measured zero" and
/// "this platform does not expose it" is load-bearing for every conclusion
/// drawn from the snapshot, so it is carried in the type rather than left to a
/// convention a caller can forget.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemoryMetric {
    /// Measured, in bytes.
    Bytes(u64),
    /// This platform exposes no figure with these semantics through the APIs
    /// SonicTerm builds against, or the query failed.
    Unsupported,
}

impl MemoryMetric {
    /// The measured byte count, if there is one.
    ///
    /// Returns `None` for [`Self::Unsupported`] so a caller computing a delta
    /// cannot accidentally treat an unavailable figure as zero.
    #[must_use]
    pub fn bytes(self) -> Option<u64> {
        match self {
            Self::Bytes(bytes) => Some(bytes),
            Self::Unsupported => None,
        }
    }
}

impl fmt::Display for MemoryMetric {
    /// Renders as the byte count, or the literal `unsupported`.
    ///
    /// Used directly as a tracing field value, so this string is what lands in
    /// the log a user greps. `unsupported` is spelled out rather than left
    /// blank because an empty field reads as a bug in the logger.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Bytes(bytes) => write!(f, "{bytes}"),
            Self::Unsupported => f.write_str("unsupported"),
        }
    }
}

/// A change between two samples of the same figure.
///
/// Separate from [`MemoryMetric`] because "unavailable" has two distinct
/// causes here and a reader needs to tell them apart: there may be no earlier
/// sample to compare against (the first snapshot of a session), or the figure
/// itself may be unsupported on this platform. Both render as `unavailable`
/// rather than as `+0`, which would claim the process did not move.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemoryDelta {
    /// Both samples were measured; the difference in bytes, signed.
    Changed(i64),
    /// No comparable pair — a first sample, or an unsupported figure.
    Unavailable,
}

impl MemoryDelta {
    /// The change between two figures, when both were measured.
    ///
    /// Any unsupported or absent side yields [`Self::Unavailable`]: a delta
    /// against a value that was never measured is not a smaller signal, it is
    /// a different claim entirely.
    #[must_use]
    pub fn between(previous: MemoryMetric, current: MemoryMetric) -> Self {
        match (previous.bytes(), current.bytes()) {
            (Some(previous), Some(current)) => {
                // Signed and saturating: the figure exists to be read, and a
                // diagnostic that panics on a large process is absent exactly
                // when it is needed.
                let previous = i64::try_from(previous).unwrap_or(i64::MAX);
                let current = i64::try_from(current).unwrap_or(i64::MAX);
                Self::Changed(current.saturating_sub(previous))
            }
            _ => Self::Unavailable,
        }
    }
}

impl fmt::Display for MemoryDelta {
    /// Renders with an explicit sign, or the literal `unavailable`.
    ///
    /// The sign is always written, including for a positive change, so a
    /// growth curve can be read by eye without checking a column header.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Changed(delta) => write!(f, "{delta:+}"),
            Self::Unavailable => f.write_str("unavailable"),
        }
    }
}

/// What the OS reports for this process, at one instant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProcessMemory {
    /// Memory charged to this process alone and not backed by a clean file
    /// page. Windows calls it private/commit; macOS's nearest equivalent is
    /// not reachable through the APIs this crate builds against.
    pub private_committed: MemoryMetric,
    /// Pages resident in physical memory (macOS "resident size", Windows
    /// "working set").
    pub resident: MemoryMetric,
    /// Reserved address space. Large by design; not a consumption figure.
    pub virtual_bytes: MemoryMetric,
}

impl ProcessMemory {
    /// Every figure unavailable.
    ///
    /// Used both by platforms with no implementation and by a failed query on
    /// a platform that has one — a query that failed produced no measurement,
    /// which is the same claim.
    #[must_use]
    pub const fn unsupported() -> Self {
        Self {
            private_committed: MemoryMetric::Unsupported,
            resident: MemoryMetric::Unsupported,
            virtual_bytes: MemoryMetric::Unsupported,
        }
    }
}

/// Read this process's memory figures from the OS.
///
/// Cheap enough for the thirty-second sampling cadence and nowhere near cheap
/// enough for a per-frame path: on Windows the virtual figure walks the
/// process's address space region by region.
///
/// Never panics and never blocks. A failed query reports
/// [`MemoryMetric::Unsupported`] rather than a zero or a stale value.
#[must_use]
pub fn sample() -> ProcessMemory {
    sample_platform()
}

/// macOS: `proc_pidinfo(PROC_PIDTASKINFO)`.
///
/// Yields resident and virtual sizes for the task. **Private/committed is
/// deliberately unsupported here.** The figure a macOS user sees as "Memory"
/// in Activity Monitor, and the one a jetsam termination is applied against,
/// is `phys_footprint` from `task_vm_info`. `libc` does not expose
/// `task_vm_info_data_t` at this MSRV, so producing it would mean declaring
/// the struct layout by hand and reading a kernel structure through it —
/// a layout mismatch would not fail to compile, it would return a plausible
/// wrong number in a diagnostic whose whole value is being trustworthy.
///
/// Reporting it as unsupported is the honest alternative: a user reading the
/// snapshot learns that this figure is not available rather than being handed
/// a number that might be another field entirely.
#[cfg(target_os = "macos")]
fn sample_platform() -> ProcessMemory {
    let mut info: libc::proc_taskinfo =
        // SAFETY: `proc_taskinfo` is a plain C struct of integers, for which an
        // all-zero bit pattern is a valid value. Zeroing first also means a
        // short write below leaves defined bytes rather than uninitialised ones.
        unsafe { std::mem::zeroed() };
    let size = std::mem::size_of::<libc::proc_taskinfo>();
    let Ok(size_arg) = libc::c_int::try_from(size) else {
        // When: size does not fit a c_int, so the kernel cannot be asked for a
        // struct that large and no measurement is possible.
        return ProcessMemory::unsupported();
    };
    let Ok(pid) = libc::c_int::try_from(std::process::id()) else {
        // When: this process id does not fit a c_int, so it cannot be passed to
        // proc_pidinfo and nothing can be sampled.
        return ProcessMemory::unsupported();
    };
    let written =
        // SAFETY: `proc_pidinfo` writes at most `size_arg` bytes through the
        // out-pointer, which addresses `info` — a correctly-sized and
        // correctly-aligned `proc_taskinfo` owned by this frame. `pid` is this
        // process.
        unsafe {
            libc::proc_pidinfo(
                pid,
                libc::PROC_PIDTASKINFO,
                0,
                std::ptr::addr_of_mut!(info).cast::<libc::c_void>(),
                size_arg,
            )
        };
    if written != size_arg {
        // When: written disagrees with size_arg, so the kernel left the struct
        // unfilled and any figure read from it would be the zeroes above.
        return ProcessMemory::unsupported();
    }

    ProcessMemory {
        private_committed: MemoryMetric::Unsupported,
        resident: MemoryMetric::Bytes(info.pti_resident_size),
        virtual_bytes: MemoryMetric::Bytes(info.pti_virtual_size),
    }
}

/// Windows: `GetProcessMemoryInfo` plus a `VirtualQuery` walk.
///
/// `PrivateUsage` is the commit charge for this process — the figure that
/// matters when the system runs out of commit — and `WorkingSetSize` is the
/// resident set. Neither structure carries reserved address space, so the
/// virtual figure is summed by walking the address space; that walk is why
/// this function is too expensive for a per-frame path.
#[cfg(windows)]
fn sample_platform() -> ProcessMemory {
    use windows::Win32::System::ProcessStatus::{
        GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS, PROCESS_MEMORY_COUNTERS_EX,
    };
    use windows::Win32::System::Threading::GetCurrentProcess;

    let mut counters = PROCESS_MEMORY_COUNTERS_EX {
        cb: u32::try_from(std::mem::size_of::<PROCESS_MEMORY_COUNTERS_EX>()).unwrap_or(0),
        ..Default::default()
    };
    if counters.cb == 0 {
        // When: cb is zero, so the struct size did not fit a u32 and the API
        // has no way to learn how many bytes it may write.
        return ProcessMemory::unsupported();
    }

    let queried =
        // SAFETY: `counters` is a correctly-sized, correctly-aligned
        // `PROCESS_MEMORY_COUNTERS_EX`; `cb` tells the API how many bytes it may
        // write. The `EX` form is layout-compatible with the base structure,
        // which is why the API is documented to take the cast — it reads `cb`
        // to decide which it was given.
        unsafe {
            GetProcessMemoryInfo(
                GetCurrentProcess(),
                std::ptr::addr_of_mut!(counters).cast::<PROCESS_MEMORY_COUNTERS>(),
                counters.cb,
            )
        };
    if queried.is_err() {
        // When: queried reports failure, so counters was left unpopulated and
        // reading it would report an invented measurement.
        return ProcessMemory::unsupported();
    }

    ProcessMemory {
        private_committed: MemoryMetric::Bytes(counters.PrivateUsage as u64),
        resident: MemoryMetric::Bytes(counters.WorkingSetSize as u64),
        virtual_bytes: reserved_address_space(),
    }
}

/// Sum every region of this process's address space that is not free.
///
/// `GetProcessMemoryInfo` reports commit and working set but not reserved
/// address space, and reserved space is exactly the figure that makes a
/// process look enormous while it is behaving. Walking the regions is the
/// documented way to obtain it.
///
/// Bounded by construction: each step advances past the region just measured,
/// and a region of zero size ends the walk rather than repeating it.
#[cfg(windows)]
fn reserved_address_space() -> MemoryMetric {
    use windows::Win32::System::Memory::{VirtualQuery, MEMORY_BASIC_INFORMATION, MEM_FREE};

    let mut total: u64 = 0;
    let mut address: usize = 0;
    let entry_size = std::mem::size_of::<MEMORY_BASIC_INFORMATION>();

    loop {
        let mut info = MEMORY_BASIC_INFORMATION::default();
        let written =
            // SAFETY: `info` is a correctly-sized, correctly-aligned
            // out-parameter and `entry_size` describes it. `address` is only
            // ever a region base the previous iteration reported, so it stays
            // inside the space the API accepts.
            unsafe {
                VirtualQuery(Some(address as *const core::ffi::c_void), &mut info, entry_size)
            };
        if written == 0 {
            // When: written is zero, so VirtualQuery walked off the end of the
            // address space — the only documented way this loop terminates.
            break;
        }
        let region = info.RegionSize as u64;
        if region == 0 {
            // When: region is zero, so address would not advance and the walk
            // would spin here forever.
            break;
        }
        if info.State != MEM_FREE {
            total = total.saturating_add(region);
        }
        let Some(next) = address.checked_add(info.RegionSize) else {
            // When: checked_add overflows, so there is no next region base to
            // query and the walk has reached the top of the address space.
            break;
        };
        address = next;
    }

    MemoryMetric::Bytes(total)
}

/// Platforms with no implementation report every figure unavailable.
///
/// SonicTerm ships on macOS and Windows; this arm keeps the crate building
/// elsewhere without inventing a number for a platform nobody measured on.
#[cfg(not(any(target_os = "macos", windows)))]
fn sample_platform() -> ProcessMemory {
    ProcessMemory::unsupported()
}

#[cfg(test)]
#[path = "process_memory_tests.rs"]
mod process_memory_tests;
