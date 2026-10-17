//! What the operating system says this process holds in graphics memory.
//!
//! The renderer's graphics ledger counts what SonicTerm *requested* from the
//! device. This module reads what the OS charges the process for, which is the
//! other half of the comparison: driver padding, deferred releases and
//! compositor-held drawables appear here and in no request.
//!
//! Each platform exposes a different figure, so each is labelled as what it is:
//!
//! - **macOS**: the kernel's per-task graphics ledgers from
//!   `task_info(TASK_VM_INFO)` (`mach/task_info.h`, revision 3 and later):
//!   `ledger_tag_graphics_footprint`, graphics memory charged to the process's
//!   physical footprint, and `ledger_tag_graphics_nofootprint`, graphics memory
//!   tagged to the process but not charged to it. They are the kernel's ledger,
//!   not necessarily the split the `footprint` tool prints, and IOSurface
//!   mappings are counted by neither.
//! - **Windows**: `IDXGIAdapter3::QueryVideoMemoryInfo` on the renderer's DXGI
//!   adapter, `DXGI_MEMORY_SEGMENT_GROUP_LOCAL` (dedicated) and `NON_LOCAL`
//!   (shared) current usage, reported separately. Only when the renderer runs
//!   on DX12; the GDI software presenter has no adapter to ask.
//! - **Linux and others**: unavailable.
//!
//! ## Why the macOS structure is declared here, and how it stays honest
//!
//! [`crate::process_memory`] declined to hand-declare `task_vm_info` because an
//! unchecked layout mismatch returns a plausible wrong number. This module reads
//! it under two checks that turn a mismatch into "unavailable" instead. The
//! declaration is packed to four bytes, as `mach/task_info.h` declares it under
//! `#pragma pack(push, 4)`, and its size and the offsets of the fields read are
//! asserted at compile time against figures measured from the SDK header with a
//! C probe (`sizeof` 372, `TASK_VM_INFO_COUNT` 93). At run time a field is read
//! only when the count the kernel returns covers the field's last byte. A kernel
//! older than the revision that added a field leaves it unread, never zero.
//!
//! ## Why Windows can decline to answer
//!
//! wgpu identifies its adapter by PCI vendor and device id, which name a GPU
//! model, not one installed GPU. When two DXGI adapters share the renderer's ids
//! the reading is unavailable rather than possibly another GPU's usage.

use crate::process_memory::MemoryMetric;

/// Which OS query produced a [`GraphicsMemory`] reading.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GraphicsSource {
    /// macOS `task_info(TASK_VM_INFO)` graphics ledgers.
    TaskVmInfo,
    /// Windows `IDXGIAdapter3::QueryVideoMemoryInfo` on the renderer's adapter.
    Dxgi,
    /// No OS query applies: another platform, or a renderer with no DXGI adapter.
    None,
}

impl GraphicsSource {
    /// The stable name a log field prints.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::TaskVmInfo => "task_vm_info",
            Self::Dxgi => "dxgi",
            Self::None => "none",
        }
    }
}

/// The process's graphics memory as the OS reports it, with each figure's own name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GraphicsMemory {
    /// The query that produced this reading.
    pub source: GraphicsSource,
    /// Why the source gave no figure, when the reason is known: for example, two adapters that
    /// share the renderer's ids. `None` when the figures are measured or simply not offered.
    pub unavailable_reason: Option<&'static str>,
    /// macOS: graphics memory charged to the physical footprint.
    pub footprint: MemoryMetric,
    /// macOS: graphics memory tagged to the process but not charged to its footprint.
    pub nofootprint: MemoryMetric,
    /// Windows: dedicated (`LOCAL` segment group) video memory in use.
    pub dedicated: MemoryMetric,
    /// Windows: shared (`NON_LOCAL` segment group) system memory in use for video.
    pub shared: MemoryMetric,
}

