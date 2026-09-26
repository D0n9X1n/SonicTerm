use super::*;

use crate::device_errors::DeviceState;

const USABLE: DeviceGate = DeviceGate { state: DeviceState::Usable, destroy_requested: false };

/// Every gate reading that refuses GPU work: an unusable device, a lost
/// device, and a usable device whose intentional destroy is pending.
const STOPPED: [DeviceGate; 5] = [
    DeviceGate { state: DeviceState::Unusable, destroy_requested: false },
    DeviceGate { state: DeviceState::Unusable, destroy_requested: true },
    DeviceGate { state: DeviceState::Lost, destroy_requested: false },
    DeviceGate { state: DeviceState::Lost, destroy_requested: true },
    DeviceGate { state: DeviceState::Usable, destroy_requested: true },
];

const REASONS: [SurfaceRetryReason; 5] = [
    SurfaceRetryReason::Timeout,
    SurfaceRetryReason::Occluded,
    SurfaceRetryReason::Outdated,
    SurfaceRetryReason::Suboptimal,
    SurfaceRetryReason::SurfaceLost,
];

fn suspended(gate: DeviceGate, reports_stop: bool) -> SuspendedContext {
    SuspendedContext { generation: 7, gate, reports_stop }
}

/// Every outcome that presents nothing, and a real present, keep `render`'s
/// `Ok(())`: the typed outcome adds detail without changing the old result.
#[test]
fn successful_outcomes_keep_the_render_ok_result() {
    let outcomes = [
        PresentOutcome::Skipped(SkipReason::NoPanes),
        PresentOutcome::Skipped(SkipReason::Unchanged),
        PresentOutcome::Skipped(SkipReason::Noop),
        PresentOutcome::CachedReblit,
        PresentOutcome::AtlasRetry,
        PresentOutcome::SurfaceRetry(SurfaceRetryReason::Timeout),
        PresentOutcome::SurfaceRetry(SurfaceRetryReason::Occluded),
        PresentOutcome::SurfaceRetry(SurfaceRetryReason::Outdated),
        PresentOutcome::SurfaceRetry(SurfaceRetryReason::Suboptimal),
        PresentOutcome::SurfaceRetry(SurfaceRetryReason::SurfaceLost),
        PresentOutcome::Presented,
    ];
    for outcome in outcomes {
        let label = format!("{outcome:?}");
        assert!(outcome.into_render_result().is_ok(), "{label} must map to Ok(())");
    }
}

/// A stopped device is not a success by default: only the frame that carries
/// the renderer's one-time stop report returns the stop error, with the exact
/// message `render` returned before, and later stopped frames stay `Ok(())`.
#[test]
fn unavailable_context_returns_its_error_only_on_the_reporting_frame() {
    let reporting = PresentOutcome::RenderingUnavailable(suspended(STOPPED[0], true));
    let error = reporting.into_render_result().expect_err("the reporting frame must fail");
    assert_eq!(
        error.to_string(),
        "GPU device 7 stopped accepting work (Unusable, destroy requested: false)"
    );
    for gate in STOPPED {
        let expected = format!(
            "GPU device 7 stopped accepting work ({:?}, destroy requested: {})",
            gate.state, gate.destroy_requested
        );
        assert_eq!(suspended(gate, true).stop_error().to_string(), expected);
        let silent = PresentOutcome::RenderingUnavailable(suspended(gate, false));
        assert!(silent.into_render_result().is_ok(), "{gate:?} after the report must stay silent");
    }
}

/// A failed frame hands back its original error object, not a rebuilt message.
#[test]
fn failed_outcome_keeps_its_original_error() {
    let original = anyhow::Error::new(std::io::Error::other("surface creation failed"));
    let error = PresentOutcome::Failed(original)
        .into_render_result()
        .expect_err("a failed frame must stay an error");
    let io = error.downcast_ref::<std::io::Error>().expect("the original io::Error survives");
    assert_eq!(io.to_string(), "surface creation failed");
}

/// Timeout and occlusion keep the surface, outdated and suboptimal surfaces are
/// reconfigured, and only a lost surface is recreated on the same device.
#[test]
fn surface_retry_reasons_map_to_their_recovery() {
    assert_eq!(SurfaceRetryReason::Timeout.recovery(), SurfaceRecovery::Keep);
    assert_eq!(SurfaceRetryReason::Occluded.recovery(), SurfaceRecovery::Keep);
    assert_eq!(SurfaceRetryReason::Outdated.recovery(), SurfaceRecovery::Reconfigure);
    assert_eq!(SurfaceRetryReason::Suboptimal.recovery(), SurfaceRecovery::Reconfigure);
    assert_eq!(SurfaceRetryReason::SurfaceLost.recovery(), SurfaceRecovery::Recreate);
}

