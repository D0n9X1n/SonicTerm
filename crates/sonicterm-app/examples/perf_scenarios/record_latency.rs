//! Credited latency facts, causal split and per-run latency reporting.

use super::*;

/// Settle an open sample from one dispatch.
///
/// The candidate is the first dispatch that presents with the echo already in the grid before
/// it; it is credited only when the grid's revision is unchanged after it. A busy lock at a
/// presenting dispatch, an echo that first appears during one, or a revision change during the
/// candidate leaves the sample unattributed rather than credited to another frame.
pub(crate) fn attribute_dispatch(observation: &DispatchObservation) -> Attribution {
    if !observation.advanced {
        // When: the dispatch presented nothing, it can neither show nor hide the echo.
        return Attribution::Pending;
    }
    match (observation.before, observation.after) {
        (EchoSnapshot::Busy, _) | (_, EchoSnapshot::Busy) => {
            Attribution::Unattributed(UnattributedReason::LockBusy)
        }
        (
            EchoSnapshot::Read { present: true, revision: before, .. },
            EchoSnapshot::Read { revision: after, .. },
        ) => {
            if before == after {
                Attribution::Credited
            } else {
                // When: output was parsed during the frame, the presented grid is unknown.
                Attribution::Unattributed(UnattributedReason::RevisionChanged)
            }
        }
        (EchoSnapshot::Read { present: false, .. }, EchoSnapshot::Read { present: true, .. }) => {
            Attribution::Unattributed(UnattributedReason::EchoDuringFrame)
        }
        (EchoSnapshot::Read { present: false, .. }, EchoSnapshot::Read { present: false, .. }) => {
            Attribution::Pending
        }
    }
}

/// Whether a sample of `scenario`/`variant` is split at the flush and armed for its echo timeline: S2's
/// single idle pane and S2/flood's typing pane beside the flood. Both sides of a comparison arm the same
/// variants with the same overlaid harness, so the watch's cost is symmetric within a run.
#[cfg_attr(not(any(target_os = "macos", windows)), allow(dead_code))]
pub(crate) fn split_in_scope(scenario: &str, variant: &str) -> bool {
    scenario == "S2" && matches!(variant, "default" | "flood")
}

/// The outcome for a credited sample armed as `arm`: `take` runs only for an armed token.
pub(crate) fn echo_outcome(
    arm: ArmState,
    take: impl FnOnce(EchoToken) -> EchoOutcome,
) -> EchoOutcome {
    match arm {
        ArmState::OutOfScope | ArmState::Unsupported => EchoOutcome::Unsupported,
        ArmState::Failed(reason) => EchoOutcome::Failed(reason),
        ArmState::Armed(token) => take(token),
    }
}

/// Nanoseconds as milliseconds.
pub(super) fn ns_to_ms(nanoseconds: u64) -> f64 {
    nanoseconds as f64 / 1_000_000.0
}

/// A duration in whole nanoseconds, or `None` when it does not fit in a `u64`.
pub(crate) fn duration_ns(duration: Duration) -> Option<u64> {
    u64::try_from(duration.as_nanos()).ok()
}

