use super::*;
use crate::app::{next_pane_id, TabState};
use sonicterm_cfg::{
    config::Config,
    keymap::{Direction, Keymap},
    theme::Theme,
};
use sonicterm_grid::grid::Grid;
use sonicterm_ui::pane::PaneTree;
use std::{sync::Barrier, time::Duration};

/// Two tabs and two visible panes; the active pane is deliberately second in layout order.
fn fixture(child: bool, zoom: bool) -> (App, WindowId, u64, u64, u64) {
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    app.__test_seed_tab("main");
    let window = if child {
        app.__test_seed_child_window(&["visible", "inactive"])
    } else {
        app.__test_seed_tab("inactive");
        app.main_window_id.unwrap()
    };
    let right = next_pane_id();
    let parser = Arc::new(Mutex::new(Parser::new_with_staging_pool(
        Grid::new(20, 3),
        None,
        Arc::clone(&app.capture_staging_pool),
    )));
    let pane = PaneState::new_with_media_pool(parser, None, &app.inline_media_pool);
    let state = app.windows.get_mut(&window).unwrap();
    state.tabs.activate(0);
    let left = state.tab_states[0].active_pane;
    let inactive = state.tab_states[1].active_pane;
    state.panes.insert(right, pane);
    assert!(state.tab_states[0].tree.split(left, Direction::Right, right));
    state.tab_states[0].active_pane = right;
    if zoom {
        assert!(state.tab_states[0].tree.toggle_zoom(right));
    }
    for id in [left, right, inactive] {
        state.panes[&id].inline_images.lock().push(InlineImage {
            id,
            row: 0,
            col: 0,
            width: 1,
            height: 1,
            bgra: Arc::from([0, 0, 0, 255]),
        });
    }
    (app, window, left, right, inactive)
}

/// Exercise the same role adapter called by each production RedrawRequested branch.
fn sources(
    app: &mut App,
    window: WindowId,
    child: bool,
) -> Result<VisibleFrameSources, FrameUnavailable> {
    let outer = Rect::new(0.0, 0.0, 400.0, 120.0);
    if child {
        app.child_visible_frame_sources(window, outer)
    } else {
        app.main_visible_frame_sources(outer)
    }
}

