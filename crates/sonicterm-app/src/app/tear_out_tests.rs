//! Unit tests for tear-out identity and renderer adoption.
//!
//! A queued tear-out records where a tab sat. Positions move, so the request
//! also records which tab it means, and re-resolves the position from that id
//! when it is applied. These tests pin the re-resolution — the part that
//! decides whether the tab the user grabbed is the tab that moves.
//!
//! Renderer adoption is pinned here too. A pooled renderer is a real wgpu
//! renderer backed by an OS window and cannot be constructed in this unit-test
//! process, so that regression is checked at its production call site.

use super::*;

use sonicterm_cfg::{config::Config, keymap::Keymap, theme::Theme};

/// A renderer adopted from the warm pool must be brought to every live setting
/// that can change while it is hidden.
///
/// Runtime weight and theme actions update visible renderers only. The helper
/// proves the values independently of wgpu; the bounded source assertion proves
/// the production pooled-only branch applies them before scale rebuild. A real
/// pooled renderer requires a native event-loop window, device, and surface and
/// cannot be constructed in this unit-test process.
#[test]
fn pooled_renderer_adoption_applies_live_font_theme_and_tab_bar() {
    let mut config = Config::default();
    config.font.family = "SonicTerm Adoption Test".to_string();
    config.font.size = 19.0;
    config.font.line_height = 1.25;
    config.font.weight_scale = 2.75;
    let theme = Theme { name: "Adoption Theme".to_string(), ..Theme::default() };

    let warm = live_renderer_settings(&config, &theme, false, ChildRendererOrigin::WarmPool);
    assert_eq!(
        warm.font,
        Some(LiveFontSettings {
            family: "SonicTerm Adoption Test",
            size: 19.0,
            line_height: 1.25,
            weight_scale: 2.75,
        })
    );
    assert_eq!(warm.theme.map(|theme| theme.name.as_str()), Some("Adoption Theme"));
    assert_eq!(warm.background, theme.colors.background.0.as_str());
    assert!(!warm.tab_bar_visible);

    let fresh = live_renderer_settings(&config, &theme, false, ChildRendererOrigin::Fresh);
    assert_eq!(fresh.font, None, "fresh renderers must not rebuild an identical font atlas");
    assert!(fresh.theme.is_none(), "fresh renderers already received the constructor theme");
    assert_eq!(fresh.background, theme.colors.background.0.as_str());
    assert!(!fresh.tab_bar_visible);

    const SOURCE: &str = include_str!("tear_out.rs");
    let start = SOURCE
        .find("fn configure_child_renderer")
        .expect("the renderer-adoption function must exist");
    let body = &SOURCE[start..];
    let end = body.find("\n    pub(super) fn warm_window_pool_maintain").expect(
        "the test must stay bounded to configure_child_renderer rather than accepting a call elsewhere",
    );
    let body = &body[..end];
    let universal = body
        .find("renderer.set_tab_bar_visible(")
        .expect("every child renderer must receive live tab-bar visibility");
    let background = body
        .find("install_native_window_background(")
        .expect("every child renderer must receive the live native background");
    let plan = body
        .find("live_renderer_settings(&self.config, &self.theme, self.tab_bar_visible, origin)")
        .expect("renderer configuration must derive one typed live-settings plan");
    let exact_set_font =
        "renderer.set_font(font.family, font.size, font.line_height, font.weight_scale);";
    let set_font = body.find(exact_set_font).expect(
        "warm adoption must pass the planned family, size, line height, and weight in order",
    );
    let set_theme = body
        .find("renderer.set_theme(theme)")
        .expect("warm adoption must update the planned live theme");
    let resize = body
        .find("renderer.force_rebuild_for_scale(")
        .expect("adoption must still rebuild for the destination display scale");
    assert!(plan < universal && universal < set_font);
    assert!(plan < background && background < set_font);
    assert!(set_font < set_theme && set_theme < resize);

    let tear_out = &SOURCE
        [SOURCE.find("fn install_torn_out_window").expect("tear-out installer must exist")..];
    let warm_arm = &tear_out[tear_out.find("Some(warm) =>").expect("warm arm must exist")
        ..tear_out.find("None =>").expect("cold arm must exist")];
    assert!(warm_arm.contains("ChildRendererOrigin::WarmPool"));
    assert!(!warm_arm.contains("ChildRendererOrigin::Fresh"));
    let cold_arm = &tear_out[tear_out.find("None =>").expect("cold arm must exist")
        ..tear_out.find("let resize_start").expect("origin arms must end before resize")];
    assert!(cold_arm.contains("ChildRendererOrigin::Fresh"));
    assert!(!cold_arm.contains("ChildRendererOrigin::WarmPool"));
    assert!(tear_out.contains("configure_child_renderer(&mut renderer, &window, renderer_origin)"));

    let warm_create =
        &SOURCE[SOURCE.find("fn create_warm_window").expect("warm-window constructor must exist")
            ..SOURCE.find("fn take_warm_window").expect("warm-window take seam must exist")];
    assert!(warm_create.contains("ChildRendererOrigin::Fresh"));

    const MISC_SOURCE: &str = include_str!("misc.rs");
    let new_window_start =
        MISC_SOURCE.find("fn create_new_terminal_window").expect("Cmd+N constructor must exist");
    let new_window = &MISC_SOURCE[new_window_start..];
    let new_window = &new_window[..new_window
        .find("fn drain_menubar_actions")
        .expect("the Cmd+N assertion must stay bounded to its own constructor")];
    assert!(new_window.contains("ChildRendererOrigin::Fresh"));
    assert!(
        !new_window.contains("ChildRendererOrigin::WarmPool"),
        "Cmd+N builds its renderer from the current settings, so treating it as pooled would \
         rebuild the font atlas and theme it was just constructed with"
    );
    assert!(new_window.contains("configure_child_renderer("));
}