impl GraphicsMemory {
    /// A reading with no figure, from `source`: the query does not apply or did not answer.
    #[must_use]
    pub const fn unavailable(source: GraphicsSource) -> Self {
        Self {
            source,
            unavailable_reason: None,
            footprint: MemoryMetric::Unsupported,
            nofootprint: MemoryMetric::Unsupported,
            dedicated: MemoryMetric::Unsupported,
            shared: MemoryMetric::Unsupported,
        }
    }

    /// A reading from `source` with no figure, for the stated `reason`.
    #[must_use]
    pub const fn unavailable_because(source: GraphicsSource, reason: &'static str) -> Self {
        let mut reading = Self::unavailable(source);
        reading.unavailable_reason = Some(reason);
        reading
    }

    /// The figures this reading's source defines, as `(label, figure)` pairs in report order.
    #[must_use]
    pub fn figures(&self) -> Vec<(&'static str, MemoryMetric)> {
        match self.source {
            GraphicsSource::TaskVmInfo => vec![
                ("graphics in footprint", self.footprint),
                ("graphics not in footprint", self.nofootprint),
            ],
            GraphicsSource::Dxgi => {
                vec![("dedicated (local)", self.dedicated), ("shared (non-local)", self.shared)]
            }
            GraphicsSource::None => Vec::new(),
        }
    }
}

/// The DXGI adapter the renderer presents through, by PCI vendor and device id.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DxgiAdapterIds {
    /// PCI vendor id, as `wgpu::AdapterInfo::vendor` reports it.
    pub vendor: u32,
    /// PCI device id, as `wgpu::AdapterInfo::device` reports it.
    pub device: u32,
}

/// Read the process's graphics memory from the OS.
///
/// `dxgi_adapter` names the renderer's DXGI adapter on Windows; `None` there (the GDI software
/// presenter, or no renderer) and every argument elsewhere is ignored. Never panics; a failed
/// query reports [`MemoryMetric::Unsupported`] figures, never zero.
#[must_use]
pub fn sample(dxgi_adapter: Option<DxgiAdapterIds>) -> GraphicsMemory {
    sample_platform(dxgi_adapter)
}

/// One DXGI adapter as enumeration lists it: its PCI ids.
#[cfg(any(windows, test))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ListedAdapter {
    /// PCI vendor id from `DXGI_ADAPTER_DESC1::VendorId`.
    pub(crate) vendor: u32,
    /// PCI device id from `DXGI_ADAPTER_DESC1::DeviceId`.
    pub(crate) device: u32,
}

/// Which listed adapter is the renderer's, judged by PCI ids alone.
#[cfg(any(windows, test))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AdapterMatch {
    /// Exactly one listed adapter has the renderer's ids; its position in the list.
    Unique(usize),
    /// No listed adapter has the renderer's ids.
    Missing,
    /// Two or more listed adapters have the renderer's ids, so the ids cannot tell them apart.
    Ambiguous,
}

/// Find the renderer's adapter, `ids`, in `listed`; ids shared by two adapters are ambiguous.
#[cfg(any(windows, test))]
pub(crate) fn match_adapter(ids: DxgiAdapterIds, listed: &[ListedAdapter]) -> AdapterMatch {
    let mut matching = listed
        .iter()
        .enumerate()
        .filter(|(_, adapter)| adapter.vendor == ids.vendor && adapter.device == ids.device)
        .map(|(position, _)| position);
    match (matching.next(), matching.next()) {
        (Some(position), None) => AdapterMatch::Unique(position),
        (None, _) => AdapterMatch::Missing,
        (Some(_), Some(_)) => AdapterMatch::Ambiguous,
    }
}