/// The real guarded PaneRender slice is the callback boundary, not a copied-grid substitute.
fn assemble(
    app: &mut App,
    window: WindowId,
    sources: &VisibleFrameSources,
    held: &mut HeldVisibleFrame<'_>,
    callback: impl FnOnce(&mut [PaneRender<'_>]),
) {
    let state = app.windows.get_mut(&window).unwrap();
    let viewports = sources.reconcile_viewports(&mut state.panes, &held.guards).unwrap();
    let mut panes = pane_renders(
        &mut held.guards,
        &mut held.images,
        &viewports,
        sources.active_id(),
        &BTreeSet::new(),
        &HashMap::new(),
    );
    callback(&mut panes);
}

/// Parser and media barriers in inactive tabs and zoom-hidden siblings never block either role.
#[test]
fn hidden_parser_and_image_barriers_do_not_block_visible_assembly_in_either_role() {
    for child in [false, true] {
        for zoom in [false, true] {
            for image_lock in [false, true] {
                let (mut app, window, left, right, inactive) = fixture(child, zoom);
                let hidden = if zoom { left } else { inactive };
                let parser = Arc::clone(&app.windows[&window].panes[&hidden].parser);
                let images = Arc::clone(&app.windows[&window].panes[&hidden].inline_images);
                let ready = Arc::new(Barrier::new(2));
                let release = Arc::new(Barrier::new(2));
                let worker_ready = Arc::clone(&ready);
                let worker_release = Arc::clone(&release);
                let sources = sources(&mut app, window, child).ok().unwrap();
                let worker = std::thread::spawn(move || {
                    if image_lock {
                        let _held = images.lock();
                        worker_ready.wait();
                        worker_release.wait();
                    } else {
                        let _held = parser.lock();
                        worker_ready.wait();
                        worker_release.wait();
                    }
                });
                ready.wait();
                // Always release the worker even when the collector returns an error under mutation.
                let collected = sources.try_collect(|| {});
                release.wait();
                worker.join().unwrap();
                let mut held = collected.expect("hidden locks cannot defer the visible frame");
                let expected = if zoom { vec![right] } else { vec![left, right] };
                assert_eq!(*sources.image_visits.borrow(), expected);
                assert_eq!(
                    *sources.image_clones.borrow(),
                    expected.iter().map(|id| (*id, 1)).collect::<Vec<_>>()
                );
                let mut called = false;
                assemble(&mut app, window, &sources, &mut held, |panes| {
                    called = true;
                    assert_eq!(panes.iter().map(|pane| pane.id).collect::<Vec<_>>(), expected);
                    assert_eq!(
                        panes.iter().position(|pane| pane.is_active),
                        Some(sources.active_pos)
                    );
                    assert_eq!(panes[sources.active_pos].id, right);
                    for pane in panes {
                        assert_eq!(pane.inline_images.len(), 1);
                        assert!(app_free_try_lock_probe(&sources, pane.id).is_none());
                    }
                });
                assert!(called);
                assert!(
                    held.images.iter().all(Vec::is_empty),
                    "media snapshots were moved, not cloned twice"
                );
                assert_eq!(app.windows[&window].retry_not_before, None);
            }
        }
    }
}

fn app_free_try_lock_probe(
    sources: &VisibleFrameSources,
    id: u64,
) -> Option<MutexGuard<'_, Parser>> {
    sources.entries.iter().find(|entry| entry.id == id).unwrap().parser.try_lock()
}

/// Any visible parser/media miss aborts the whole frame and releases earlier locks and media clones.
#[test]
fn visible_parser_and_image_barriers_release_everything_before_retry_in_either_role() {
    for child in [false, true] {
        for image_lock in [false, true] {
            for blocked_position in [0, 1] {
                let (mut app, window, left, right, _) = fixture(child, false);
                let blocked = [left, right][blocked_position];
                let source_counts: Vec<_> = [left, right]
                    .into_iter()
                    .map(|id| {
                        let pane = &app.windows[&window].panes[&id];
                        (
                            id,
                            Arc::strong_count(&pane.parser),
                            Arc::strong_count(&pane.inline_images),
                        )
                    })
                    .collect();
                let parser = Arc::clone(&app.windows[&window].panes[&blocked].parser);
                let images = Arc::clone(&app.windows[&window].panes[&blocked].inline_images);
                let pixel =
                    Arc::clone(&app.windows[&window].panes[&left].inline_images.lock()[0].bgra);
                let pixel_baseline = Arc::strong_count(&pixel);
                let ready = Arc::new(Barrier::new(2));
                let release = Arc::new(Barrier::new(2));
                let worker_ready = Arc::clone(&ready);
                let worker_release = Arc::clone(&release);
                let sources = sources(&mut app, window, child).ok().unwrap();
                let worker = std::thread::spawn(move || {
                    if image_lock {
                        let _held = images.lock();
                        worker_ready.wait();
                        worker_release.wait();
                    } else {
                        let _held = parser.lock();
                        worker_ready.wait();
                        worker_release.wait();
                    }
                });
                ready.wait();
                let outcome = sources.try_collect(|| {});
                let wrong_success = outcome.is_ok();
                let why = outcome.err();
                release.wait();
                worker.join().unwrap();
                assert!(!wrong_success, "visible lock contention cannot assemble a partial frame");
                let why = why.unwrap();
                assert_eq!(
                    why,
                    FrameUnavailable::Contended { pane_id: blocked, images: image_lock }
                );
                if !image_lock {
                    assert!(
                        sources.image_visits.borrow().is_empty(),
                        "every parser is tried before any image"
                    );
                }
                assert_eq!(
                    Arc::strong_count(&pixel),
                    pixel_baseline,
                    "partial snapshots were released"
                );
                for entry in &sources.entries {
                    assert!(
                        entry.parser.try_lock().is_some(),
                        "all parser guards must be released"
                    );
                    assert!(
                        entry.images.try_lock().is_some(),
                        "image locks never escape the collector"
                    );
                }
                drop(sources);
                for (id, parsers, stores) in source_counts {
                    let pane = &app.windows[&window].panes[&id];
                    assert_eq!(Arc::strong_count(&pane.parser), parsers);
                    assert_eq!(Arc::strong_count(&pane.inline_images), stores);
                }
                let before = app.windows[&window].last_render;
                let now = Instant::now();
                app.visible_frame_unavailable(window, why, true, now);
                let floor = app.windows[&window].retry_not_before.unwrap();
                assert!(floor > now);
                assert_eq!(app.windows[&window].last_render, before);
                assert!(if child {
                    app.pending_redraw_windows.contains(&window)
                } else {
                    app.pending_redraw
                });
                app.visible_frame_unavailable(window, why, true, now + Duration::from_micros(1));
                assert_eq!(
                    app.windows[&window].retry_not_before,
                    Some(floor),
                    "early attempts cannot postpone the floor"
                );
            }
        }
    }
}

/// With no barrier, clone observations and Arc counts prove hidden media was not visited or copied.
#[test]
fn hidden_media_clone_counts_stay_zero_and_sources_outlive_guards() {
    for child in [false, true] {
        for zoom in [false, true] {
            let (mut app, window, left, right, inactive) = fixture(child, zoom);
            let hidden = if zoom { left } else { inactive };
            let hidden_parser = Arc::clone(&app.windows[&window].panes[&hidden].parser);
            let hidden_store = Arc::clone(&app.windows[&window].panes[&hidden].inline_images);
            let pixel = Arc::clone(&hidden_store.lock()[0].bgra);
            let before = (
                Arc::strong_count(&hidden_parser),
                Arc::strong_count(&hidden_store),
                Arc::strong_count(&pixel),
            );
            let sources = sources(&mut app, window, child).ok().unwrap();
            let mut held = sources.try_collect(|| {}).ok().unwrap();
            assert_eq!(
                before,
                (
                    Arc::strong_count(&hidden_parser),
                    Arc::strong_count(&hidden_store),
                    Arc::strong_count(&pixel)
                )
            );
            assert!(!sources.image_visits.borrow().contains(&hidden));
            assert!(!sources.image_clones.borrow().iter().any(|(id, _)| *id == hidden));
            assert_eq!(sources.active_pos, usize::from(!zoom));
            assemble(&mut app, window, &sources, &mut held, |panes| {
                assert_eq!(panes.iter().filter(|pane| pane.is_active).count(), 1);
                assert_eq!(panes[sources.active_pos].id, right);
                for pane in panes {
                    pane.grid.mark_all_dirty();
                }
            });
            drop(held);
            for entry in &sources.entries {
                assert!(entry.parser.try_lock().is_some());
            }
        }
    }
}

/// The generation-capture callback runs once before any visible lock is attempted, even on failure.
#[test]
fn scheduling_snapshot_slot_precedes_every_collection_lock() {
    for child in [false, true] {
        let (mut app, window, _, _, _) = fixture(child, false);
        let sources = sources(&mut app, window, child).ok().unwrap();
        let count = std::cell::Cell::new(0);
        let held = sources
            .try_collect(|| {
                count.set(count.get() + 1);
                for entry in &sources.entries {
                    assert!(entry.parser.try_lock().is_some());
                    assert!(entry.images.try_lock().is_some());
                }
            })
            .ok()
            .unwrap();
        assert_eq!(count.get(), 1);
        drop(held);
        let parser = sources.entries[0].parser.lock();
        let rejected = sources.try_collect(|| count.set(count.get() + 1));
        assert!(matches!(rejected, Err(FrameUnavailable::Contended { images: false, .. })));
        assert_eq!(count.get(), 2);
        drop(parser);
    }
}

/// Layout validation distinguishes each structural defect before any parser/media state is visited.
#[test]
fn layout_classifier_rejects_missing_duplicate_active_and_zoom_disagreement() {
    let rect = Rect::new(0.0, 0.0, 10.0, 10.0);
    let classify = |leaves: &[u64], ids: &[u64], active, zoom, missing| {
        validate_layout(leaves, ids.iter().map(|id| (*id, rect)).collect(), active, zoom, |id| {
            id != missing
        })
        .err()
    };
    assert_eq!(classify(&[1, 2], &[1, 2], 2, None, 1), Some(LayoutInvalid::MissingPane(1)));
    assert_eq!(classify(&[1, 1], &[1, 1], 1, None, 0), Some(LayoutInvalid::DuplicatePane(1)));
    assert_eq!(classify(&[1, 2], &[1, 2], 3, None, 0), Some(LayoutInvalid::ActiveNotLeaf(3)));
    assert_eq!(classify(&[1, 2], &[1], 2, Some(1), 0), Some(LayoutInvalid::ZoomDisagrees(1)));
    assert_eq!(classify(&[1, 2], &[1, 2], 2, Some(3), 0), Some(LayoutInvalid::ZoomDisagrees(3)));
    assert_eq!(classify(&[1, 2], &[2, 1], 2, None, 0), Some(LayoutInvalid::VisibleDisagrees));
    assert_eq!(
        validate_layout(&[1, 2], vec![(1, rect), (2, rect)], 2, None, |_| true).unwrap().active_pos,
        1
    );
}

/// Both real adapters reject complete invalid layouts without assembly, timestamps, or retry scheduling.
#[test]
fn structural_invalidity_is_not_contention_in_either_role() {
    for child in [false, true] {
        for defect in 0..5 {
            let (mut app, window, left, right, _) = fixture(child, false);
            let state = app.windows.get_mut(&window).unwrap();
            let before = state.last_render;
            let tab = &mut state.tab_states[0];
            let reason = match defect {
                0 => {
                    state.panes.remove(&left);
                    LayoutInvalid::MissingPane(left)
                }
                1 => {
                    tab.tree = PaneTree::leaf(right);
                    assert!(tab.tree.split(right, Direction::Right, right));
                    LayoutInvalid::DuplicatePane(right)
                }
                2 => {
                    tab.active_pane = u64::MAX;
                    LayoutInvalid::ActiveNotLeaf(u64::MAX)
                }
                3 => {
                    assert!(tab.tree.toggle_zoom(left));
                    LayoutInvalid::ZoomDisagrees(left)
                }
                _ => {
                    state.tab_states.clear();
                    LayoutInvalid::MissingTabState
                }
            };
            let parser = Arc::clone(&app.windows[&window].panes[&right].parser);
            let baseline = Arc::strong_count(&parser);
            for _ in 0..2 {
                let mut assembled = false;
                let why = match sources(&mut app, window, child) {
                    Err(why) => why,
                    Ok(sources) => {
                        if let Ok(mut held) = sources.try_collect(|| {}) {
                            assemble(&mut app, window, &sources, &mut held, |_| assembled = true);
                        }
                        panic!("structural topology was accepted; assembled={assembled}");
                    }
                };
                assert!(!assembled);
                assert_eq!(why, FrameUnavailable::StructuralInvalid(reason));
                assert_eq!(
                    Arc::strong_count(&parser),
                    baseline,
                    "invalid layouts clone no source handle"
                );
                app.visible_frame_unavailable(window, why, false, Instant::now());
                let state = &app.windows[&window];
                assert!(state.visible_frame_invalid);
                assert_eq!(state.retry_not_before, None);
                assert_eq!(state.last_render, before);
                assert!(!app.pending_redraw_windows.contains(&window));
                assert!(!app.pending_redraw);
            }
            let state = app.windows.get_mut(&window).unwrap();
            if state.tab_states.is_empty() {
                state.tab_states.push(TabState::new(PaneTree::leaf(right), right));
            } else {
                state.tab_states[0] = TabState::new(PaneTree::leaf(right), right);
            }
            app.mark_window_redraw(window, super::super::redraw::RedrawCause::Topology);
            let valid = sources(&mut app, window, child).ok().unwrap();
            assert!(
                app.windows[&window].visible_frame_invalid,
                "capturing valid topology does not yet complete the warning episode"
            );
            let held = valid.try_collect(|| {}).ok().unwrap();
            let state = app.windows.get_mut(&window).unwrap();
            valid.reconcile_viewports(&mut state.panes, &held.guards).unwrap();
            state.coherent_frame_collected();
            assert!(!state.visible_frame_invalid, "successful reconciliation resets the episode");
        }
    }
}

/// A closing tab bar has no layout and must neither warn nor arm a retry.
#[test]
fn closing_tab_without_layout_is_silent_in_both_roles() {
    for child in [false, true] {
        let (mut app, window, _, _, _) = fixture(child, false);
        app.windows.get_mut(&window).unwrap().tabs = Default::default();
        let why = sources(&mut app, window, child).err().unwrap();
        assert_eq!(why, FrameUnavailable::NoLayout);
        app.visible_frame_unavailable(window, why, false, Instant::now());
        assert!(!app.windows[&window].visible_frame_invalid);
        assert_eq!(app.windows[&window].retry_not_before, None);
    }
}

/// The captured viewport anchor rebases under the same held parser used by every PaneRender and active view.
#[test]
fn visible_frame_preserves_eviction_anchored_viewport_and_actual_active_index() {
    for child in [false, true] {
        let (mut app, window, _, right, _) = fixture(child, false);
        let parser = Arc::clone(&app.windows[&window].panes[&right].parser);
        {
            let mut parser = parser.lock();
            parser.resize(20, 3);
            parser.grid_mut().set_scrollback_limit(10);
            for n in 0..18 {
                parser.advance(format!("line {n:03}\r\n").as_bytes());
            }
        }
        app.windows
            .get_mut(&window)
            .unwrap()
            .panes
            .get_mut(&right)
            .unwrap()
            .pin_viewport_top(Some(4));
        let sources = sources(&mut app, window, child).ok().unwrap();
        for n in 18..21 {
            parser.lock().advance(format!("line {n:03}\r\n").as_bytes());
        }
        let mut held = sources.try_collect(|| {}).ok().unwrap();
        assert_eq!(sources.active_pos, 1);
        assemble(&mut app, window, &sources, &mut held, |panes| {
            assert_eq!(panes[1].id, right);
            assert!(panes[1].is_active);
            assert_eq!(panes[1].viewport_top_abs, Some(1));
        });
        assert_eq!(app.windows[&window].panes[&right].viewport_top_abs, Some(1));
    }
}

/// Switching to a nonzero tab index captures only that tab, not the main/child fixture's first one.
#[test]
fn active_tab_index_is_captured_in_both_role_adapters() {
    for child in [false, true] {
        let (mut app, window, _, _, inactive) = fixture(child, false);
        app.windows.get_mut(&window).unwrap().tabs.activate(1);
        let sources = sources(&mut app, window, child).ok().unwrap();
        assert_eq!(sources.tab_index, 1);
        assert_eq!(sources.active_pos, 0);
        assert_eq!(sources.active_id(), inactive);
        let mut held = sources.try_collect(|| {}).ok().unwrap();
        assemble(&mut app, window, &sources, &mut held, |panes| {
            assert_eq!(panes.len(), 1);
            assert_eq!(panes[0].id, inactive);
            assert!(panes[0].is_active);
        });
        assert_eq!(*sources.image_visits.borrow(), vec![inactive]);
    }
}

/// Both native-role handlers use the collector and its guarded PaneRender builder with no lifetime extension.
#[test]
fn production_roles_share_the_visible_collector_and_guarded_pane_builder() {
    let main = include_str!("window_event.rs");
    let child = include_str!("child_window.rs");
    assert!(main.contains("self.main_visible_frame_sources(outer)"));
    assert!(child.contains("self.child_visible_frame_sources(win_id, outer)"));
    for source in [main, child] {
        assert!(source.contains("sources.try_collect(|| self.snapshot_window_redraw(win_id))"));
        assert!(source.contains("sources.reconcile_viewports("));
        assert!(source.contains("super::visible_frame::pane_renders("));
        assert!(!source.contains("std::mem::transmute"));
        assert!(!source.contains("inline_images_by_pane"));
        assert!(!source.contains("active pane guard collected above"));
    }
}

/// Main's geometry-only scrollbar update, fade tick, and redraw request survive visible lock contention.
#[test]
fn main_scrollbar_service_precedes_collection_and_contention_exit() {
    let source: String = include_str!("window_event.rs")
        .lines()
        .filter(|line| !line.trim_start().starts_with("//"))
        .flat_map(|line| line.chars().filter(|ch| !ch.is_whitespace()))
        .collect();
    let layout = source.find("letpane_rects=sources.rects();").unwrap();
    let collect = source[layout..]
        .find("letcollected=sources.try_collect(||self.snapshot_window_redraw(win_id));")
        .unwrap()
        + layout;
    let before_lock = &source[layout..collect];
    let tick = before_lock.find("letscrollbar_now=Instant::now();").unwrap();
    let update = before_lock.find("scrollbar_visibility::update_and_collect(").unwrap();
    let fade = before_lock.find("scrollbar_visibility::is_animating(").unwrap();
    let redraw = before_lock
        .find("ifscrollbar_needs_more_frames{ifletSome(w)=self.main_window(){w.request_redraw();}}")
        .unwrap();
    assert!(tick < update && update < fade && fade < redraw);
    assert!(!before_lock.contains(".parser") && !before_lock.contains(".inline_images"));
    let retry = source[collect..].find("self.visible_frame_unavailable(").unwrap() + collect;
    assert!(layout + redraw < collect && collect < retry);
}

/// Valid capture followed by malformed held guards stays one warning episode until reconciliation succeeds.
#[test]
fn postlock_structural_invalidity_stays_latched_until_valid_reconciliation() {
    for child in [false, true] {
        let (mut app, window, left, right, _) = fixture(child, false);
        let last_render = app.windows[&window].last_render;
        for attempt in 0..3 {
            let sources = sources(&mut app, window, child).ok().unwrap();
            assert_eq!(
                app.windows[&window].visible_frame_invalid,
                attempt > 0,
                "valid capture cannot rearm a warning before held-frame validation",
            );
            let mut held = sources.try_collect(|| {}).ok().unwrap();
            // Both parsers are held, but their swapped order must reject the entire frame before viewport writes.
            held.guards.swap(0, 1);
            let state = app.windows.get_mut(&window).unwrap();
            let before =
                [state.panes[&left].viewport_top_abs, state.panes[&right].viewport_top_abs];
            let why = sources.reconcile_viewports(&mut state.panes, &held.guards).err().unwrap();
            assert_eq!(why, FrameUnavailable::StructuralInvalid(LayoutInvalid::VisibleDisagrees));
            assert_eq!(
                before,
                [state.panes[&left].viewport_top_abs, state.panes[&right].viewport_top_abs]
            );
            drop(held);
            drop(sources);
            app.visible_frame_unavailable(window, why, false, Instant::now());
            let state = &app.windows[&window];
            assert!(state.visible_frame_invalid);
            assert_eq!(state.retry_not_before, None);
            assert_eq!(state.last_render, last_render);
            assert!(!app.pending_redraw && !app.pending_redraw_windows.contains(&window));
        }
        let valid = sources(&mut app, window, child).ok().unwrap();
        let held = valid.try_collect(|| {}).ok().unwrap();
        let state = app.windows.get_mut(&window).unwrap();
        assert!(state.visible_frame_invalid);
        valid.reconcile_viewports(&mut state.panes, &held.guards).unwrap();
        // The real role handlers call this completion method only after their successful reconciliation branch.
        state.coherent_frame_collected();
        assert!(!state.visible_frame_invalid);
        assert_eq!(state.last_render, last_render);
    }
}

/// Both roles reset the warning through successful held-frame completion, never from capture alone.
#[test]
fn warning_reset_is_after_reconciliation_in_both_production_roles() {
    let compact = |text: &str| -> String {
        text.lines()
            .filter(|line| !line.trim_start().starts_with("//"))
            .flat_map(|line| line.chars().filter(|ch| !ch.is_whitespace()))
            .collect()
    };
    let collector = compact(include_str!("visible_frame.rs"));
    assert!(!collector.contains("visible_frame_invalid=false"));
    assert!(collector
        .contains("if!std::mem::replace(&mutwindow.visible_frame_invalid,true){tracing::warn!"));
    let owner = compact(include_str!("mod.rs"));
    assert!(owner.contains("fncoherent_frame_collected(&mutself){self.retry_not_before=None;self.visible_frame_invalid=false;}"));
    for source in [include_str!("window_event.rs"), include_str!("child_window.rs")] {
        let source = compact(source);
        let reconcile = source.find("sources.reconcile_viewports(").unwrap();
        let complete =
            source[reconcile..].find(".coherent_frame_collected();").unwrap() + reconcile;
        let error = source[reconcile..complete].find("self.visible_frame_unavailable(").unwrap()
            + reconcile;
        assert!(
            source[error..complete].contains("return;"),
            "invalid reconciliation cannot reach completion"
        );
        assert_eq!(source.matches(".coherent_frame_collected();").count(), 1);
        assert!(!source[..reconcile].contains(".coherent_frame_collected();"));
    }
    let collector = include_str!("visible_frame.rs");
    assert!(collector.contains("Closing or reaped window/tab: silent skip."));
    assert!(!collector.contains("or no renderer geometry yet"));
}
