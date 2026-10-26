use super::*;

use std::collections::BTreeSet;
use std::ops::Range;
use std::sync::atomic::AtomicUsize;

fn validation(description: &str) -> wgpu::Error {
    wgpu::Error::Validation {
        source: Box::new(std::io::Error::other(description.to_owned())),
        description: description.to_owned(),
    }
}

fn out_of_memory() -> wgpu::Error {
    wgpu::Error::OutOfMemory { source: Box::new(std::io::Error::other("out of memory")) }
}

fn internal(description: &str) -> wgpu::Error {
    wgpu::Error::Internal {
        source: Box::new(std::io::Error::other(description.to_owned())),
        description: description.to_owned(),
    }
}

fn counting_waker(state: &DeviceErrorState) -> Arc<AtomicUsize> {
    let wakes = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&wakes);
    assert!(state.set_waker(Arc::new(move || {
        counter.fetch_add(1, Ordering::SeqCst);
    })));
    wakes
}

/// States only move forward: an error after a loss keeps the device lost, and
/// the first stop stays the recorded cause.
#[test]
fn transitions_are_one_way() {
    let state = DeviceErrorState::new();
    assert_eq!(state.state(), DeviceState::Usable);
    assert!(state.accepts_gpu_work());

    state.record_uncaptured(&validation("first"));
    assert_eq!(state.state(), DeviceState::Unusable);
    assert!(!state.accepts_gpu_work());

    state.record_lost(wgpu::DeviceLostReason::Unknown, "driver reset");
    assert_eq!(state.state(), DeviceState::Lost);

    state.record_uncaptured(&validation("after loss"));
    let snapshot = state.snapshot();
    assert_eq!(snapshot.state, DeviceState::Lost);
    assert_eq!(snapshot.unusable.map(|record| record.description), Some("first".to_owned()));
    assert_eq!(snapshot.counts.validation, 2);
    assert_eq!(snapshot.records_logged, 2);
}

/// A loss while usable records `Lost` directly, with no `Unusable` record.
#[test]
fn loss_from_usable_goes_straight_to_lost() {
    let state = DeviceErrorState::new();
    state.record_lost(wgpu::DeviceLostReason::Destroyed, "");
    let snapshot = state.snapshot();
    assert_eq!(snapshot.state, DeviceState::Lost);
    assert!(snapshot.unusable.is_none());
    assert_eq!(snapshot.lost.map(|record| record.description), Some("Destroyed".to_owned()));
}

/// The lost record is set once: a second callback only raises the count.
#[test]
fn lost_record_is_set_once() {
    let state = DeviceErrorState::new();
    state.record_lost(wgpu::DeviceLostReason::Unknown, "first reset");
    state.record_lost(wgpu::DeviceLostReason::Destroyed, "second");
    let snapshot = state.snapshot();
    let lost = snapshot.lost.expect("lost record");
    assert_eq!(lost.kind, DeviceErrorKind::Lost);
    assert_eq!(lost.description, "Unknown: first reset");
    assert_eq!(snapshot.counts.lost, 2);
    assert_eq!(snapshot.records_logged, 1);
}

/// The app is woken once per transition, not once per error; repeats and
/// isolated errors never wake it.
#[test]
fn one_wake_per_transition() {
    let state = DeviceErrorState::new();
    let wakes = counting_waker(&state);
    state.record_isolated(&validation("isolated"));
    assert_eq!(wakes.load(Ordering::SeqCst), 0);
    for _ in 0..3 {
        state.record_uncaptured(&validation("repeat"));
    }
    assert_eq!(wakes.load(Ordering::SeqCst), 1);
    state.record_lost(wgpu::DeviceLostReason::Unknown, "reset");
    state.record_lost(wgpu::DeviceLostReason::Unknown, "reset again");
    assert_eq!(wakes.load(Ordering::SeqCst), 2);
    assert_eq!(state.snapshot().wakes, 2);
}

