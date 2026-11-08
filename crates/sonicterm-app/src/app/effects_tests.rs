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
    let mut app = App::new(Default::default(), Default::default(), Default::default());
    app.__test_seed_tab("main");
    let child = app.__test_seed_child_window(&["child"]);
    let main = app.registry.main_window_id.unwrap();
    let main_key = app.registry.window_keys.get(main).unwrap();
    let child_key = app.registry.window_keys.get(child).unwrap();
    let before = app.redraw_request_count.load(Ordering::SeqCst);
    for window in [child_key, main_key] {
        app.dispatch_effects(smallvec::smallvec![sonicterm_app_core::AppEffect::Render {
            window,
            reason: sonicterm_app_core::RedrawReason::Layout
        }]);
    }
    assert_eq!(app.redraw_request_count.load(Ordering::SeqCst), before + 2);
    app.registry.remove_window(child);
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
    let mut app = App::new(Default::default(), Default::default(), Default::default());
    app.__test_seed_tab("main");
    let child = app.__test_seed_child_window(&["child"]);
    let key = app.registry.window_keys.get(child).unwrap();
    app.dispatch_intent(sonicterm_app_core::AppIntent::WindowCloseRequested { window: key });
    assert!(!app.pending_exit);
    assert_eq!(app.registry.windows.len(), 2);
}

#[test]
fn missing_explicit_action_source_cannot_fall_back_to_main() {
    // A queued action for a closed child is not authority to mutate whichever window now has focus.
    let mut app = App::new(Default::default(), Default::default(), Default::default());
    app.__test_seed_tab("main");
    let main = app.registry.main_window_id.unwrap();
    let active = app.registry.windows[&main].tabs.active().unwrap().id;
    let missing = WindowId::from(7);
    assert!(!app.run_action_for_window(&Action::NewTab, missing));
    assert!(!app.run_action_for_window(&Action::CloseTab, missing));
    assert_eq!(app.registry.windows[&main].tabs.len(), 1);
    assert_eq!(app.registry.windows[&main].tabs.active().unwrap().id, active);
}

#[test]
fn explicit_window_input_uses_its_active_pane_not_a_zero_sentinel() {
    // Input effects select the addressed window's live active pane and never a peer or pane zero.
    let mut app = App::new(Default::default(), Default::default(), Default::default());
    let main_pane = app.__test_seed_tab("main");
    let child = app.__test_seed_child_window(&["child"]);
    let child_pane = app.registry.windows[&child].tab_states[0].active_pane;
    let key = app.window_key(child).unwrap();
    app.__test_enable_pty_write_log();
    app.dispatch_intent(sonicterm_app_core::AppIntent::ImeCommit {
        window: key,
        text: "text".into(),
    });
    assert_eq!(app.__test_pty_write_log(), [(child_pane, b"text".to_vec())]);
    assert_ne!(child_pane, main_pane);
    app.registry.remove_window(child);
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
    const SOURCE: &str = include_str!("effects.rs");
    assert!(SOURCE.contains("if let Err(error) = open_url_effect(&url)"));
    assert!(SOURCE.contains(
        "tracing::warn!(target: \"sonicterm_app::app\", %error, \"failed to open URL effect\")"
    ));
    assert!(!SOURCE.contains("tracing::warn!(target: \"sonicterm_app::app\", %error, %url"));
    assert!(!SOURCE.contains("url_open::validate(&url)"));
    assert!(!SOURCE.contains("let _ = sonicterm_cfg::url_open::open"));
}

#[test]
fn synthetic_window_ids_use_winit_safe_conversion() {
    const SOURCE: &str = include_str!("window_registry.rs");

    assert!(!SOURCE.contains("transmute::<u64, WindowId>"));
    assert!(SOURCE.contains("WindowId::from(u64::MAX - tag)"));
    assert!(SOURCE.contains("WindowId::from(u64::MAX)"));
}

