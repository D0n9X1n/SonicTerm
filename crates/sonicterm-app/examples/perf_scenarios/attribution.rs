//! S10 per-present attribution: arming the App's watch for the S10 pane before GO, and the record
//! `result.json` carries for the phase.
//!
//! Every call into the App's attribution API, and the parser read it depends on, sits behind
//! `#[cfg(perf_s10_attribution_api)]`. perf-compare passes that cfg to both sides only when both
//! trees define the API, so the overlaid harness also builds on a tree that predates it; that build
//! compiles the no-call fallback below, which reads attribution `unavailable`.
#![cfg_attr(not(any(target_os = "macos", windows)), allow(dead_code))]

use serde::Serialize;

/// Whether this build calls the App's attribution API: the effective cfg, recorded in `result.json`.
pub(crate) const API_ENABLED: bool = cfg!(perf_s10_attribution_api);

/// One pane's synchronized-output state as the parser holds it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct SyncReading {
    /// Whether an update is open.
    pub(crate) set: bool,
    /// Reset-to-set transitions.
    pub(crate) epoch: u64,
    /// Set-to-reset transitions.
    pub(crate) resets: u64,
}

/// What arming the App's watch returned.
// Each build's adapter builds only its own variants: `Disabled` without the cfg, the other two with it.
#[allow(dead_code)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ArmResult {
    /// This build does not call the API.
    Disabled,
    /// The App armed nothing: its gate is off, or it is the prerequisite's stub.
    NotArmed,
    /// The App armed a watch with this id.
    Armed(u64),
}

/// The S10 phase's attribution, as `result.json` records it.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "state", rename_all = "kebab-case")]
pub(crate) enum Attribution {
    /// No watch ran, for `reason`; the comparison reads it as unavailable, never as passed.
    Unavailable { reason: &'static str },
    /// A watch ran for the whole phase.
    Armed(ArmedRecord),
}

/// An armed phase: the arming, its baseline before GO and the state and presented counts around the phase.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub(crate) struct ArmedRecord {
    /// The arming id every line of this watch carries.
    pub(crate) arming: u64,
    /// The watched pane.
    pub(crate) pane: u64,
    /// The phase's logical updates.
    pub(crate) updates: u32,
    /// The pane's state before GO; its update is closed.
    pub(crate) baseline: SyncReading,
    /// The main renderer's presented count when the phase started.
    pub(crate) seq_start: u64,
    /// The main renderer's presented count when the phase ended; `None` while the phase runs.
    pub(crate) seq_end: Option<u64>,
    /// The pane's state when the phase ended; `None` while the phase runs or when it could not be read.
    pub(crate) end: Option<SyncReading>,
}

/// What to do before GO.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Start {
    /// Record attribution unavailable for this reason; nothing is armed.
    Skip(&'static str),
    /// The run is void: an update was already open before GO, so update 1 cannot be told apart.
    Refuse(String),
    /// Arm the watch with this baseline.
    Arm(SyncReading),
}

/// The step before GO for a phase that plays counted updates: attribution runs only in a counting
/// run of a build that calls the API, and only from a closed baseline. `read_baseline` reads the
/// pane's state under the parser lock, so it runs only after both checks pass, and at most once.
pub(crate) fn start(
    counting: bool,
    api_enabled: bool,
    read_baseline: impl FnOnce() -> Option<SyncReading>,
) -> Start {
    if !api_enabled {
        return Start::Skip("api-disabled");
    }
    if !counting {
        // When: `counting` is false, the App's frame-counter gate is off and the watch records nothing.
        return Start::Skip("counters-off");
    }
    match read_baseline() {
        None => Start::Skip("no-baseline"),
        Some(reading) if reading.set => Start::Refuse(format!(
            "S10's pane had an update open before GO (epoch {}, resets {}), so update 1 cannot be attributed",
            reading.epoch, reading.resets
        )),
        Some(reading) => Start::Arm(reading),
    }
}

/// The phase's record once arming returned `outcome` for `pane` at presented count `seq_start`.
pub(crate) fn armed(
    outcome: ArmResult,
    pane: u64,
    updates: u32,
    baseline: SyncReading,
    seq_start: u64,
) -> Attribution {
    match outcome {
        ArmResult::Disabled => Attribution::Unavailable { reason: "api-disabled" },
        ArmResult::NotArmed => Attribution::Unavailable { reason: "not-armed" },
        ArmResult::Armed(arming) => Attribution::Armed(ArmedRecord {
            arming,
            pane,
            updates,
            baseline,
            seq_start,
            seq_end: None,
            end: None,
        }),
    }
}

/// The App's attribution API, called only in a build with the cfg.
#[cfg(all(perf_s10_attribution_api, any(target_os = "macos", windows)))]
pub(crate) mod api {
    use sonicterm_app::app::App;

    use super::{ArmResult, SyncReading};

    /// Pane `pane_id`'s synchronized-output state, read under its parser lock; `None` when the pane
    /// is not in the main window.
    pub(crate) fn read_sync(app: &App, pane_id: u64) -> Option<SyncReading> {
        let pane = app.main_panes()?.get(&pane_id)?;
        let state = pane.parser.lock().synchronized_output();
        Some(SyncReading { set: state.set, epoch: state.epoch, resets: state.resets })
    }

    /// Arm `pane_id`'s watch with the sentinel line, the prompt and the phase's update count.
    pub(crate) fn arm(
        app: &mut App,
        pane_id: u64,
        sentinel: &str,
        prompt: &str,
        updates: u32,
    ) -> ArmResult {
        match app.arm_s10_attribution(pane_id, sentinel, prompt, updates) {
            Some(arming) => ArmResult::Armed(arming),
            None => ArmResult::NotArmed,
        }
    }

    /// Disarm `pane_id`'s watch.
    pub(crate) fn disarm(app: &mut App, pane_id: u64) {
        app.disarm_s10_attribution(pane_id);
    }
}

/// Without the cfg the App is never called: nothing is read or armed, and attribution is unavailable.
#[cfg(all(not(perf_s10_attribution_api), any(target_os = "macos", windows)))]
pub(crate) mod api {
    use sonicterm_app::app::App;

    use super::{ArmResult, SyncReading};

    /// Nothing is read in this build.
    pub(crate) fn read_sync(_app: &App, _pane_id: u64) -> Option<SyncReading> {
        None
    }

    /// Nothing is armed in this build.
    pub(crate) fn arm(
        _app: &mut App,
        _pane_id: u64,
        _sentinel: &str,
        _prompt: &str,
        _updates: u32,
    ) -> ArmResult {
        ArmResult::Disabled
    }

    /// Nothing was armed, so nothing is disarmed.
    pub(crate) fn disarm(_app: &mut App, _pane_id: u64) {}
}

#[cfg(test)]
#[path = "attribution_tests.rs"]
mod attribution_tests;