/// While the device accepts work, every surface result, including a suboptimal
/// or lost surface, is an ordinary surface retry.
#[test]
fn usable_device_reports_every_surface_result_as_a_retry() {
    for reason in REASONS {
        assert_eq!(
            surface_retry_disposition(reason, USABLE),
            SurfaceRetryDisposition::Retry,
            "{reason:?} on a usable device"
        );
    }
}

/// A stopped device is never an ordinary surface retry. A kept surface leaves
/// the stop report to the next frame's device check, as `render` always did; a
/// reconfigured or recreated surface reports the stop now and asks no redraw.
#[test]
fn stopped_device_is_never_a_surface_retry() {
    for gate in STOPPED {
        for reason in REASONS {
            let expected = match reason {
                SurfaceRetryReason::Timeout | SurfaceRetryReason::Occluded => {
                    SurfaceRetryDisposition::DeferStop
                }
                SurfaceRetryReason::Outdated
                | SurfaceRetryReason::Suboptimal
                | SurfaceRetryReason::SurfaceLost => SurfaceRetryDisposition::Stop,
            };
            assert_eq!(
                surface_retry_disposition(reason, gate),
                expected,
                "{reason:?} with {gate:?}"
            );
        }
    }
}

// These source contracts complement the GPU-free outcome tables: they pin the
// actual production exits without adding fault hooks or pretending to run a GPU.
fn compact(source: &str) -> String {
    source
        .lines()
        .filter(|line| !line.trim_start().starts_with("//"))
        .flat_map(|line| line.chars().filter(|ch| !ch.is_whitespace()))
        .collect()
}

fn source_between<'a>(source: &'a str, start: &str, end: &str) -> &'a str {
    let from = source.find(start).unwrap_or_else(|| panic!("missing {start}"));
    let to = source[from..].find(end).unwrap_or_else(|| panic!("missing {end}")) + from;
    &source[from..to]
}

/// Noop records identity but does not acknowledge dirt; an evicted atlas resets
/// and requests a retry before any presenter can see its stale UVs.
#[test]
fn noop_and_atlas_retry_are_wired_to_unacknowledged_exits() {
    let core = compact(include_str!("core.rs"));
    let noop = source_between(&core, "ifplan.mode==RenderMode::Noop{", "letinline_media_changed=");
    assert!(noop.contains(
        "self.last_frame_key=Some(plan.key);returnOk(PresentOutcome::Skipped(SkipReason::Noop));"
    ));
    assert!(
        !noop.contains("finish_successful_frame") && !noop.contains("acknowledge_presented_plan")
    );
    let atlas = source_between(
        &core,
        "ifatlas_evicted_during_frame(atlas_epoch_at_frame_start,",
        "#[cfg(debug_assertions)]",
    );
    assert!(atlas.contains("self.reset_glyph_atlas_after_eviction(atlas_epoch_at_frame_start);returnOk(PresentOutcome::AtlasRetry);"));
    assert!(!atlas.contains("present_frame(") && !atlas.contains("finish_successful_frame"));
    let reset =
        source_between(&core, "fnreset_glyph_atlas_after_eviction(", "fnglyph_atlas_epoch(");
    assert!(reset.contains("self.last_frame_key=None;self.window.request_redraw();"));
}

/// Every wgpu acquisition exit keeps its own typed reason; Suboptimal releases
/// its texture before recovery, while Validation stops rather than retries.
#[test]
fn surface_acquisition_wires_every_reason_and_drops_suboptimal_before_recovery() {
    let source = compact(include_str!("present.rs"));
    let acquire = source_between(
        &source,
        "letacquired=matchself.surface.get_current_texture(){",
        "lap(timing,\"surface_acquire\");",
    );
    for (status, reason) in [
        ("Timeout", "Timeout"),
        ("Occluded", "Occluded"),
        ("Outdated", "Outdated"),
        ("Lost", "SurfaceLost"),
    ] {
        assert!(
            acquire.contains(&format!(
                "wgpu::CurrentSurfaceTexture::{status}=>Err(SurfaceRetryReason::{reason})"
            )),
            "{status} lost its reason"
        );
    }
    assert!(acquire.contains("wgpu::CurrentSurfaceTexture::Suboptimal(frame)=>{drop(frame);Err(SurfaceRetryReason::Suboptimal)"));
    assert!(acquire.contains("wgpu::CurrentSurfaceTexture::Validation=>{self.device_errors.record_observed_validation(\"surfaceacquisitionvalidation\");returnOk(self.rendering_unavailable());"));
    let clear = acquire.find("self.last_frame_key=None;").unwrap();
    let recover = acquire.find("matchreason.recovery(){").unwrap();
    let finish = acquire.find("returnOk(self.finish_surface_retry(reason));").unwrap();
    assert!(clear < recover && recover < finish);
    assert!(acquire.contains(
        "SurfaceRecovery::Reconfigure=>{self.surface.configure(&self.device,&self.config);}"
    ));
    assert!(acquire.contains("SurfaceRecovery::Recreate=>{self.surface=self.instance.create_surface(self.window.clone())?;self.surface.configure(&self.device,&self.config);}"));
    assert!(!acquire.contains("finish_successful_frame") && !acquire.contains("queue.present("));
}

