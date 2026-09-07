use super::*;
use enum_map::enum_map;

fn test_governor() -> ResourceGovernor {
    ResourceGovernor::new(
        ProcessKind::Gui,
        GovernorLimits {
            process_bytes: usize::MAX,
            class_bytes: enum_map! { _ => usize::MAX },
            class_items: enum_map! { _ => None },
        },
    )
    .unwrap()
}

fn owner_limits() -> OwnerLimits {
    OwnerLimits {
        owner_bytes: usize::MAX,
        class_bytes: enum_map! { _ => usize::MAX },
        class_items: enum_map! { _ => None },
    }
}

#[test]
fn operational_redraw_effects_use_only_the_named_live_window() {
    // Stable targets cannot redirect to main/frontmost, and a closed key never gains a replacement target.
    let _serialised = crate::app::media::MEDIA_COUNTER_LOCK.lock();
    let mut app = App::new(Default::default(), Default::default(), Default::default());
    app.__test_seed_tab("main");
    let child = app.__test_seed_child_window(&["child"]);
    let main = app.main_window_id.unwrap();
    let main_key = app.window_keys.get(main).unwrap();
    let child_key = app.window_keys.get(child).unwrap();
    let before = app.redraw_request_count.load(Ordering::SeqCst);
    for window in [child_key, main_key] {
        app.dispatch_effects(smallvec::smallvec![sonicterm_app_core::AppEffect::Render {
            window,
            reason: sonicterm_app_core::RedrawReason::Layout
        }]);
    }
    assert_eq!(app.redraw_request_count.load(Ordering::SeqCst), before + 2);
    app.windows.remove(&child);
    app.dispatch_effects(smallvec::smallvec![sonicterm_app_core::AppEffect::Render {
        window: child_key,
        reason: sonicterm_app_core::RedrawReason::Layout
    }]);
    app.dispatch_effects(smallvec::smallvec![sonicterm_app_core::AppEffect::Render {
        window: sonicterm_types::WindowKey::new(0),
        reason: sonicterm_app_core::RedrawReason::Layout
    }]);
    assert_eq!(app.redraw_request_count.load(Ordering::SeqCst), before + 2);
}

#[test]
fn stale_reducer_topology_does_not_quit_live_windows() {
    // Legacy zero mirror counts are observations, not authority to terminate surviving live windows.
    let _serialised = crate::app::media::MEDIA_COUNTER_LOCK.lock();
    let mut app = App::new(Default::default(), Default::default(), Default::default());
    app.__test_seed_tab("main");
    let child = app.__test_seed_child_window(&["child"]);
    let key = app.window_keys.get(child).unwrap();
    app.dispatch_intent(sonicterm_app_core::AppIntent::WindowCloseRequested { window: key });
    assert!(!app.pending_exit);
    assert_eq!(app.windows.len(), 2);
}

#[test]
fn missing_explicit_action_source_cannot_fall_back_to_main() {
    // A queued action for a closed child is not authority to mutate whichever window now has focus.
    let _serialised = crate::app::media::MEDIA_COUNTER_LOCK.lock();
    let mut app = App::new(Default::default(), Default::default(), Default::default());
    app.__test_seed_tab("main");
    let main = app.main_window_id.unwrap();
    let active = app.windows[&main].tabs.active().unwrap().id;
    let missing = WindowId::from(7);
    assert!(!app.run_action_for_window(&Action::NewTab, missing));
    assert!(!app.run_action_for_window(&Action::CloseTab, missing));
    assert_eq!(app.windows[&main].tabs.len(), 1);
    assert_eq!(app.windows[&main].tabs.active().unwrap().id, active);
}

#[test]
fn explicit_window_input_uses_its_active_pane_not_a_zero_sentinel() {
    // Input effects select the addressed window's live active pane and never a peer or pane zero.
    let _serialised = crate::app::media::MEDIA_COUNTER_LOCK.lock();
    let mut app = App::new(Default::default(), Default::default(), Default::default());
    let main_pane = app.__test_seed_tab("main");
    let child = app.__test_seed_child_window(&["child"]);
    let child_pane = app.windows[&child].tab_states[0].active_pane;
    let key = app.window_key(child).unwrap();
    app.__test_enable_pty_write_log();
    app.dispatch_intent(sonicterm_app_core::AppIntent::ImeCommit {
        window: key,
        text: "text".into(),
    });
    assert_eq!(app.__test_pty_write_log(), [(child_pane, b"text".to_vec())]);
    assert_ne!(child_pane, main_pane);
    app.windows.remove(&child);
    app.dispatch_intent(sonicterm_app_core::AppIntent::ImeCommit {
        window: key,
        text: "discard".into(),
    });
    assert_eq!(app.__test_pty_write_log().len(), 1);
}

#[test]
fn open_url_effect_delegates_validation_to_the_real_opener() {
    let error = open_url_effect("invalid:anything").expect_err("unsupported scheme must fail");
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
    assert_eq!(error.to_string(), "scheme not allowed");
}

#[test]
fn open_url_effect_logs_real_errors_without_revalidating() {
    const SOURCE: &str = include_str!("mod.rs");
    assert!(SOURCE.contains("if let Err(error) = open_url_effect(&url)"));
    assert!(SOURCE.contains("tracing::warn!(%error, \"failed to open URL effect\")"));
    assert!(!SOURCE.contains("tracing::warn!(%error, %url"));
    assert!(!SOURCE.contains("url_open::validate(&url)"));
    assert!(!SOURCE.contains("let _ = sonicterm_cfg::url_open::open"));
}

#[test]
fn synthetic_window_ids_use_winit_safe_conversion() {
    const SOURCE: &str = include_str!("mod.rs");

    assert!(!SOURCE.contains("transmute::<u64, WindowId>"));
    assert!(SOURCE.contains("WindowId::from(u64::MAX - tag)"));
    assert!(SOURCE.contains("WindowId::from(u64::MAX)"));
}

#[test]
fn windows_native_background_parser_rejects_non_ascii_byte_slices() {
    const SOURCE: &str = include_str!("mod.rs");

    assert!(SOURCE.contains("if h.len() != 6 || !h.is_ascii()"));
    assert!(SOURCE.contains("not exactly six ASCII bytes"));
}

#[test]
fn owner_cleanup_returns_success_and_governor_refusals() {
    let governor = test_governor();
    let clean =
        governor.create_child(governor.root_owner(), OwnerKind::Window, owner_limits()).unwrap();
    close_owner(&governor, clean).unwrap();
    assert!(governor.snapshot(clean).is_err());

    let charged =
        governor.create_child(governor.root_owner(), OwnerKind::Window, owner_limits()).unwrap();
    let reservation = governor
        .try_reserve(
            charged,
            ResourceClass::GridVisible,
            sonicterm_types::ResourceAmount { bytes: 1, items: 1 },
        )
        .unwrap();

    let error = close_owner(&governor, charged).expect_err("live charges must remain observable");
    assert!(
        matches!(error, sonicterm_types::BudgetError::OwnerHasLiveCharges { owner, .. } if owner == charged)
    );
    drop(reservation);
}