/// Renderers sharing a device keep the first waker; a second one is refused
/// and never runs.
#[test]
fn first_waker_wins() {
    let state = DeviceErrorState::new();
    let wakes = counting_waker(&state);
    let refused = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&refused);
    assert!(!state.set_waker(Arc::new(move || {
        counter.fetch_add(1, Ordering::SeqCst);
    })));
    state.record_uncaptured(&validation("stop"));
    assert_eq!(wakes.load(Ordering::SeqCst), 1);
    assert_eq!(refused.load(Ordering::SeqCst), 0);
}

/// Without an installed waker a transition still records and logs, and counts
/// no wake.
#[test]
fn transition_without_waker_counts_no_wake() {
    let state = DeviceErrorState::new();
    state.record_uncaptured(&out_of_memory());
    let snapshot = state.snapshot();
    assert_eq!(snapshot.state, DeviceState::Unusable);
    assert_eq!(snapshot.wakes, 0);
    assert_eq!(snapshot.records_logged, 1);
    assert_eq!(snapshot.unusable.map(|record| record.kind), Some(DeviceErrorKind::OutOfMemory));
}

/// Each kind has its own coalesced counter; only the stop and the first
/// isolated error produce records.
#[test]
fn counters_coalesce_by_kind() {
    let state = DeviceErrorState::new();
    state.record_uncaptured(&validation("v"));
    state.record_uncaptured(&validation("v"));
    state.record_uncaptured(&out_of_memory());
    state.record_uncaptured(&internal("i"));
    state.record_isolated(&validation("iso"));
    state.record_isolated(&validation("iso"));
    let snapshot = state.snapshot();
    assert_eq!(
        snapshot.counts,
        DeviceErrorCounts { validation: 2, out_of_memory: 1, internal: 1, isolated: 2, lost: 0 }
    );
    assert_eq!(snapshot.records_logged, 2);
    assert_eq!(snapshot.unusable.map(|record| record.kind), Some(DeviceErrorKind::Validation));
    assert_eq!(snapshot.isolated.map(|record| record.kind), Some(DeviceErrorKind::Isolated));
}

/// A validation stop the renderer observed itself counts only when the handler
/// has not already stopped the device, so one error is never counted twice.
#[test]
fn observed_validation_is_not_double_counted() {
    let fresh = DeviceErrorState::new();
    fresh.record_observed_validation("surface acquisition validation");
    assert_eq!(fresh.state(), DeviceState::Unusable);
    assert_eq!(fresh.counts().validation, 1);

    let stopped = DeviceErrorState::new();
    stopped.record_uncaptured(&validation("handler saw it"));
    stopped.record_observed_validation("surface acquisition validation");
    assert_eq!(stopped.counts().validation, 1);
    assert_eq!(
        stopped.snapshot().unusable.map(|record| record.description),
        Some("handler saw it".to_owned())
    );
}

fn every_gate() -> Vec<DeviceGate> {
    let mut gates = Vec::new();
    for state in [DeviceState::Usable, DeviceState::Unusable, DeviceState::Lost] {
        for destroy_requested in [false, true] {
            gates.push(DeviceGate { state, destroy_requested });
        }
    }
    gates
}

/// Over every combination of the three readings, the first checkpoint that
/// refused work decides the outcome, and only `Presented` acknowledges.
#[test]
fn decision_table_covers_every_reading() {
    let gates = every_gate();
    for &before in &gates {
        for &after_submit in &gates {
            for &after_present in &gates {
                let outcome = decide_frame_outcome(before, after_submit, after_present);
                let expected = if !before.accepts_gpu_work() {
                    DeviceFrameOutcome::NotStarted
                } else if !after_submit.accepts_gpu_work() {
                    DeviceFrameOutcome::SubmittedNotPresented
                } else if !after_present.accepts_gpu_work() {
                    DeviceFrameOutcome::PresentedNotAcknowledged
                } else {
                    DeviceFrameOutcome::Presented
                };
                assert_eq!(outcome, expected, "{before:?} {after_submit:?} {after_present:?}");
                assert_eq!(
                    outcome.acknowledges(),
                    before.accepts_gpu_work()
                        && after_submit.accepts_gpu_work()
                        && after_present.accepts_gpu_work()
                );
            }
        }
    }
}

