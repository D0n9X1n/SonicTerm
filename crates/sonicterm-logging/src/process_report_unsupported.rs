//! Other platforms: no process-table or footprint query.

use std::sync::atomic::AtomicBool;

use super::{MemoryMetric, ProcessFootprint, ProcessTable, ThreadSummary};

/// Placeholder table type; `table` never returns one on these platforms.
#[allow(dead_code)] // Named only in `table`'s signature, which always returns `None`.
pub(super) struct Table;

impl ProcessTable for Table {
    fn name(&self, _pid: u32) -> Option<String> {
        None
    }
    fn start_token(&self, _pid: u32) -> Option<u64> {
        None
    }
    fn children(&self, _pid: u32, _limit: usize) -> Vec<u32> {
        Vec::new()
    }
    fn memory(&self, _pid: u32) -> MemoryMetric {
        MemoryMetric::Unsupported
    }
}

/// No process table on this platform.
pub(super) fn table(_cancel: &AtomicBool) -> Option<Table> {
    None
}

/// No footprint query on this platform.
pub(super) fn footprint() -> ProcessFootprint {
    ProcessFootprint {
        current: MemoryMetric::Unsupported,
        current_label: "footprint",
        peak: MemoryMetric::Unsupported,
        peak_label: "peak",
    }
}

/// No uptime query on this platform.
pub(super) fn uptime_secs() -> Option<u64> {
    None
}

/// No CPU-time query on this platform.
pub(super) fn cpu_time() -> Option<std::time::Duration> {
    None
}

/// No thread query on this platform.
pub(super) fn threads(_cancel: &AtomicBool) -> ThreadSummary {
    ThreadSummary { total: None, names: Vec::new() }
}