/// `task_vm_info` through revision 7, as `mach/task_info.h` declares it under
/// `#pragma pack(push, 4)`: 8-byte fields can sit at 4-byte offsets, so the struct is 372 bytes,
/// not the 376 a naturally aligned declaration would be. Fields are only ever copied out by value.
#[cfg(target_os = "macos")]
#[repr(C, packed(4))]
#[derive(Clone, Copy)]
struct TaskVmInfo {
    virtual_size: u64,
    region_count: i32,
    page_size: i32,
    resident_size: u64,
    resident_size_peak: u64,
    device: u64,
    device_peak: u64,
    internal: u64,
    internal_peak: u64,
    external: u64,
    external_peak: u64,
    reusable: u64,
    reusable_peak: u64,
    purgeable_volatile_pmap: u64,
    purgeable_volatile_resident: u64,
    purgeable_volatile_virtual: u64,
    compressed: u64,
    compressed_peak: u64,
    compressed_lifetime: u64,
    // Revision 1.
    phys_footprint: u64,
    // Revision 2.
    min_address: u64,
    max_address: u64,
    // Revision 3.
    ledger_phys_footprint_peak: i64,
    ledger_purgeable_nonvolatile: i64,
    ledger_purgeable_novolatile_compressed: i64,
    ledger_purgeable_volatile: i64,
    ledger_purgeable_volatile_compressed: i64,
    ledger_tag_network_nonvolatile: i64,
    ledger_tag_network_nonvolatile_compressed: i64,
    ledger_tag_network_volatile: i64,
    ledger_tag_network_volatile_compressed: i64,
    ledger_tag_media_footprint: i64,
    ledger_tag_media_footprint_compressed: i64,
    ledger_tag_media_nofootprint: i64,
    ledger_tag_media_nofootprint_compressed: i64,
    ledger_tag_graphics_footprint: i64,
    ledger_tag_graphics_footprint_compressed: i64,
    ledger_tag_graphics_nofootprint: i64,
    ledger_tag_graphics_nofootprint_compressed: i64,
    ledger_tag_neural_footprint: i64,
    ledger_tag_neural_footprint_compressed: i64,
    ledger_tag_neural_nofootprint: i64,
    ledger_tag_neural_nofootprint_compressed: i64,
    // Revision 4.
    limit_bytes_remaining: u64,
    // Revision 5.
    decompressions: i32,
    // Revision 6.
    ledger_swapins: i64,
    // Revision 7.
    ledger_tag_neural_nofootprint_total: i64,
    ledger_tag_neural_nofootprint_peak: i64,
}

// Size and the offsets of the fields read, as a C probe compiled against the SDK header measured
// them (`sizeof` 372, `TASK_VM_INFO_COUNT` 93): a declaration that drifts from the packed layout
// fails to compile instead of reading another field.
#[cfg(target_os = "macos")]
const _: () = {
    assert!(std::mem::offset_of!(TaskVmInfo, phys_footprint) == 144);
    assert!(std::mem::offset_of!(TaskVmInfo, ledger_tag_graphics_footprint) == 272);
    assert!(std::mem::offset_of!(TaskVmInfo, ledger_tag_graphics_nofootprint) == 288);
    assert!(std::mem::offset_of!(TaskVmInfo, ledger_swapins) == 348);
    assert!(std::mem::size_of::<TaskVmInfo>() == 372);
    assert!(std::mem::size_of::<TaskVmInfo>() / NATURAL_BYTES == 93);
};

/// `TASK_VM_INFO` from `mach/task_info.h`.
#[cfg(target_os = "macos")]
const TASK_VM_INFO: libc::task_flavor_t = 22;

/// Bytes per `natural_t`, the unit of a `task_info` count.
#[cfg(target_os = "macos")]
const NATURAL_BYTES: usize = std::mem::size_of::<libc::natural_t>();

/// One `task_info(TASK_VM_INFO)` reply: the structure and how many bytes of it the kernel wrote.
#[cfg(target_os = "macos")]
struct TaskVmReply {
    info: TaskVmInfo,
    written_bytes: usize,
}

#[cfg(target_os = "macos")]
impl TaskVmReply {
    /// Whether the kernel wrote every byte of a field at `offset` of `size` bytes.
    fn covers(&self, offset: usize, size: usize) -> bool {
        offset.saturating_add(size) <= self.written_bytes
    }