/// Split a sample injected at `injected` and credited to a dispatch that ended at `ended`, or name
/// the first reason in `SPLIT_REASONS` order that it cannot be. `harness_identity_changed` is the
/// harness's own snapshot check of the armed row identity.
pub(crate) fn split_for(
    injected: Instant,
    ended: Instant,
    outcome: &EchoOutcome,
    harness_identity_changed: bool,
) -> Result<Split, &'static str> {
    let facts = match outcome {
        EchoOutcome::Unsupported => return Err("unsupported"),
        EchoOutcome::Failed(reason) => return Err(reason),
        EchoOutcome::Taken(facts) => facts,
    };
    if !facts.shown {
        return Err("pane-not-shown");
    }
    if facts.identity_changed || harness_identity_changed {
        return Err("row-identity-changed");
    }
    if facts.pre_present {
        return Err("first-appearance-unobserved");
    }
    let Some(appearance) = facts.appearance else {
        return Err("no-appearance-observed");
    };
    if facts.lost {
        return Err("echo-overwritten");
    }
    let Some(publication) = facts.publication else {
        return Err("no-publication-observed-by-take");
    };
    match publication.window {
        PublicationWindow::Untargeted => return Err("untargeted"),
        PublicationWindow::Other => return Err("other-window"),
        PublicationWindow::Main => {}
    }
    let delivery = match facts.delivery {
        None => return Err("delivery-not-observed-by-take"),
        Some(DeliveryFacts { outcome: Delivery::Refused, .. }) => return Err("send-refused"),
        Some(delivery) => delivery,
    };
    if ended < publication.published_at {
        return Err("presented-before-publication");
    }
    if delivery.decided_at > ended {
        return Err("delivered-after-present");
    }
    // Checked intervals only: an out-of-order or unrepresentable endpoint is never saturated. Order is
    // compared first: Windows' `checked_duration_since` reads a reversal inside its clock epsilon as zero.
    let part = |from: Instant, to: Instant| {
        (to >= from).then(|| to.duration_since(from)).and_then(duration_ns)
    };
    let parts = (
        part(injected, appearance.parsed_at),
        part(appearance.parsed_at, publication.published_at),
        part(publication.published_at, ended),
        part(publication.published_at, delivery.decided_at),
    );
    let (
        Some(input_to_parse_ns),
        Some(parse_to_publication_ns),
        Some(publication_to_present_ns),
        Some(delivery_lag_ns),
    ) = parts
    else {
        return Err("clock-order");
    };
    Ok(Split {
        input_to_parse_ns,
        parse_to_publication_ns,
        publication_to_present_ns,
        delivery_lag_ns,
        delivery: delivery.outcome,
        coalesced: publication.coalesced,
        sync_open: appearance.sync_open,
        echo_generation: appearance.generation,
    })
}

impl LatencySample {
    /// A sample no frame was credited with, for `reason`.
    pub(crate) fn uncredited(inject_unix_s: f64, reason: &'static str) -> Self {
        Self {
            inject_unix_s,
            latency_ms: None,
            reason,
            split: None,
            split_reason: NOT_CREDITED,
            dispatch_timeline: TIMELINE_NOT_TAKEN,
            echo_timeline: None,
        }
    }
}

impl LatencySample {
    /// This sample with its dispatch timeline's disposition, recorded at its close.
    pub(crate) fn with_dispatch_timeline(self, dispatch_timeline: &'static str) -> Self {
        Self { dispatch_timeline, ..self }
    }
}

impl LatencySample {
    /// This sample with its credited echo timeline.
    #[cfg_attr(not(any(target_os = "macos", windows)), allow(dead_code))]
    pub(crate) fn with_timeline(
        self,
        echo_timeline: Option<crate::timeline::TimelineSample>,
    ) -> Self {
        Self { echo_timeline, ..self }
    }
}

impl LatencySample {
    /// A sample injected at `injected` and credited to the dispatch that ended at `ended`, split
    /// from the echo watch's `outcome` when it can be.
    pub(crate) fn credited(
        inject_unix_s: f64,
        injected: Instant,
        ended: Instant,
        outcome: &EchoOutcome,
        harness_identity_changed: bool,
    ) -> Self {
        let latency_ms = ended.saturating_duration_since(injected).as_secs_f64() * 1_000.0;
        let (split, split_reason) =
            match split_for(injected, ended, outcome, harness_identity_changed) {
                Ok(split) => (Some(split), SPLIT),
                Err(reason) => (None, reason),
            };
        debug_assert!(
            SPLIT_REASONS.contains(&split_reason),
            "{split_reason} is not a split reason"
        );
        Self {
            inject_unix_s,
            latency_ms: Some(latency_ms),
            reason: CREDITED,
            split,
            split_reason,
            dispatch_timeline: TIMELINE_NOT_TAKEN,
            echo_timeline: None,
        }
    }
}

#[cfg(test)]
#[path = "record_latency_tests.rs"]
mod record_latency_tests;
