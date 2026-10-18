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
    // A kernel older than revision 7 writes fewer bytes; the reply only has to cover the fields
    // this module reads, and can never exceed the buffer offered.
    let nofootprint_end = std::mem::offset_of!(TaskVmInfo, ledger_tag_graphics_nofootprint) + 8;
    assert!(reply.written_bytes >= nofootprint_end, "{} bytes", reply.written_bytes);
    assert!(reply.written_bytes <= std::mem::size_of::<TaskVmInfo>(), "{}", reply.written_bytes);
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

/// An enumerator over `adapters` that ends with `DXGI_ERROR_NOT_FOUND` after the last one.
fn listing(adapters: &[ListedAdapter]) -> impl FnMut(u32) -> Enumerated + '_ {
    |index| {
        adapters
            .get(usize::try_from(index).expect("index fits usize"))
            .map_or(Enumerated::End, |adapter| Enumerated::Adapter(*adapter))
    }
}

/// Contract: the renderer's DXGI adapter is chosen only when its PCI ids are unique in a list
/// proved complete by `DXGI_ERROR_NOT_FOUND`. Ids name a GPU model, so two identical GPUs are
/// ambiguous and no figure is read, rather than possibly the other GPU's usage.
#[test]
fn duplicate_adapter_ids_are_ambiguous_not_the_first_match() {
    let renderer = DxgiAdapterIds { vendor: 0x10de, device: 0x2684 };
    let gpu = ListedAdapter { vendor: 0x10de, device: 0x2684 };
    let other = ListedAdapter { vendor: 0x10de, device: 0x1234 };
    let warp = ListedAdapter { vendor: 0x1414, device: 0x008c };
    assert_eq!(select_adapter(renderer, listing(&[warp, gpu])), Ok(1));
    assert_eq!(select_adapter(renderer, listing(&[other, gpu, warp])), Ok(1));
    assert!(select_adapter(renderer, listing(&[gpu, warp, gpu])).is_err());
    assert!(select_adapter(renderer, listing(&[warp, other])).is_err());
    assert!(select_adapter(renderer, listing(&[])).is_err());
    let reading = GraphicsMemory::unavailable_because(GraphicsSource::Dxgi, "ambiguous");
    assert_eq!(reading.unavailable_reason, Some("ambiguous"));
    assert_eq!(reading.dedicated, MemoryMetric::Unsupported);
}

/// Contract: enumeration fails closed. A list that reaches the adapter cap without
/// `DXGI_ERROR_NOT_FOUND` may hide a twin beyond it, and a failed enumeration may hide one too, so
/// both are unavailable even when the adapters seen so far match only once.
#[test]
fn an_incomplete_adapter_list_never_proves_a_match_unique() {
    let renderer = DxgiAdapterIds { vendor: 0x10de, device: 0x2684 };
    let gpu = ListedAdapter { vendor: 0x10de, device: 0x2684 };
    let filler = ListedAdapter { vendor: 0x1414, device: 0x008c };
    let cap = usize::try_from(MAX_DXGI_ADAPTERS).expect("cap fits usize");
    // The renderer's twin sits just past the cap, where enumeration never looks.
    let mut beyond_cap = vec![gpu];
    beyond_cap.extend(std::iter::repeat_n(filler, cap - 1));
    beyond_cap.push(gpu);
    let error = select_adapter(renderer, listing(&beyond_cap)).unwrap_err();
    assert!(error.contains("more adapters than SonicTerm checks"), "{error}");
    let failing = |index| if index == 0 { Enumerated::Adapter(gpu) } else { Enumerated::Failed };
    let error = select_adapter(renderer, failing).unwrap_err();
    assert!(error.contains("enumeration failed"), "{error}");
    let full = vec![filler; cap - 1].into_iter().chain([gpu]).collect::<Vec<_>>();
    assert_eq!(
        select_adapter(renderer, listing(&full)),
        Ok(MAX_DXGI_ADAPTERS - 1),
        "a list that ends with DXGI_ERROR_NOT_FOUND at the cap is complete"
    );
}