    /// A ledger field at `offset`, or `Unsupported` when the reply does not reach it or it is
    /// negative, which a byte balance cannot be.
    fn ledger(&self, offset: usize, value: i64) -> MemoryMetric {
        if !self.covers(offset, std::mem::size_of::<i64>()) {
            // When: `covers` is false, the kernel's revision predates this field.
            return MemoryMetric::Unsupported;
        }
        u64::try_from(value).map_or(MemoryMetric::Unsupported, MemoryMetric::Bytes)
    }
}

/// Ask the kernel for this task's `TASK_VM_INFO`, or `None` when it refuses.
///
/// libc marks `mach_task_self` deprecated in favour of the `mach2` crate; this crate keeps libc's
/// binding rather than add a dependency for one call.
#[cfg(target_os = "macos")]
#[allow(deprecated)]
fn task_vm_info() -> Option<TaskVmReply> {
    let mut info: TaskVmInfo =
        // SAFETY: `TaskVmInfo` is a `repr(C)` struct of integers, so all-zero bytes are valid.
        unsafe { std::mem::zeroed() };
    let capacity = std::mem::size_of::<TaskVmInfo>() / NATURAL_BYTES;
    let mut count = libc::mach_msg_type_number_t::try_from(capacity).ok()?;
    let status =
        // SAFETY: `info` is writable for `count` natural_t units, the kernel writes at most
        // `count` of them and returns how many it wrote, and `mach_task_self` names this task.
        unsafe {
            libc::task_info(
                libc::mach_task_self(),
                TASK_VM_INFO,
                std::ptr::addr_of_mut!(info).cast::<libc::integer_t>(),
                &mut count,
            )
        };
    if status != libc::KERN_SUCCESS {
        // When: `status` is not `KERN_SUCCESS`, the reply carries no measurement.
        return None;
    }
    // A count above what was offered cannot describe this buffer; clamp it to the buffer.
    let written_units = usize::try_from(count).unwrap_or(0).min(capacity);
    Some(TaskVmReply { info, written_bytes: written_units * NATURAL_BYTES })
}

/// macOS: the two graphics ledgers of `TASK_VM_INFO`, each only when the reply covers it.
#[cfg(target_os = "macos")]
fn sample_platform(_dxgi_adapter: Option<DxgiAdapterIds>) -> GraphicsMemory {
    let Some(reply) = task_vm_info() else {
        // When: `task_vm_info` returned None, the kernel answered nothing for either ledger.
        return GraphicsMemory::unavailable(GraphicsSource::TaskVmInfo);
    };
    // Copied out by value: a reference into the packed struct could be misaligned.
    let footprint = reply.info.ledger_tag_graphics_footprint;
    let nofootprint = reply.info.ledger_tag_graphics_nofootprint;
    GraphicsMemory {
        footprint: reply
            .ledger(std::mem::offset_of!(TaskVmInfo, ledger_tag_graphics_footprint), footprint),
        nofootprint: reply
            .ledger(std::mem::offset_of!(TaskVmInfo, ledger_tag_graphics_nofootprint), nofootprint),
        ..GraphicsMemory::unavailable(GraphicsSource::TaskVmInfo)
    }
}

/// Windows: dedicated and shared usage of the renderer's DXGI adapter, when it has one.
#[cfg(windows)]
fn sample_platform(dxgi_adapter: Option<DxgiAdapterIds>) -> GraphicsMemory {
    let Some(ids) = dxgi_adapter else {
        // When: `dxgi_adapter` is None, the renderer presents without a DXGI adapter to ask.
        return GraphicsMemory::unavailable(GraphicsSource::None);
    };
    match dxgi_usage(ids) {
        Ok((dedicated, shared)) => GraphicsMemory {
            dedicated: MemoryMetric::Bytes(dedicated),
            shared: MemoryMetric::Bytes(shared),
            ..GraphicsMemory::unavailable(GraphicsSource::Dxgi)
        },
        Err(reason) => GraphicsMemory::unavailable_because(GraphicsSource::Dxgi, reason),
    }
}

/// Most adapters enumerated while looking for the renderer's; a real system lists a handful.
#[cfg(windows)]
const MAX_DXGI_ADAPTERS: u32 = 16;