/// Stopped outcomes keep the parent report-once mapping: DeferStop still redraws
/// without consuming the report; Stop clears the key and consumes it exactly once.
#[test]
fn suspended_outcomes_keep_generation_gate_and_report_once_wiring() {
    let source = compact(include_str!("present.rs"));
    let retry =
        source_between(&source, "fnfinish_surface_retry(", "pub(super)fnrendering_unavailable(");
    assert!(retry.contains("SurfaceRetryDisposition::Retry=>{self.window.request_redraw();PresentOutcome::SurfaceRetry(reason)}"));
    assert!(retry.contains("SurfaceRetryDisposition::DeferStop=>{self.window.request_redraw();PresentOutcome::RenderingUnavailable(self.suspended_context(false))}"));
    assert!(retry.contains("SurfaceRetryDisposition::Stop=>self.rendering_unavailable()"));
    let stop =
        source_between(&source, "pub(super)fnrendering_unavailable(", "fnsuspended_context(");
    assert!(stop.contains("self.last_frame_key=None;letreports_stop=!std::mem::replace(&mutself.device_stop_reported,true);PresentOutcome::RenderingUnavailable(self.suspended_context(reports_stop))"));
    assert!(source.contains("SuspendedContext{generation:self.device_errors.generation(),gate:self.device_errors.gate(),reports_stop,"));
    assert!(!stop.contains("request_redraw") && !stop.contains("finish_successful_frame"));
}

/// Cached reblits preserve their before/after device checks, fault timing, and
/// stopped early exit before the caller can request a focus-flash redraw.
#[test]
fn cached_reblit_checkpoint_order_and_stopped_early_exit_are_preserved() {
    let source = compact(include_str!("present.rs"));
    let reblit = source_between(&source, "fnreblit_software_frame(", "fnpresent_software_frame(");
    let prepare = source_between(
        &source,
        "pub(super)fnprepare_cached_present(",
        "pub(super)fnpresent_unchanged_frame(",
    );
    let eligible =
        prepare.find("self.software_render_degrade&&self.software_frame.is_some()").unwrap();
    let fault = prepare.find("std::mem::take(&mutself.fault_stop_before_cached_present)").unwrap();
    let before = prepare.find("returnSome(self.device_errors.gate());").unwrap();
    assert!(eligible < fault && fault < before);
    let count = reblit.find("self.present_calls=self.present_calls.saturating_add(1);").unwrap();
    let present =
        reblit.find("crate::software_windows::present_frame(frame,&self.window)?;").unwrap();
    let after =
        reblit.find("decide_frame_outcome(before,before,self.device_errors.gate())").unwrap();
    let check = reblit.find("if!outcome.acknowledges()").unwrap();
    let done = reblit.find("Ok(PresentOutcome::CachedReblit)").unwrap();
    assert!(count < present && present < after && after < check && check < done);
    // The admitted scope is transferred, not recreated or replaced by a gate reading.
    assert!(source.contains(
        "fnpresent_unchanged_frame(&mutself,before:DeviceGate,reblit_scope:GpuWorkScope,"
    ));
    assert!(source
        .contains("fnreblit_software_frame(&mutself,before:DeviceGate,reblit_scope:GpuWorkScope,"));
    assert!(source.contains("self.reblit_software_frame(before,reblit_scope)"));
    let core = compact(include_str!("core.rs"));
    let unchanged = source_between(&core, "ifplan.unchanged{", "ifplan.mode==RenderMode::Noop{");
    let prepare = unchanged.find("self.prepare_cached_present()").unwrap();
    let gate = unchanged.find("self.device_errors.enter_gpu_work(\"render.reblit\")").unwrap();
    let handoff = unchanged.find("self.present_unchanged_frame(before,reblit_scope)?").unwrap();
    assert!(prepare < gate && gate < handoff);
    assert!(unchanged.contains("returnOk(self.rendering_unavailable());"));
    assert!(unchanged.contains(
        "ifmatches!(outcome,PresentOutcome::RenderingUnavailable(_)){returnOk(outcome);}"
    ));
    assert!(
        unchanged.find("returnOk(outcome);").unwrap()
            < unchanged.find("self.window.request_redraw();").unwrap()
    );
}