/// A pooled window must stay hidden until its renderer has been adopted and
/// the child window installed.
///
/// A warm window is created hidden and carries the font, theme, tab-bar, and
/// scale state it captured when it was pooled. Revealing it before adoption
/// puts one frame of that stale state on screen; revealing it before the
/// adoption guard puts a mis-sized window on screen for a tear-out that then
/// fails. Both are visible to the user, and neither is reachable from a unit
/// test, because a pooled renderer needs a native window, device, and surface.
/// The call order is asserted from source instead, bounded to the installer.
#[test]
fn a_pooled_child_is_revealed_only_after_adoption_and_install_succeed() {
    const SOURCE: &str = include_str!("tear_out.rs");
    let start = SOURCE.find("fn install_torn_out_window").expect("tear-out installer must exist");
    let body = &SOURCE[start..];
    let end = body
        .find("pub fn tear_out_apply_source_side")
        .expect("the test must stay bounded to the installer rather than the whole module");
    let install = &body[..end];

    let warm_arm = &install[install.find("Some(warm) =>").expect("warm arm must exist")
        ..install.find("None =>").expect("cold arm must exist")];
    assert!(
        !warm_arm.contains("set_visible"),
        "the pooled arm must not reveal its window: at that point the renderer still holds the \
         font, theme, tab-bar, and scale state it captured while pooled, so the first frame the \
         user sees is the stale one"
    );
    assert!(
        warm_arm.contains("set_outer_position"),
        "a pooled window must still be positioned under the cursor while it is hidden"
    );
    assert_eq!(
        install.matches("set_visible").count(),
        1,
        "a second reveal would race the guarded one and defeat it"
    );

    let configure = install
        .find("configure_child_renderer(&mut renderer, &window, renderer_origin)")
        .expect("the installer must adopt the renderer through the typed origin");
    let adoption_guard = install[configure..]
        .find("return None")
        .map(|offset| configure + offset)
        .expect("a failed adoption must abandon the tear-out");
    let installed = install
        .find("insert_window_registered(")
        .expect("the child window must be installed before it is shown");
    let reveal = install.find("set_visible(true)").expect("the child window must be revealed");

    assert!(
        configure < adoption_guard && adoption_guard < reveal,
        "the reveal must sit after the adoption guard, or a tear-out that fails its resize \
         flashes a mis-sized window before it is abandoned"
    );
    assert!(
        installed < reveal,
        "the reveal must follow installation, so the window that appears already has its panes \
         sized to their own sub-rects"
    );
}

