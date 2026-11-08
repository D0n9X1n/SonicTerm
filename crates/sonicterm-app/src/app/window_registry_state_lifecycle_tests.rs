//! A seeded state machine over a headless App: splits, closes, tab transfers between windows,
//! tear-out into a fresh window, waits on pane signals, output and process exits, in random order.
//! After every step each pane belongs to exactly one window and `locate_pane` agrees with a scan;
//! each live pane's owner is open, parented to its window's owner, and holds exactly the charges
//! the pane carries (checked as the step left it, so a move must carry them itself); every retired
//! pane's signal and owner are closed. Real MCP waits stay pending across steps and must end
//! `pane_closed` when their pane goes, at least once per seed. One pane runs a real process: its
//! retirement puts the transport in the reaper's custody, one `ReaperWork` item while teardown is
//! held back, released once teardown settles. Each seed must run every kind of step at least once.

use std::time::{Duration, Instant};

use sonicterm_cfg::keymap::Direction;
use sonicterm_cfg::{config::Config, keymap::Keymap, theme::Theme};
use sonicterm_types::PaneExitStatus;

use crate::app::{App, PaneSignal, PaneState, WindowId};

/// Steps per seed, and the seeds the gate runs; `SONICTERM_LIFECYCLE_SEED` runs one more.
const STEPS: usize = 300;
const SEEDS: [u64; 8] = [1, 2, 3, 5, 8, 13, 21, 34];

/// The kinds of step, so a seed can prove it ran each one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Step {
    Split,
    Close,
    Transfer,
    TearOut,
    Wait,
    Output,
    Exit,
}

/// A small xorshift generator so every seed replays the same sequence.
struct Rng(u64);

impl Rng {
    /// A value in `0..bound`.
    fn below(&mut self, bound: usize) -> usize {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 % bound.max(1) as u64) as usize
    }
}

/// Every live pane as (window, pane), in a stable order for picking.
fn panes(app: &App) -> Vec<(WindowId, u64)> {
    let mut all: Vec<_> = app
        .registry
        .windows
        .iter()
        .flat_map(|(window, state)| state.panes.keys().map(move |pane| (*window, *pane)))
        .collect();
    all.sort_by_key(|(_, pane)| *pane);
    all
}

/// Every window, main first, in a stable order for picking.
fn windows(app: &App) -> Vec<WindowId> {
    let main = app.registry.main_window_id;
    let mut ids: Vec<_> =
        app.registry.windows.keys().copied().filter(|id| Some(*id) != main).collect();
    ids.sort_by_key(|id| format!("{id:?}"));
    main.into_iter().chain(ids).collect()
}

/// Split `pane` in its window with a PTY-less pane, as a split does, and give it its owner.
fn split(app: &mut App, window: WindowId, pane: u64) -> bool {
    let new_id = crate::app::next_pane_id();
    let parser = std::sync::Arc::new(parking_lot::Mutex::new(
        sonicterm_vt::vt::Parser::new_with_staging_pool(
            sonicterm_grid::grid::Grid::new(40, 12),
            None,
            std::sync::Arc::clone(&app.capture_staging_pool),
        ),
    ));
    let state = PaneState::new_with_media_pool(parser, None, &app.inline_media_pool);
    let Some(target) = app.registry.windows.get_mut(&window) else {
        return false;
    };
    let Some(tab) = target.tab_states.iter_mut().find(|tab| tab.tree.contains_pane(pane)) else {
        return false;
    };
    if !tab.tree.split(pane, Direction::Right, new_id) {
        return false;
    }
    target.panes.insert_for_test(new_id, state);
    app.reattribute_pane_owners();
    true
}

