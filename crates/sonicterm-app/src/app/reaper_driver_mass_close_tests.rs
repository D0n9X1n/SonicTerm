//! Pins the reaper under a mass close: with its admitted runs held, 32 retired PTYs split into the
//! tasks it can admit and bounded synchronous fallbacks, the driver counts every one on exactly one
//! path, the fallbacks' total blocked time is recorded and bounded, and once the runs are released
//! every admitted task settles and the root's reaper work returns to zero.

use super::*;
use sonicterm_types::{GovernorLimits, ProcessKind};

/// PTYs closed at once, past the two tasks the driver admits.
const CLOSED: usize = 32;
/// Tasks the driver admits before falling back.
const ADMITTED: usize = 2;

/// A governor with no limits, so only the driver's own ceilings decide admission.
fn governor() -> ResourceGovernor {
    ResourceGovernor::new(
        ProcessKind::Gui,
        GovernorLimits {
            process_bytes: usize::MAX,
            class_bytes: enum_map::enum_map! { _ => usize::MAX },
            class_items: enum_map::enum_map! { _ => None },
        },
    )
    .unwrap()
}

/// The root's live reaper work items.
fn reaper_work(governor: &ResourceGovernor) -> usize {
    governor.snapshot(governor.root_owner()).unwrap().process_class_items[ResourceClass::ReaperWork]
}

/// Opens the run hold however the test ends, so a failed assertion never leaves the driver's
/// worker waiting forever.
struct OpenOnDrop(Arc<RunHold>);

// Lifecycle: OpenOnDrop drop calls RunHold::set(false), releasing the driver's held runs.
impl Drop for OpenOnDrop {
    fn drop(&mut self) {
        self.0.set(false);
    }
}

/// 32 PTYs retired while the driver's runs are held: exactly two are admitted and thirty fall
/// back; the fallbacks' summed blocked time is recorded and within thirty native tail bounds; and
/// after the hold opens, the admitted tasks settle and reaper work returns to zero.
#[test]
fn a_mass_close_splits_between_admission_and_bounded_fallback() {
    if super::super::pty_test_support::isolated() {
        return;
    }
    let governor = governor();
    let mut driver = ReaperDriver::with_limits(
        governor.clone(),
        ReaperLimits::new(
            ADMITTED,
            ADMITTED * PTY_NATIVE_HELPER_DEMAND,
            ADMITTED * PTY_NATIVE_HANDLE_DEMAND,
        )
        .unwrap(),
    )
    .unwrap();
    // Held runs keep admitted tasks in their slots, so capacity cannot recycle mid-close.
    driver.hold.set(true);
    let opener = OpenOnDrop(Arc::clone(&driver.hold));
    let mut pids = Vec::with_capacity(CLOSED);
    let mut paths = Vec::with_capacity(CLOSED);
    for _ in 0..CLOSED {
        let pty = PtyHandle::spawn_with_args("/bin/sh", &["-s".to_owned()], 80, 24).unwrap();
        let pid = pty.pid().unwrap();
        super::super::pty_test_support::record_process(pid, true);
        pids.push(pid);
        paths.push(driver.retire(pty, None));
    }
    let admitted = paths.iter().filter(|path| **path == RetirementPath::Reaper).count();
    assert_eq!(driver.path_counts(), (admitted, CLOSED - admitted), "every close on one path");
    assert_eq!(admitted, ADMITTED, "{paths:?}");
    let timing = driver.fallback_timing();
    assert_eq!(timing.count, (CLOSED - ADMITTED) as u64);
    assert!(timing.total_us >= timing.longest_us, "the total sums every fallback");
    let bound = PTY_TEARDOWN_TAIL_BOUND.saturating_mul((CLOSED - ADMITTED) as u32);
    assert!(
        Duration::from_micros(timing.total_us) <= bound,
        "fallbacks blocked {} us, past {bound:?}",
        timing.total_us
    );
    assert_eq!(reaper_work(&governor), ADMITTED, "only the admitted tasks hold reaper work");
    drop(opener);
    assert!(driver.finish(), "the admitted tasks settled once released");
    assert_eq!(reaper_work(&governor), 0);
    for pid in pids {
        super::super::pty_test_support::record_process(pid, false);
    }
}