/// Only a pooled child is owed a reveal.
///
/// Mapping an origin the wrong way is invisible to a source-order check: the
/// reveal still sits in exactly the right place, it is simply asked about the
/// wrong window. A pooled window mapped to `AlreadyVisible` is never shown at
/// all, leaving a torn-out tab running behind an invisible window; a fresh
/// window mapped to `AfterInstall` is shown a second time it never needed.
#[test]
fn only_a_pooled_child_is_owed_a_reveal() {
    assert_eq!(
        child_window_reveal(ChildRendererOrigin::WarmPool),
        ChildWindowReveal::AfterInstall,
        "a pooled window was created hidden, so the tear-out owes it a reveal"
    );
    assert_eq!(
        child_window_reveal(ChildRendererOrigin::Fresh),
        ChildWindowReveal::AlreadyVisible,
        "a fresh window is created visible, and its renderer was built from the current settings"
    );

    const SOURCE: &str = include_str!("tear_out.rs");
    let start = SOURCE.find("fn install_torn_out_window").expect("tear-out installer must exist");
    let body = &SOURCE[start..];
    let install = &body[..body
        .find("pub fn tear_out_apply_source_side")
        .expect("the test must stay bounded to the installer")];
    assert!(
        install.contains("child_window_reveal(renderer_origin)"),
        "the reveal must be decided from the origin the window was built from; an unconditional \
         show would ignore the mapping entirely and reveal every child twice"
    );

    let warm_create =
        &SOURCE[SOURCE.find("fn create_warm_window").expect("warm-window constructor must exist")
            ..SOURCE.find("fn take_warm_window").expect("warm-window take seam must exist")];
    assert!(
        warm_create.contains("with_visible(false)"),
        "the deferred reveal rests on a pooled window starting hidden; created visible it would \
         sit on screen empty from the moment it entered the pool"
    );
}

fn app_with_tabs(titles: &[&str]) -> App {
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    // Every seeded pane creates an inline-media charge; see the note in
    // `pane_exit_tests.rs` for why the counters are process-global.
    for title in titles {
        app.__test_seed_tab(title);
    }
    app
}

/// A tab closing at a lower index must not redirect a queued tear-out.
///
/// This is the whole defect: the recorded index stays *in range* when a tab
/// below it closes, so a bounds check passes while the slot now holds a
/// different tab. Asserting both halves — that the stale index misnames the
/// tab, and that the id still finds it — is what makes this a test of the fix
/// rather than of `position`.
#[test]
fn a_lower_tab_closing_moves_the_index_but_not_the_identity() {
    let _serialised = crate::app::media::MEDIA_COUNTER_LOCK.lock();
    let mut app = app_with_tabs(&["a", "b", "c"]);
    let window = app.__test_main_window_id().expect("synthetic main window");

    // The user grabs the middle tab.
    let grabbed = app.tab_id_at(window, 1).expect("tab at index 1");
    let last = app.tab_id_at(window, 2).expect("tab at index 2");
    assert_ne!(grabbed, last, "test setup: three distinct tabs");

    // Its neighbour below closes mid-gesture — a shell exiting, now that a
    // background thread can close a tab with no user action to serialise
    // against the drag.
    app.close_tab_at(0);

    assert_eq!(
        app.tab_index_of_id(window, grabbed),
        Some(0),
        "the grabbed tab moved down one slot and must still be findable there"
    );
    // The half that shows why the id is needed: the recorded index survives
    // the close and now names the wrong tab.
    assert_eq!(
        app.tab_id_at(window, 1),
        Some(last),
        "the recorded index 1 now holds the tab that was at 2 — a build trusting the index \
         would tear out this one instead of the one the user grabbed"
    );
}