#[test]
fn windows_native_background_parser_rejects_non_ascii_byte_slices() {
    const SOURCE: &str = include_str!("window_setup/windows.rs");

    assert!(SOURCE.contains("if hex_digits.len() != 6 || !hex_digits.is_ascii()"));
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

/// Main-window cursor motion is live behavior only: it never reaches the compatibility reducer,
/// whose cursor state nothing reads, while structural intents still do. The live hover path is
/// untouched, so the cursor position the App tracks still follows the pointer.
#[test]
fn cursor_motion_skips_the_compatibility_reducer() {
    let mut app = App::new(
        sonicterm_cfg::theme::Theme::default(),
        sonicterm_cfg::config::Config::default(),
        sonicterm_cfg::keymap::Keymap::default(),
    );
    app.__test_seed_tab("main");
    for step in 0..20 {
        let position = winit::dpi::PhysicalPosition::new(10.0 + f64::from(step), 15.0);
        app.handle_main_cursor_moved(position);
    }
    assert_eq!(app.machine.state().last_mouse_pos, None, "cursor motion reached the reducer");
    assert_eq!(app.main().unwrap().cursor_pos, (29.0, 15.0), "the live cursor still follows");
    app.observe_intent(sonicterm_app_core::AppIntent::MouseMove {
        window: sonicterm_types::WindowKey::new(0),
        pos: sonicterm_app_core::LogicalPos { pixel_x: 1.0, pixel_y: 2.0 },
    });
    assert!(app.machine.state().last_mouse_pos.is_some(), "the reducer still observes when told");
}

/// One example of every `AppIntent` variant, checked against the enum's own syntax tree, so a new
/// variant fails here until it has an example. Each `Drives` example is dispatched on a live App,
/// which reaches no fall-through (a debug assertion); each `Observes` one is routed to observation.
#[test]
fn every_intent_variant_is_classified_and_dispatched() {
    use sonicterm_app_core::{
        AppIntent, BroadcastScope, IntentAuthority, KeyCode, LogicalPos, MouseButton,
        PaletteChoice, PaneId, PendingDragOutcomeCore, SelectionMode, SplitDir, WindowRole,
    };
    use sonicterm_types::{ModKey, Pos, WindowKey};
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    let pane_id = app.__test_seed_tab("driven");
    let window = WindowKey(1);
    let pane = PaneId(pane_id);
    let pos = LogicalPos { pixel_x: 1.0, pixel_y: 2.0 };
    let examples = vec![
        AppIntent::NewWindow { role: WindowRole::Primary },
        AppIntent::WindowCloseRequested { window },
        AppIntent::WindowFocused { window },
        AppIntent::WindowBlurred { window },
        AppIntent::WindowResized { window, cols: 80, rows: 24 },
        AppIntent::WindowMoved { window, pos },
        AppIntent::NewTab { window, cwd: None },
        AppIntent::CloseTab { window, idx: 0 },
        AppIntent::NextTab { window },
        AppIntent::PrevTab { window },
        AppIntent::GoToTab { window, idx: 0 },
        AppIntent::TearOutTab { src_window: window, src_tab: 0 },
        AppIntent::SplitPane { window, dir: SplitDir::Right },
        AppIntent::ClosePane { window },
        AppIntent::ResizePane { window, dir: SplitDir::Left, cells: 1 },
        AppIntent::FocusPaneLeft { window },
        AppIntent::FocusPaneRight { window },
        AppIntent::FocusPaneUp { window },
        AppIntent::FocusPaneDown { window },
        AppIntent::PtyBurst { pane, generation: 1 },
        AppIntent::PtyWrite { pane, bytes: bytes::Bytes::from_static(b"x") },
        AppIntent::ForegroundProcChanged { pane, name: None },
        AppIntent::Key { window, code: KeyCode(0), mods: ModKey::empty(), pressed: true },
        AppIntent::Key { window, code: KeyCode(0), mods: ModKey::empty(), pressed: false },
        AppIntent::ImeStart { window },
        AppIntent::ImePreedit { window, text: String::new(), cursor: 0..0 },
        AppIntent::ImeCommit { window, text: String::new() },
        AppIntent::ImeEnd { window },
        AppIntent::MouseButton {
            window,
            pressed: true,
            button: MouseButton::Left,
            mods: ModKey::empty(),
            pos,
        },
        AppIntent::MouseMove { window, pos },
        AppIntent::MouseWheel { window, delta_y: 1.0, delta_x: 0.0, mods: ModKey::empty() },
        AppIntent::HoverUrl { window, url: None },
        AppIntent::ScrollUp { window, lines: 1 },
        AppIntent::ScrollDown { window, lines: 1 },
        AppIntent::ScrollPageUp { window },
        AppIntent::ScrollPageDown { window },
        AppIntent::ScrollToTop { window },
        AppIntent::ScrollToBottom { window },
        AppIntent::ScrollToCursor { window },
        AppIntent::SelectionStart {
            window,
            anchor: Pos { row: 0, col: 0 },
            mode: SelectionMode::Cell,
        },
        AppIntent::SelectionExtend { window, to: Pos { row: 0, col: 1 } },
        AppIntent::SelectionEnd { window },
        AppIntent::ClearSelection { window },
        AppIntent::CopySelection { window },
        AppIntent::Paste { window, text: String::new(), bracketed: false },
        AppIntent::OpenSearch { window },
        AppIntent::SearchQuery { window, query: String::new() },
        AppIntent::SearchStep { window, forward: true },
        AppIntent::CloseSearch { window },
        AppIntent::ToggleCommandPalette { window },
        AppIntent::PaletteFilter { window, filter: String::new() },
        AppIntent::PaletteStep { window, delta: 1 },
        AppIntent::PaletteSubmit { window, choice: PaletteChoice { id: "none".into() } },
        AppIntent::OsDragOutcome(PendingDragOutcomeCore { src_window: window, committed: false }),
        AppIntent::FilesDropped { window, paths: Vec::new() },
        AppIntent::ClickUrl { window, url: "not a url".into() },
        AppIntent::ConfigChanged { new: Box::default() },
        AppIntent::ApplyTheme { name: String::new() },
        AppIntent::FontSizeDelta { delta: 0 },
        AppIntent::SetBroadcastScope { scope: BroadcastScope::CurrentTab },
        AppIntent::RedrawRequested { window },
        AppIntent::Tick { now: std::time::Instant::now() },
        AppIntent::PtyExit { pane, status: 0 },
        AppIntent::Exit,
    ];
    // The variant names the enum declares, read from its source.
    let source = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../sonicterm-app-core/src/intent.rs"),
    )
    .expect("intent.rs");
    let file = syn::parse_file(&source).expect("intent.rs parses");
    let declared: std::collections::BTreeSet<String> = file
        .items
        .iter()
        .find_map(|item| match item {
            syn::Item::Enum(found) if found.ident == "AppIntent" => {
                Some(found.variants.iter().map(|variant| variant.ident.to_string()).collect())
            }
            _ => None,
        })
        .expect("enum AppIntent");
    let name = |intent: &AppIntent| {
        let debug = format!("{intent:?}");
        debug.split(|character: char| !character.is_alphanumeric()).next().unwrap_or("").to_owned()
    };
    let covered: std::collections::BTreeSet<String> = examples.iter().map(name).collect();
    assert_eq!(covered, declared, "an example for every AppIntent variant");
    let mut driven = 0;
    for intent in examples {
        match intent.authority() {
            IntentAuthority::Drives => {
                driven += 1;
                app.dispatch_intent(intent);
            }
            IntentAuthority::Observes => app.observe_intent(intent),
        }
    }
    assert!(driven > 20, "the driven set is exercised, ClickUrl included");
    assert!(app.pending_exit);
}