/// Named rows: a pending destroy refuses work like a stop, a failed submission
/// never presents, and a failed presentation is never acknowledged.
#[test]
fn decision_table_named_rows() {
    let usable = DeviceGate { state: DeviceState::Usable, destroy_requested: false };
    let unusable = DeviceGate { state: DeviceState::Unusable, destroy_requested: false };
    let lost = DeviceGate { state: DeviceState::Lost, destroy_requested: false };
    let destroying = DeviceGate { state: DeviceState::Usable, destroy_requested: true };
    let decide = decide_frame_outcome;
    assert_eq!(decide(usable, usable, usable), DeviceFrameOutcome::Presented);
    assert_eq!(decide(unusable, usable, usable), DeviceFrameOutcome::NotStarted);
    assert_eq!(decide(destroying, usable, usable), DeviceFrameOutcome::NotStarted);
    assert_eq!(decide(usable, unusable, unusable), DeviceFrameOutcome::SubmittedNotPresented);
    assert_eq!(decide(usable, lost, lost), DeviceFrameOutcome::SubmittedNotPresented);
    assert_eq!(decide(usable, usable, unusable), DeviceFrameOutcome::PresentedNotAcknowledged);
    assert_eq!(decide(usable, usable, destroying), DeviceFrameOutcome::PresentedNotAcknowledged);
    assert!(!DeviceFrameOutcome::NotStarted.presents());
    assert!(!DeviceFrameOutcome::SubmittedNotPresented.presents());
    assert!(DeviceFrameOutcome::PresentedNotAcknowledged.presents());
    assert!(!DeviceFrameOutcome::PresentedNotAcknowledged.acknowledges());
    assert!(DeviceFrameOutcome::Presented.acknowledges());
}

/// The gate admits work only while usable and not destroy-requested, a refused
/// scope never runs its work, and both outcomes are counted.
#[test]
fn gate_refuses_work_after_stop_or_destroy_request() {
    let destroying = DeviceErrorState::new();
    assert_eq!(destroying.gpu_work("probe", || 7), Some(7));
    destroying.request_destroy();
    let mut ran = false;
    assert_eq!(destroying.gpu_work("probe", || ran = true), None);
    assert!(!ran);
    assert!(destroying.enter_gpu_work("probe").is_none());
    let snapshot = destroying.snapshot();
    assert!(snapshot.destroy_requested);
    assert_eq!(snapshot.state, DeviceState::Usable);
    assert_eq!((snapshot.admitted_work, snapshot.refused_work), (1, 2));

    let stopped = DeviceErrorState::new();
    stopped.record_uncaptured(&internal("stop"));
    assert!(stopped.enter_gpu_work("probe").is_none());
    assert_eq!(stopped.snapshot().refused_work, 1);
}

/// Errors carry the innermost live label, a relabel covers only later errors,
/// and dropping a scope restores the enclosing label.
#[test]
fn scopes_label_errors_and_restore_on_drop() {
    assert_eq!(current_operation(), UNLABELLED_OPERATION);
    let state = DeviceErrorState::new();
    let outer = state.enter_gpu_work("outer").expect("usable device");
    {
        let inner = state.enter_gpu_work("inner").expect("usable device");
        assert_eq!(current_operation(), "inner");
        inner.set_operation("inner.relabelled");
        state.record_uncaptured(&validation("labelled"));
    }
    assert_eq!(current_operation(), "outer");
    drop(outer);
    assert_eq!(current_operation(), UNLABELLED_OPERATION);
    assert_eq!(state.snapshot().unusable.map(|record| record.operation), Some("inner.relabelled"));
}

/// Each state gets its own device generation, so renderers can tell whether
/// they share a device.
#[test]
fn generations_are_unique() {
    let first = DeviceErrorState::new();
    let second = DeviceErrorState::new();
    assert_ne!(first.generation(), second.generation());
    assert_eq!(first.snapshot().generation, first.generation());
}