/// A tab that closed entirely must fail the tear-out, not promote a neighbour.
#[test]
fn a_tab_that_closed_resolves_to_nothing() {
    let _serialised = crate::app::media::MEDIA_COUNTER_LOCK.lock();
    let mut app = app_with_tabs(&["a", "b"]);
    let window = app.__test_main_window_id().expect("synthetic main window");
    let doomed = app.tab_id_at(window, 0).expect("tab at index 0");

    app.close_tab_at(0);

    assert_eq!(
        app.tab_index_of_id(window, doomed),
        None,
        "a closed tab must resolve to nothing so the tear-out fails, rather than moving \
         whichever tab inherited its slot"
    );
    // And the surviving tab is still reachable by its own id, so the failure
    // above is specific rather than the lookup being broken outright.
    let survivor = app.tab_id_at(window, 0).expect("survivor at index 0");
    assert_eq!(app.tab_index_of_id(window, survivor), Some(0));
}

/// The drain must resolve through the id, not the recorded index.
///
/// This drives `resolve_tear_out_source_index` — the method the drain calls —
/// rather than the lookup helpers underneath it. That distinction is the point
/// of the test: an earlier version exercised only the helpers, and a build that
/// ignored the recorded id entirely and trusted the stale index passed it.
#[test]
fn a_queued_tear_out_follows_its_tab_when_a_lower_tab_closes() {
    let _serialised = crate::app::media::MEDIA_COUNTER_LOCK.lock();
    let mut app = app_with_tabs(&["a", "b", "c"]);
    let window = app.__test_main_window_id().expect("synthetic main window");
    let grabbed = app.tab_id_at(window, 1).expect("tab at index 1");

    // The request as the gesture recorded it.
    let req = crate::app::PendingTearOut {
        source_window: window,
        source_tab_idx: 1,
        source_tab_id: Some(grabbed),
        drop_screen_pos: None,
    };

    // A shell exits in the tab below and closes it mid-gesture.
    app.close_tab_at(0);

    assert_eq!(
        app.resolve_tear_out_source_index(&req),
        Some(0),
        "the drop must move the tab the user grabbed, which is now at index 0 — resolving to \
         the recorded index 1 would tear out the tab that inherited the slot"
    );
}

/// A tear-out whose tab closed must fail rather than move a neighbour.
#[test]
fn a_queued_tear_out_fails_when_its_own_tab_closed() {
    let _serialised = crate::app::media::MEDIA_COUNTER_LOCK.lock();
    let mut app = app_with_tabs(&["a", "b"]);
    let window = app.__test_main_window_id().expect("synthetic main window");
    let grabbed = app.tab_id_at(window, 0).expect("tab at index 0");
    let req = crate::app::PendingTearOut {
        source_window: window,
        source_tab_idx: 0,
        source_tab_id: Some(grabbed),
        drop_screen_pos: None,
    };

    app.close_tab_at(0);

    assert_eq!(
        app.resolve_tear_out_source_index(&req),
        None,
        "the grabbed tab is gone, so the tear-out must fail; index 0 still resolves to a live \
         tab and a build trusting it would tear out the wrong one"
    );
}

/// A request with no recorded id keeps the old index behaviour.
#[test]
fn a_tear_out_without_an_id_falls_back_to_its_index() {
    let _serialised = crate::app::media::MEDIA_COUNTER_LOCK.lock();
    let app = app_with_tabs(&["a", "b"]);
    let window = app.__test_main_window_id().expect("synthetic main window");
    let req = crate::app::PendingTearOut {
        source_window: window,
        source_tab_idx: 1,
        source_tab_id: None,
        drop_screen_pos: None,
    };

    assert_eq!(
        app.resolve_tear_out_source_index(&req),
        Some(1),
        "a request built without an id must still resolve, or the fallback path is dead"
    );
}