/// The invariants every step keeps.
fn check(app: &App, retired: &[Retired], context: &str) {
    assert_eq!(app.registry.duplicate_pane(), None, "{context}: a pane in two windows");
    for (window, pane) in panes(app) {
        let slot = app.registry.locate_pane(pane);
        let scanned = app.registry.windows[&window]
            .tab_states
            .iter()
            .position(|tab| tab.tree.contains_pane(pane));
        assert_eq!(
            slot.map(|slot| (slot.window_id, slot.tab_index)),
            scanned.map(|tab| (window, tab)),
            "{context}: pane {pane}"
        );
        let owner =
            app.registry.windows[&window].panes[&pane].owner.as_ref().map(|guard| guard.id());
        let owner = owner.unwrap_or_else(|| panic!("{context}: pane {pane} has no owner"));
        assert!(app.__test_owner_is_open(owner), "{context}: pane {pane}'s owner is closed");
        // The owner hangs under the window that holds the pane, and its ledger holds exactly the
        // charges the pane carries: a transfer moved them with it rather than leaving them behind.
        let snapshot = app.__test_owner_snapshot(owner).expect("an open owner");
        let window_owner = app.registry.windows[&window].owner.as_ref().map(|guard| guard.id());
        assert_eq!(snapshot.parent, window_owner, "{context}: pane {pane}'s owner parent");
        // A pane new since the last sample has no charges yet; every other one carries some.
        let charged = app.__test_pane_charge_total(window, pane).unwrap();
        assert_eq!(snapshot.owner_amount.bytes, charged, "{context}: pane {pane}'s ledger amount");
    }
    for gone in retired {
        assert!(gone.signal.is_closed(), "{context}: a retired pane's signal is open");
        // A wait from the signal's current sequence would sleep to its deadline on an open signal;
        // on a closed one it ends at once.
        let started = Instant::now();
        gone.signal.wait_past(gone.signal.sequence(), started + Duration::from_secs(1));
        assert!(
            started.elapsed() < Duration::from_millis(100),
            "{context}: a wait outlived its pane"
        );
        // The pane's governor owner closed with it, so its charges left the live accounting.
        if let Some(owner) = gone.owner {
            assert!(!app.__test_owner_is_open(owner), "{context}: a retired pane's owner is open");
        }
    }
}

/// Retired transports the reaper holds in custody: process-wide `ReaperWork` items.
fn reaper_work(app: &App) -> usize {
    app.__test_governor_snapshot_root().process_class_items
        [sonicterm_types::ResourceClass::ReaperWork]
}

