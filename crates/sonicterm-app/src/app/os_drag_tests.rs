use super::*;
use crate::app::App;
use sonicterm_cfg::{config::Config, keymap::Keymap, theme::Theme};

struct DropOwner(bool);

impl OsTabDragBackend for DropOwner {
    fn begin_session(
        &mut self,
        _handle: AppHandle,
        _source_window: WindowId,
        _source_tab_idx: usize,
        _payload_json: String,
        _drag_image_png: Vec<u8>,
    ) {
    }

    fn owns_native_drop_target(&self) -> bool {
        self.0
    }
}

#[test]
fn only_an_explicit_drop_owner_disables_the_winit_target() {
    // Publication-only backends and missing OLE initialization must retain default native file drops.
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    assert!(!app.owns_native_drop_target());
    app.set_os_drag_backend(Box::new(DropOwner(false)));
    assert!(!app.owns_native_drop_target());
    app.set_os_drag_backend(Box::new(DropOwner(true)));
    assert!(app.owns_native_drop_target());
}

#[test]
fn overlapping_windows_resolve_only_the_registered_drop_target() {
    // Native destination identity outranks snapshot insertion order when two tab bars overlap.
    let registry = TabBarRegistry::new();
    let child = WindowId::from(31);
    for (window, midpoint) in [(None, 60), (Some(child), 20)] {
        registry.publish(TabBarSnapshot {
            window,
            window_rect: (0, 0, 100, 100),
            bar_rect: (0, 0, 100, 20),
            tab_midpoints: vec![midpoint],
            tab_indices: vec![0],
            total_tabs: 1,
            overflow_append_from: None,
        });
    }
    assert_eq!(registry.resolve_window_screen_pos(None, 40, 10), Some(0));
    assert_eq!(registry.resolve_window_screen_pos(Some(child), 40, 10), Some(1));
    assert_eq!(registry.resolve_window_screen_pos(Some(child), 40, 40), None);
    registry.remove(Some(child));
    assert_eq!(registry.resolve_window_screen_pos(Some(child), 40, 10), None);
    assert_eq!(registry.resolve_window_screen_pos(None, 40, 10), Some(0));
}

#[test]
fn every_native_creation_selects_drop_ownership_before_create_window() {
    // Main, warm, tear-out, and NewWindow must choose one owner before winit registers its default target.
    for (source, creations) in [
        (include_str!("event_loop.rs"), 1),
        (include_str!("tear_out.rs"), 2),
        (include_str!("misc.rs"), 1),
    ] {
        let selected: Vec<_> = source.match_indices("self.native_drop_attributes(attrs)").collect();
        let created: Vec<_> = source
            .match_indices("el.create_window(attrs)")
            .filter(|(offset, _)| {
                source[..*offset]
                    .rsplit('\n')
                    .next()
                    .is_some_and(|line| line.trim_start().starts_with("let window ="))
            })
            .collect();
        assert_eq!(selected.len(), creations);
        assert_eq!(created.len(), creations);
        for ((selected, _), (created, _)) in selected.into_iter().zip(created) {
            assert!(selected < created);
            assert!(!source[selected..created].contains("set_visible(true)"));
        }
    }
}

#[test]
fn drop_registration_precedes_irreversible_destination_effects() {
    // Registration refusal cannot spawn a new shell or strand a detached tab in an unusable destination.
    let startup = include_str!("event_loop.rs");
    let startup = &startup[startup.find("pub(super) fn do_resumed(").unwrap()..];
    assert!(
        startup.find("register_window_with_os_drag_backend").unwrap()
            < startup.find("self.seed_initial_tabs()").unwrap()
    );
    let new_window = include_str!("misc.rs");
    let new_window =
        &new_window[new_window.find("pub(super) fn create_new_terminal_window(").unwrap()..];
    assert!(
        new_window.find("register_window_with_os_drag_backend").unwrap()
            < new_window.find("spawn_pane_state_for_child").unwrap()
    );
    let tear_out = include_str!("tear_out.rs");
    let tear_out = &tear_out[tear_out.find("fn commit_torn_out_window(").unwrap()..];
    let register = tear_out.find("register_window_with_os_drag_backend").unwrap();
    let transfer = tear_out.find("transfer_pane_owners").unwrap();
    assert!(register < transfer);
    assert!(tear_out[register..transfer].contains("rollback_detached_tab(transaction)"));
    assert!(tear_out[transfer..].contains("release_child_window_registries(win_id)"));
}
