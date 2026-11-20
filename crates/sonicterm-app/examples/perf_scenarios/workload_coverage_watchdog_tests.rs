use super::*;

/// A child that exits after `exits_after` checks, whose kill fails when `kill_fails`, and which
/// is reaped `reaped_after_kill` checks after a kill, or never.
struct ScriptedChild {
    checks: u32,
    exits_after: Option<(u32, bool)>,
    kill_fails: bool,
    killed: bool,
    reaped_after_kill: Option<u32>,
}

impl ScriptedChild {
    fn new(exits_after: Option<(u32, bool)>) -> Self {
        Self {
            checks: 0,
            exits_after,
            kill_fails: false,
            killed: false,
            reaped_after_kill: Some(0),
        }
    }
}

impl Supervised for ScriptedChild {
    fn try_wait(&mut self) -> io::Result<Option<ChildExit>> {
        self.checks += 1;
        if self.killed {
            let reaped = self.reaped_after_kill.is_some_and(|after| {
                self.reaped_after_kill = after.checked_sub(1);
                after == 0
            });
            return Ok(reaped.then(|| ChildExit { success: false, status: "killed".into() }));
        }
        Ok(self.exits_after.and_then(|(after, success)| {
            (self.checks > after)
                .then(|| ChildExit { success, status: format!("exit {}", u8::from(!success)) })
        }))
    }

    fn kill(&mut self) -> io::Result<()> {
        if self.kill_fails {
            return Err(io::Error::other("access denied"));
        }
        self.killed = true;
        Ok(())
    }

    fn pid(&self) -> u32 {
        4242
    }
}

/// Virtual time that only `sleep` advances.
#[derive(Default)]
struct VirtualClock {
    elapsed: Duration,
}

impl WatchClock for VirtualClock {
    fn elapsed(&self) -> Duration {
        self.elapsed
    }

    fn sleep(&mut self, step: Duration) {
        self.elapsed += step;
    }
}

const BOUNDS: WatchBounds = WatchBounds {
    deadline: Duration::from_secs(90),
    reap: Duration::from_secs(5),
    poll: Duration::from_millis(100),
};

/// A child that exits before the deadline decides the run by its own result.
#[test]
fn an_exited_child_decides_the_run() {
    let mut clock = VirtualClock::default();
    assert_eq!(
        supervise(&mut ScriptedChild::new(Some((3, true))), &mut clock, BOUNDS),
        CoverageVerdict::Passed
    );
    let verdict = supervise(&mut ScriptedChild::new(Some((3, false))), &mut clock, BOUNDS);
    assert_eq!(verdict, CoverageVerdict::Failed("the coverage child failed: exit 1".into()));
}

/// A child still running at the deadline that is killed and reaped is a host limit.
#[test]
fn a_blocked_child_that_is_stopped_is_host_incapable() {
    let mut clock = VirtualClock::default();
    let verdict = supervise(&mut ScriptedChild::new(None), &mut clock, BOUNDS);
    assert_eq!(verdict, CoverageVerdict::HostIncapable);
    assert!(clock.elapsed > BOUNDS.deadline, "the deadline passed before the kill");
}

/// A kill that fails leaves the child possibly running, so the run fails and names the pid.
#[test]
fn a_failed_kill_fails_the_run() {
    let mut child = ScriptedChild::new(None);
    child.kill_fails = true;
    let verdict = supervise(&mut child, &mut VirtualClock::default(), BOUNDS);
    assert_eq!(
        verdict,
        CoverageVerdict::Failed("could not stop coverage child pid 4242: access denied".into())
    );
}

/// A killed child that is not reaped within the reap bound fails the run instead of waiting
/// forever, and the wait after the kill stops at that bound.
#[test]
fn a_child_never_reaped_fails_within_the_bound() {
    let mut child = ScriptedChild::new(None);
    child.reaped_after_kill = None;
    let mut clock = VirtualClock::default();
    let verdict = supervise(&mut child, &mut clock, BOUNDS);
    assert_eq!(
        verdict,
        CoverageVerdict::Failed("coverage child pid 4242 still running after kill".into())
    );
    assert!(
        clock.elapsed <= BOUNDS.deadline + BOUNDS.reap + BOUNDS.poll * 2,
        "{:?}",
        clock.elapsed
    );
}

/// A killed child reaped a few checks later is still stopped in time.
#[test]
fn a_child_reaped_after_some_checks_is_host_incapable() {
    let mut child = ScriptedChild::new(None);
    child.reaped_after_kill = Some(3);
    assert_eq!(
        supervise(&mut child, &mut VirtualClock::default(), BOUNDS),
        CoverageVerdict::HostIncapable
    );
}