/// The retired PTY is in the reaper's custody, charged one `ReaperWork` item while its teardown is
/// held back; once the hold opens, teardown settles and the charge is released.
fn assert_custody(app: &App, before: usize, context: &str) {
    assert_eq!(reaper_work(app), before + 1, "{context}: the retired transport is in custody");
    app.services.pty_reaper.hold.set(false);
    let settled_by = Instant::now() + Duration::from_secs(10);
    while reaper_work(app) != before {
        assert!(Instant::now() < settled_by, "{context}: the retired transport never settled");
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// A pane that left the App: its change signal and its governor owner, if it had one.
struct Retired {
    signal: std::sync::Arc<PaneSignal>,
    owner: Option<sonicterm_types::ResourceOwnerId>,
}

/// An MCP `wait_for` on a pane, running on its own thread across later steps, and the pane it
/// waits on.
struct PendingWait {
    pane: u64,
    waiter: std::thread::JoinHandle<crate::app::mcp::tools_read::WaitReason>,
}

/// Start a real `wait_for` on `pane` that only its pane's end can finish: no pattern matches,
/// and its timeout is far past the run.
fn start_wait(
    app: &App,
    shared: &std::sync::Arc<crate::app::mcp::McpShared>,
    pane: u64,
) -> Option<PendingWait> {
    let entry = app.mcp_resolve_pane(pane)?;
    let shared = std::sync::Arc::clone(shared);
    let waiter = std::thread::spawn(move || {
        let scope = crate::app::mcp::CallScope::admit(
            &shared,
            &std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        );
        let condition = crate::app::mcp::tools_read::WaitCondition {
            regex: Some(regex::Regex::new("a pattern no pane prints").unwrap()),
            after: None,
            quiet: None,
            watch_exit: false,
            timeout: Duration::from_secs(120),
            recheck: Duration::from_millis(250),
        };
        crate::app::mcp::tools_read::wait_until(&shared, &entry, &condition, &scope).reason
    });
    Some(PendingWait { pane, waiter })
}

/// One seed's run: `STEPS` random steps; returns which kinds ran.
fn run(seed: u64) -> std::collections::HashSet<Step> {
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    app.__test_seed_tab("main-0");
    app.__test_seed_tab("main-1");
    app.__test_seed_child_window(&["child-0", "child-1"]);
    // Seeded panes get owners as spawned panes do when they join a window, then their charges.
    app.reattribute_pane_owners();
    app.__test_force_retention_sample();
    let outer = sonicterm_ui::pane::Rect::new(0.0, 0.0, 800.0, 500.0);
    let mut rng = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
    let mut retired: Vec<Retired> = Vec::new();
    let shared = crate::app::mcp::McpShared::new(sonicterm_cfg::config::McpControl::Any, None);
    let mut waits: Vec<PendingWait> = Vec::new();
    // One pane runs a real process, so its retirement must hand a PTY to the reaper.
    #[cfg(unix)]
    let pty_pane = {
        let pane = app.__test_seed_tab("with-pty");
        let pty =
            sonicterm_io::pty::PtyHandle::spawn_with_args("/bin/sleep", &["60".into()], 80, 24)
                .unwrap();
        app.registry.pane_mut(pane).unwrap().pty = Some(pty);
        app.reattribute_pane_owners();
        app.__test_force_retention_sample();
        // Teardown waits until the custody check below has seen the retired transport.
        app.services.pty_reaper.hold.set(true);
        Some(pane)
    };
    #[cfg(not(unix))]
    let pty_pane: Option<u64> = None;
    let mut pty_retired = false;
    let mut closed_waits = 0;
    let mut ran = std::collections::HashSet::new();
    for step in 0..STEPS {
        // Every window needs geometry for a transfer; a fresh child gets it here too.
        assert!(app.__test_set_main_pane_viewport(outer, 10.0, 20.0));
        for window in windows(&app).into_iter().skip(1) {
            app.__test_set_child_pane_viewport(window, outer, 10.0, 20.0);
        }
        let live = panes(&app);
        let windows_now = windows(&app);
        let (window, pane) = live[rng.below(live.len())];
        let signal =
            std::sync::Arc::clone(&app.registry.windows[&window].panes[&pane].change_signal);
        let owner =
            app.registry.windows[&window].panes[&pane].owner.as_ref().map(|guard| guard.id());
        let reaped_before = app.services.pty_reaper.path_counts();
        let reaper_work_before = reaper_work(&app);
        let kind = match rng.below(7) {
            0 if rng.below(2) == 0 => {
                // A new tab, so later transfers and tear-outs have tabs to move.
                app.__test_seed_tab("added");
                app.reattribute_pane_owners();
                None
            }
            0 => split(&mut app, window, pane).then_some(Step::Split),
            1 if live.len() > 2 => {
                let closed = matches!(
                    app.mcp_close_pane(sonicterm_cfg::config::McpControl::Any.into(), pane),
                    crate::app::mcp::LoopReply::Closed
                );
                closed.then(|| {
                    retired.push(Retired { signal, owner });
                    Step::Close
                })
            }
            2 if windows_now.len() > 1 => {
                // The source is a window with a tab to spare and the target any other window, so
                // every draw of this step attempts a real transfer.
                let sources: Vec<WindowId> = windows_now
                    .iter()
                    .copied()
                    .filter(|window| app.registry.windows[window].tab_states.len() > 1)
                    .collect();
                (!sources.is_empty())
                    .then(|| {
                        let source = sources[rng.below(sources.len())];
                        let others: Vec<WindowId> = windows_now
                            .iter()
                            .copied()
                            .filter(|window| *window != source)
                            .collect();
                        let target = others[rng.below(others.len())];
                        let tabs = app.registry.windows[&source].tab_states.len();
                        let target_len = app.registry.windows[&target].tab_states.len();
                        app.transfer_tab(Some(source), rng.below(tabs), Some(target), target_len)
                    })
                    .and_then(Result::ok)
                    .map(|()| Step::Transfer)
            }
            3 => {
                // Tear-out without an event loop: a fresh child takes one tab of a window that has
                // two or more, as `tear_out_tab` does after creating the native window.
                let source = windows_now[rng.below(windows_now.len())];
                let tabs = app.registry.windows[&source].tab_states.len();
                (tabs > 1)
                    .then(|| {
                        let child = app.__test_seed_child_window(&["torn"]);
                        let placeholder = app.registry.windows[&child].tab_states[0].active_pane;
                        app.__test_set_child_pane_viewport(child, outer, 10.0, 20.0);
                        app.transfer_tab(Some(source), rng.below(tabs), Some(child), 1).ok()?;
                        let removed =
                            app.registry.windows[&child].panes[&placeholder].change_signal.clone();
                        let removed_owner = app.registry.windows[&child].panes[&placeholder]
                            .owner
                            .as_ref()
                            .map(|guard| guard.id());
                        matches!(
                            app.mcp_close_pane(
                                sonicterm_cfg::config::McpControl::Any.into(),
                                placeholder
                            ),
                            crate::app::mcp::LoopReply::Closed
                        )
                        .then(|| retired.push(Retired { signal: removed, owner: removed_owner }))?;
                        Some(Step::TearOut)
                    })
                    .flatten()
            }
            4 => {
                // A real MCP wait starts on this pane and stays pending across the steps that follow.
                start_wait(&app, &shared, pane).map(|wait| {
                    waits.push(wait);
                    Step::Wait
                })
            }
            5 => {
                let pane_state = &app.registry.windows[&window].panes[&pane];
                crate::app::spawn_pane::publish_grid_change(
                    &pane_state.output_generation,
                    &pane_state.change_signal,
                );
                Some(Step::Output)
            }
            _ if live.len() > 2 => {
                app.handle_pane_process_exited(pane, PaneExitStatus::Code(0));
                (app.registry.pane(pane).is_none()).then(|| {
                    retired.push(Retired { signal, owner });
                    Step::Exit
                })
            }
            _ => None,
        };
        if let Some(kind) = kind {
            ran.insert(kind);
        }
        let context = format!("seed {seed} step {step} ({kind:?})");
        // A transfer or tear-out moved charged panes: the move itself must carry their charges to
        // the destination owner, checked before reattribution or a sample could repair them. A split
        // or new tab adds an uncharged pane, which the sample then charges.
        check(&app, &retired, &format!("{context} as the step left it"));
        app.reattribute_pane_owners();
        app.__test_force_retention_sample();
        // The PTY pane's retirement handed its process to the reaper, by queue or fallback.
        if Some(pane) == pty_pane && app.registry.pane(pane).is_none() && !pty_retired {
            pty_retired = true;
            assert_custody(&app, reaper_work_before, &context);
            let (reaped, fallback) = app.services.pty_reaper.path_counts();
            assert_eq!(
                reaped + fallback,
                reaped_before.0 + reaped_before.1 + 1,
                "{context}: PTY custody"
            );
        }
        // Every wait on a pane that is gone ends with `pane_closed` within the close drain.
        let (ended, pending): (Vec<_>, Vec<_>) =
            waits.drain(..).partition(|wait| app.registry.pane(wait.pane).is_none());
        waits = pending;
        for wait in ended {
            let deadline = Instant::now() + Duration::from_secs(3);
            while !wait.waiter.is_finished() {
                assert!(Instant::now() < deadline, "{context}: a wait outlived pane {}", wait.pane);
                std::thread::sleep(Duration::from_millis(5));
            }
            let reason = wait.waiter.join().unwrap();
            assert_eq!(reason, crate::app::mcp::tools_read::WaitReason::PaneClosed, "{context}");
            closed_waits += 1;
        }
        check(&app, &retired, &context);
        // After the sample every live pane carries a nonzero charge, so the next step's transfers
        // have charges to move.
        for (window, pane) in panes(&app) {
            assert!(
                app.__test_pane_charge_total(window, pane).unwrap() > 0,
                "{context}: pane {pane}"
            );
        }
    }
    // The PTY pane goes now if no step retired it, so every seed checks its custody.
    if let Some(pane) = pty_pane.filter(|_| !pty_retired) {
        let reaped_before = app.services.pty_reaper.path_counts();
        let reaper_work_before = reaper_work(&app);
        assert!(matches!(
            app.mcp_close_pane(sonicterm_cfg::config::McpControl::Any.into(), pane),
            crate::app::mcp::LoopReply::Closed
        ));
        assert_custody(&app, reaper_work_before, &format!("seed {seed} end"));
        let (reaped, fallback) = app.services.pty_reaper.path_counts();
        assert_eq!(
            reaped + fallback,
            reaped_before.0 + reaped_before.1 + 1,
            "seed {seed}: PTY custody"
        );
    }
    assert!(closed_waits > 0, "seed {seed}: no pending wait ended through its pane's retirement");
    // Waits still pending on live panes end once the server stops.
    shared.stop();
    for wait in waits {
        let reason = wait.waiter.join().unwrap();
        assert_eq!(reason, crate::app::mcp::tools_read::WaitReason::Revoked, "seed {seed}");
    }
    ran
}

/// Every seed runs every kind of step and keeps the invariants after each one.
#[test]
fn seeded_lifecycle_sequences_keep_every_invariant() {
    let extra = std::env::var("SONICTERM_LIFECYCLE_SEED").ok().and_then(|seed| seed.parse().ok());
    for seed in SEEDS.into_iter().chain(extra) {
        let ran = run(seed);
        for kind in [
            Step::Split,
            Step::Close,
            Step::Transfer,
            Step::TearOut,
            Step::Wait,
            Step::Output,
            Step::Exit,
        ] {
            assert!(ran.contains(&kind), "seed {seed} never ran {kind:?}");
        }
    }
}