/// `CurrentUsage` of the renderer's DXGI adapter for the local and non-local segment groups of
/// node 0, or why it cannot be read.
///
/// The adapter is found by PCI vendor and device id, the identity `wgpu::AdapterInfo` exposes. Ids
/// name a model, so an adapter is read only when it is the one listed adapter with those ids; an
/// adapter whose description cannot be read makes the match unknowable too.
#[cfg(windows)]
fn dxgi_usage(ids: DxgiAdapterIds) -> Result<(u64, u64), &'static str> {
    use windows::core::Interface;
    use windows::Win32::Graphics::Dxgi::{
        CreateDXGIFactory1, IDXGIAdapter1, IDXGIAdapter3, IDXGIFactory1, DXGI_MEMORY_SEGMENT_GROUP,
        DXGI_MEMORY_SEGMENT_GROUP_LOCAL, DXGI_MEMORY_SEGMENT_GROUP_NON_LOCAL,
        DXGI_QUERY_VIDEO_MEMORY_INFO,
    };

    let created =
        // SAFETY: `CreateDXGIFactory1` takes no pointers and returns an owned COM interface.
        unsafe { CreateDXGIFactory1::<IDXGIFactory1>() };
    let factory = created.map_err(|_| "the DXGI factory could not be created")?;
    let mut adapters: Vec<IDXGIAdapter1> = Vec::new();
    let mut listed = Vec::new();
    for index in 0..MAX_DXGI_ADAPTERS {
        let enumerated =
            // SAFETY: `factory` is a live interface; an index past the last adapter returns Err.
            unsafe { factory.EnumAdapters1(index) };
        let Ok(adapter) = enumerated else {
            // When: `enumerated` is Err, the list is exhausted.
            break;
        };
        let described =
            // SAFETY: `adapter` is a live interface and `GetDesc1` writes only its own result.
            unsafe { adapter.GetDesc1() };
        let desc = described.map_err(|_| "a DXGI adapter's description could not be read")?;
        listed.push(ListedAdapter { vendor: desc.VendorId, device: desc.DeviceId });
        adapters.push(adapter);
    }
    let position = match match_adapter(ids, &listed) {
        AdapterMatch::Unique(position) => position,
        AdapterMatch::Missing => {
            // When: `Missing`, no listed adapter is the renderer's, so there is nothing to read.
            return Err("no DXGI adapter has the renderer's ids");
        }
        AdapterMatch::Ambiguous => {
            // When: `Ambiguous`, the ids cannot tell the renderer's GPU from its twin; refuse.
            return Err("two or more DXGI adapters share the renderer's vendor and device ids");
        }
    };
    let adapter3 = adapters[position]
        .cast::<IDXGIAdapter3>()
        .map_err(|_| "the renderer's DXGI adapter has no IDXGIAdapter3")?;
    let usage = |group: DXGI_MEMORY_SEGMENT_GROUP| -> Result<u64, &'static str> {
        let mut info = DXGI_QUERY_VIDEO_MEMORY_INFO::default();
        // SAFETY: `adapter3` is live and `info` is a writable, correctly typed out-parameter.
        unsafe { adapter3.QueryVideoMemoryInfo(0, group, &mut info) }
            .map_err(|_| "QueryVideoMemoryInfo failed")?;
        Ok(info.CurrentUsage)
    };
    Ok((usage(DXGI_MEMORY_SEGMENT_GROUP_LOCAL)?, usage(DXGI_MEMORY_SEGMENT_GROUP_NON_LOCAL)?))
}

/// Other platforms: no graphics memory query.
#[cfg(not(any(target_os = "macos", windows)))]
fn sample_platform(_dxgi_adapter: Option<DxgiAdapterIds>) -> GraphicsMemory {
    GraphicsMemory::unavailable(GraphicsSource::None)
}

#[cfg(test)]
#[path = "graphics_memory_tests.rs"]
mod graphics_memory_tests;