/// Renderer methods that must issue their GPU work through the device gate.
const GATED_METHODS: &[&str] = &[
    "new_async",
    // `try_resize` delegates here; this body configures the surface through the gate.
    "try_resize_outcome",
    "set_software_render_degrade",
    "rebuild_glyph_upload_if_needed",
    "rebuild_image_upload_if_needed",
    "allocator_snapshot",
    "render_releasing",
    "present_software_frame",
    "present_wgpu_frame",
    "probe_surface_availability",
    "__inject_gpu_fault",
    "prepare_rebind",
    "commit_rebind",
    // Test-only readback: copies the retained frame through the gate; the test maps it.
    "__copy_retained_frame",
];

/// Renderer methods that reach the device without issuing GPU work: they
/// clone its handles, compare their identity, or read its features.
const NOT_GPU_WORK: &[&str] = &[
    "shared_context",
    // Compares the instance and device handles for identity; it issues no GPU work.
    "shares_device_with",
    "effective_subpixel_aa_mode",
    "log_subpixel_aa_policy",
    "set_subpixel_aa_mode",
    "set_theme",
    "set_theme_with_opacity",
    // Reads the adapter's fixed backend, vendor and device ids for the OS graphics figure.
    "dxgi_adapter_ids",
];

/// Receivers and fields that hold a wgpu handle: the renderer's own, a candidate
/// context's, and a candidate surface's.
const GPU_HANDLES: &[&str] = &[
    "self.device",
    "self.queue",
    "self.surface",
    "self.instance",
    "context.device",
    "context.queue",
    "context.instance",
    "surface.surface",
    "surface.instance",
];

/// Device calls that only return values fixed when the device was created, so
/// they issue no GPU work and cannot raise an error.
const DEVICE_READS: &[&str] = &["self.device.limits()", "self.device.features()"];

/// The receivers the gate is called on: the renderer's own state, and the
/// state `new_async` binds before the renderer exists.
const GATE_RECEIVERS: &[&str] = &["self.device_errors", "errors"];

/// Builds the device. Only its explicitly named pre-device bootstrap calls
/// are exempt; surface configuration and retained-resource creation are gated.
const CONSTRUCTOR: &str = "new_async";

/// GPU work that bypasses the gate on purpose, with the method it belongs to:
/// the destroy fault must reach a device that an earlier fault already stopped.
const UNGATED_BY_DESIGN: &[(&str, &str)] =
    &[("__inject_gpu_fault", "destroy_and_await_loss(&self.device, &self.device_errors)")];

/// The exact read-only operations the recovery surface helper may perform without a
/// gate: reading the lost surface's Metal layer and wrapping it for a new instance.
/// Only these spellings are masked; any other handle use, `wgpu::` path, or call into
/// a GPU-reaching method in that helper still counts.
const SURFACE_READS: &[(&str, &str)] = &[
    ("candidate_surface", "self.surface.as_hal::<wgpu::hal::api::Metal>()"),
    ("candidate_surface", "wgpu::SurfaceTargetUnsafe::CoreAnimationLayer("),
];

/// Calls that wgpu treats as fatal whatever handler is installed, or whose
/// errors panic in a helper, plus error scopes, which would hide errors from
/// the handler. Method and path spellings are both listed.
const BANNED: &[&str] = &[
    ".poll(",
    "::poll(",
    ".poll_all(",
    "::poll_all(",
    "poll_all_devices",
    "create_render_bundle_encoder",
    "create_buffer_init",
    "create_texture_with_data",
    "DeviceExt",
    "get_mapped_range",
    "map_async",
    "mapped_at_creation: true",
    "push_error_scope",
];

struct Method {
    name: String,
    code: String,
}

/// How a gate call's result is bound, which decides what code it controls.
enum GateBinding {
    /// `if let Some(scope) = gate { work }`: the work is the body.
    IfLet,
    /// `let Some(scope) = gate else { return };`: the work follows it.
    LetElse,
    /// `let scope = gate.ok_or_else(..)?;`: the work follows it.
    Propagated,
}

const RECOVERY_RENDERER_SOURCES: [&str; 5] = [
    include_str!("core.rs"),
    include_str!("present.rs"),
    include_str!("rebind.rs"),
    include_str!("recovery_context.rs"),
    include_str!("atlas_lifecycle.rs"),
];

#[path = "device_error_source_scan.rs"]
mod device_error_source_scan;
