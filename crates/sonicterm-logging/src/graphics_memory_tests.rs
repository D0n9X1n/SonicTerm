use super::*;

/// Contract: an unavailable reading carries no number in any figure, so a report prints
/// "unavailable" rather than a measured zero, and a source with no query lists no figures.
#[test]
fn an_unavailable_reading_has_no_figure_and_never_reads_as_zero() {
    let reading = GraphicsMemory::unavailable(GraphicsSource::None);
    for metric in [reading.footprint, reading.nofootprint, reading.dedicated, reading.shared] {
        assert_eq!(metric, MemoryMetric::Unsupported);
        assert_eq!(metric.bytes(), None);
        assert_eq!(metric.to_string(), "unsupported");
    }
    assert!(reading.figures().is_empty());
    assert_eq!(GraphicsSource::None.as_str(), "none");
}

/// Contract: each source names its own figures: macOS's two kernel ledgers, Windows's dedicated
/// and shared segment groups, so a figure is never presented under another platform's name.
#[test]
fn each_source_labels_its_own_figures() {
    let mac = GraphicsMemory {
        footprint: MemoryMetric::Bytes(7),
        ..GraphicsMemory::unavailable(GraphicsSource::TaskVmInfo)
    };
    assert_eq!(
        mac.figures(),
        vec![
            ("graphics in footprint", MemoryMetric::Bytes(7)),
            ("graphics not in footprint", MemoryMetric::Unsupported),
        ]
    );
    let windows = GraphicsMemory {
        shared: MemoryMetric::Bytes(9),
        ..GraphicsMemory::unavailable(GraphicsSource::Dxgi)
    };
    assert_eq!(windows.figures()[1], ("shared (non-local)", MemoryMetric::Bytes(9)));
}

/// Contract: a ledger field is read only when the kernel's returned count covers it, and a
/// negative balance is not reported as bytes; a short reply (an older kernel revision) yields
/// `Unsupported`, never the zero the buffer was initialised with.
#[cfg(target_os = "macos")]
#[test]
fn a_reply_that_does_not_reach_a_field_leaves_it_unavailable() {
    let info: TaskVmInfo =
        // SAFETY: `TaskVmInfo` is a `repr(C)` struct of integers, so all-zero bytes are valid.
        unsafe { std::mem::zeroed() };
    let offset = std::mem::offset_of!(TaskVmInfo, ledger_tag_graphics_footprint);
    let short = TaskVmReply { info, written_bytes: offset + 4 };
    assert_eq!(short.ledger(offset, 0), MemoryMetric::Unsupported, "half a field is not read");
    let full = TaskVmReply { info, written_bytes: std::mem::size_of::<TaskVmInfo>() };
    assert_eq!(full.ledger(offset, 4096), MemoryMetric::Bytes(4096));
    assert_eq!(full.ledger(offset, -1), MemoryMetric::Unsupported, "a negative balance");
}

/// Contract: the hand-declared layout matches the kernel at run time, not only the header: the
/// `phys_footprint` this module reads agrees with `proc_pid_rusage`'s physical footprint, the
/// same kernel ledger read through a libc-declared struct, within allocation noise between the
/// two calls. A misplaced field would read an unrelated figure and fail this.
#[cfg(target_os = "macos")]
#[test]
fn the_task_vm_info_layout_agrees_with_rusage() {
    let reply = task_vm_info().expect("task_info(TASK_VM_INFO) answers for this task");
    assert!(reply.covers(std::mem::offset_of!(TaskVmInfo, ledger_tag_graphics_nofootprint), 8));
    let mut rusage: libc::rusage_info_v4 =
        // SAFETY: `rusage_info_v4` is a plain C struct of integers, so all-zero bytes are valid.
        unsafe { std::mem::zeroed() };
    let pid = libc::c_int::try_from(std::process::id()).expect("pid fits c_int");
    let status =
        // SAFETY: flavor V4 writes one `rusage_info_v4` through the pointer, which addresses `rusage`.
        unsafe {
            libc::proc_pid_rusage(
                pid,
                libc::RUSAGE_INFO_V4,
                std::ptr::addr_of_mut!(rusage).cast::<libc::rusage_info_t>(),
            )
        };
    assert_eq!(status, 0);
    let tolerance = 64 * 1024 * 1024;
    // Copied out by value: a reference into the packed struct could be misaligned.
    let phys_footprint = reply.info.phys_footprint;
    assert!(
        phys_footprint.abs_diff(rusage.ri_phys_footprint) < tolerance,
        "task_vm_info phys_footprint {phys_footprint} vs rusage {}",
        rusage.ri_phys_footprint
    );
    assert_eq!(reply.written_bytes, 372, "this SDK's kernel writes the whole revision-7 struct");
    let sampled = sample(None);
    assert_eq!(sampled.source, GraphicsSource::TaskVmInfo);
    assert!(sampled.footprint.bytes().is_some(), "revision 3+ kernels report the graphics ledger");
}

/// Contract: without a DXGI adapter (the GDI software presenter, or no renderer) Windows reports
/// no graphics figure rather than querying some other adapter.
#[cfg(windows)]
#[test]
fn windows_without_a_dxgi_adapter_is_unavailable() {
    assert_eq!(sample(None), GraphicsMemory::unavailable(GraphicsSource::None));
}

/// Contract: platforms with no graphics query report the reading as unavailable.
#[cfg(not(any(target_os = "macos", windows)))]
#[test]
fn other_platforms_are_unavailable() {
    assert_eq!(sample(None), GraphicsMemory::unavailable(GraphicsSource::None));
}

/// Contract: the renderer's DXGI adapter is chosen only when its PCI ids are unique among the
/// listed adapters. Ids name a GPU model, so two identical GPUs are ambiguous and no figure is
/// read, rather than possibly the other GPU's usage; a missing adapter is reported as missing.
#[test]
fn duplicate_adapter_ids_are_ambiguous_not_the_first_match() {
    let renderer = DxgiAdapterIds { vendor: 0x10de, device: 0x2684 };
    let gpu = ListedAdapter { vendor: 0x10de, device: 0x2684 };
    let other = ListedAdapter { vendor: 0x10de, device: 0x1234 };
    let warp = ListedAdapter { vendor: 0x1414, device: 0x008c };
    assert_eq!(match_adapter(renderer, &[warp, gpu]), AdapterMatch::Unique(1));
    assert_eq!(match_adapter(renderer, &[other, gpu, warp]), AdapterMatch::Unique(1));
    assert_eq!(match_adapter(renderer, &[gpu, warp, gpu]), AdapterMatch::Ambiguous);
    assert_eq!(match_adapter(renderer, &[warp, other]), AdapterMatch::Missing);
    assert_eq!(match_adapter(renderer, &[]), AdapterMatch::Missing);
    let reading = GraphicsMemory::unavailable_because(GraphicsSource::Dxgi, "ambiguous");
    assert_eq!(reading.unavailable_reason, Some("ambiguous"));
    assert_eq!(reading.dedicated, MemoryMetric::Unsupported);
}
