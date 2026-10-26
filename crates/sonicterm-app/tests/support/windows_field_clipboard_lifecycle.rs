//! Cases for the existing integration target; native event-loop ownership stays in its root.

use super::*;

/// Main search over a pane with all-motion mouse reporting, with a positive control.
pub(super) fn cancel_setup(session: &mut Session, active: &ActiveEventLoop) -> Result<(), String> {
    session.key_child = false;
    ensure!(session.app.__test_set_main_search_query("needle"), "main search opens");
    let index = session.app.main_tabs().ok_or("main tabs")?.active_index();
    let pane = session.app.__test_active_pane_in_tab(index).ok_or("main pane")?;
    ensure!(
        session.app.__test_advance_pane_parser(pane, b"\x1b[?1003h\x1b[?1006h"),
        "mouse tracking"
    );
    render(&mut session.app, active, session.main);
    session.app.__test_enable_pty_write_log();
    pointer_at(&mut session.app, active, session.main, TERMINAL_POINT);
    ensure!(
        !session.app.__test_pty_write_log().is_empty(),
        "positive control: unowned motion reports to the mouse-aware pane"
    );
    Ok(())
}

pub(super) fn start_search_drag(
    session: &mut Session,
    active: &ActiveEventLoop,
) -> Result<(), String> {
    let main = session.main;
    render(&mut session.app, active, main);
    let caret = session.caret()?;
    pointer_at(&mut session.app, active, main, (caret.x - 1.0, caret_center(caret)));
    // Hover before the press is ordinary unowned motion; only the press onward is the field's.
    session.app.__test_enable_pty_write_log();
    button(&mut session.app, active, main, ElementState::Pressed);
    ensure!(session.app.__test_field_pointer_capture() == Some((main, true)), "search drag starts");
    let log = session.app.__test_pty_write_log();
    ensure!(log.is_empty(), "the field press is not reported: {log:?}");
    Ok(())
}

pub(super) fn assert_swallowed(
    session: &mut Session,
    active: &ActiveEventLoop,
    cause: &str,
) -> Result<(), String> {
    let main = session.main;
    ensure!(
        session.app.__test_field_pointer_capture() == Some((main, false)),
        "{cause} cancels the drag"
    );
    pointer_at(&mut session.app, active, main, TERMINAL_POINT);
    ApplicationHandler::window_event(
        &mut session.app,
        active,
        main,
        WindowEvent::CursorLeft { device_id: DeviceId::dummy() },
    );
    ensure!(session.state()?.1.is_none(), "after {cause}, motion never selects");
    ensure!(
        session.app.__test_pty_write_log().is_empty(),
        "after {cause}, motion and leave are swallowed"
    );
    button(&mut session.app, active, main, ElementState::Released);
    ensure!(
        session.app.__test_field_pointer_capture().is_none(),
        "the paired release ends the swallow"
    );
    ensure!(
        session.app.__test_pty_write_log().is_empty(),
        "after {cause}, the release is swallowed"
    );
    pointer_at(&mut session.app, active, main, (TERMINAL_POINT.0 + 8.0, TERMINAL_POINT.1));
    ensure!(!session.app.__test_pty_write_log().is_empty(), "after {cause}, reporting resumes");
    Ok(())
}

pub(super) fn cancel_by_resize(
    session: &mut Session,
    active: &ActiveEventLoop,
) -> Result<(), String> {
    start_search_drag(session, active)?;
    let size = PhysicalSize::new(WINDOW_SIZE.0, WINDOW_SIZE.1);
    ApplicationHandler::window_event(
        &mut session.app,
        active,
        session.main,
        WindowEvent::Resized(size),
    );
    assert_swallowed(session, active, "resize")
}

pub(super) fn cancel_by_ime(session: &mut Session, active: &ActiveEventLoop) -> Result<(), String> {
    start_search_drag(session, active)?;
    ApplicationHandler::window_event(
        &mut session.app,
        active,
        session.main,
        WindowEvent::Ime(Ime::Enabled),
    );
    assert_swallowed(session, active, "synthetic IME event")
}

pub(super) fn cancel_by_key(session: &mut Session, active: &ActiveEventLoop) -> Result<(), String> {
    assert_swallowed(session, active, "native End key")
}

/// A press in the child does not clear or steal main's field drag.
pub(super) fn other_window_press(
    session: &mut Session,
    active: &ActiveEventLoop,
) -> Result<(), String> {
    let (main, child) = (session.main, session.child);
    start_search_drag(session, active)?;
    pointer_at(&mut session.app, active, child, TERMINAL_POINT);
    button(&mut session.app, active, child, ElementState::Pressed);
    ensure!(
        session.app.__test_field_pointer_capture() == Some((main, true)),
        "child press keeps main's drag"
    );
    let caret = session.caret()?;
    pointer_at(&mut session.app, active, main, (2.0, caret_center(caret)));
    ensure!(session.state()?.1.is_some(), "main motion still extends its selection");
    button(&mut session.app, active, main, ElementState::Released);
    ensure!(session.app.__test_field_pointer_capture().is_none(), "main keeps its paired release");
    button(&mut session.app, active, child, ElementState::Released);
    ensure!(session.app.__test_pty_write_log().is_empty(), "no pointer half reached main's PTY");
    Ok(())
}

/// A cancelled main drag whose release never arrives does not block the child's field;
/// main's late release is still consumed once and then reporting resumes.
pub(super) fn lost_release_recovery(
    session: &mut Session,
    active: &ActiveEventLoop,
) -> Result<(), String> {
    let (main, child) = (session.main, session.child);
    session.key_child = false;
    start_search_drag(session, active)?;
    ApplicationHandler::window_event(&mut session.app, active, main, WindowEvent::Focused(false));
    ensure!(session.app.__test_field_owes_release(main), "focus loss leaves main a release debt");
    ensure!(session.app.__test_set_child_search_query(child, "needle"), "child search");
    session.key_child = true;
    render(&mut session.app, active, child);
    let caret = session.caret()?;
    pointer_at(&mut session.app, active, child, (caret.x - 1.0, caret_center(caret)));
    button(&mut session.app, active, child, ElementState::Pressed);
    ensure!(
        session.app.__test_field_pointer_capture() == Some((child, true)),
        "the child field gesture starts despite main's debt"
    );
    pointer_at(&mut session.app, active, child, (2.0, caret_center(caret)));
    ensure!(session.state()?.1.is_some(), "the child drag selects");
    button(&mut session.app, active, child, ElementState::Released);
    ensure!(session.app.__test_field_pointer_capture().is_none(), "the child release ends it");
    ensure!(session.app.__test_field_owes_release(main), "main's debt survives the child gesture");
    session.key_child = false;
    pointer_at(&mut session.app, active, main, TERMINAL_POINT);
    ensure!(session.app.__test_pty_write_log().is_empty(), "main motion while owed is swallowed");
    button(&mut session.app, active, main, ElementState::Released);
    ensure!(!session.app.__test_field_owes_release(main), "the late release pays the debt");
    ensure!(session.app.__test_pty_write_log().is_empty(), "the late release is consumed");
    pointer_at(&mut session.app, active, main, (TERMINAL_POINT.0 + 8.0, TERMINAL_POINT.1));
    ensure!(!session.app.__test_pty_write_log().is_empty(), "main reporting resumes");
    // The next native Escape closes the child search.
    session.key_child = true;
    Ok(())
}

pub(super) fn child_search_closed(
    session: &mut Session,
    _: &ActiveEventLoop,
) -> Result<(), String> {
    ensure!(session.app.__test_field_state(session.child).is_none(), "native Escape closes search");
    session.key_child = false;
    Ok(())
}

/// Closing the captured rename tab mid-drag leaves the sibling tab unchanged.
pub(super) fn rename_tab_closed(
    session: &mut Session,
    active: &ActiveEventLoop,
) -> Result<(), String> {
    let main = session.main;
    session.app.__test_seed_tab("peer");
    session.app.__test_set_frontmost_window(None);
    render(&mut session.app, active, main);
    ensure!(session.app.run_action(&Action::RenameTab), "rename peer");
    ensure!(!session.app.__test_palette_query().is_empty(), "rename edits the active peer tab");
    render(&mut session.app, active, main);
    let caret = session.caret()?;
    let row_y = caret_center(caret);
    pointer_at(&mut session.app, active, main, (caret.x - 1.0, row_y));
    button(&mut session.app, active, main, ElementState::Pressed);
    ensure!(session.app.__test_field_pointer_capture() == Some((main, true)), "rename drag starts");
    let sibling = session.app.main_tabs().ok_or("tabs")?.tabs()[0].clone();
    ensure!(session.app.run_action(&Action::CloseTab), "close the captured tab");
    ensure!(session.app.main_tabs().ok_or("tabs")?.len() == 1, "the sibling tab remains");
    pointer_at(&mut session.app, active, main, (2.0, row_y));
    ensure!(
        session.app.__test_field_pointer_capture() == Some((main, false)),
        "the orphan drag is swallowed"
    );
    button(&mut session.app, active, main, ElementState::Released);
    ensure!(session.app.__test_field_pointer_capture().is_none(), "release consumed");
    if session.app.__test_palette_open() {
        // An orphaned editor submits nothing.
        session.app.__test_command_palette_handle_key(&Key::Named(NamedKey::Enter));
    }
    let remaining = session.app.main_tabs().ok_or("tabs")?.tabs()[0].clone();
    ensure!(
        (remaining.id, &remaining.custom_title, &remaining.title)
            == (sibling.id, &sibling.custom_title, &sibling.title),
        "the sibling was not renamed: {:?} -> {:?}",
        sibling.custom_title,
        remaining.custom_title
    );
    ensure!(session.app.__test_pty_write_log().is_empty(), "no rename gesture reached the PTY");
    Ok(())
}

/// Renderer getters answer only for the presented frame across every invalidating transition.
pub(super) fn renderer_lifecycle(
    session: &mut Session,
    active: &ActiveEventLoop,
) -> Result<(), String> {
    let main = session.main;
    session.key_child = false;
    ensure!(session.app.run_action(&Action::OpenCommandPalette), "open palette");
    session.app.__test_set_palette_query("lifecycle");
    render(&mut session.app, active, main);
    let caret = session.caret()?;
    let point = (caret.x - 1.0, caret_center(caret));
    let hit = session.app.__test_field_hit(main, point, FieldHitMode::Press);
    ensure!(matches!(hit, Some(FieldHit::Offset(_))), "presented hit: {hit:?}");
    session.app.__test_set_palette_query("lifecycle2");
    ensure!(
        session.app.__test_field_caret_rect(main).is_none(),
        "edited query: caret waits for a frame"
    );
    let stale = session.app.__test_field_hit(main, point, FieldHitMode::Press);
    ensure!(stale == Some(FieldHit::Stale), "edited query: hit is stale: {stale:?}");
    check_ime_follows_presented_caret(session, active)?;

    let (family, size, line_height, weight) =
        (session.font_family.clone(), session.font_size, session.line_height, session.weight_scale);
    let transitions: [(&str, Transition); 4] = [
        (
            "set_font",
            Box::new(move |renderer| renderer.set_font(&family, size + 2.0, line_height, weight)),
        ),
        (
            "try_resize",
            Box::new(|renderer| {
                assert!(
                    renderer.try_resize(WINDOW_SIZE.0 - 40, WINDOW_SIZE.1 - 40),
                    "resize applies"
                );
            }),
        ),
        ("set_scale_factor", Box::new(|renderer| renderer.set_scale_factor(1.25))),
        (
            "set_panel_padding",
            Box::new(|renderer| {
                assert!(renderer.set_panel_padding(12.0), "padding changes");
            }),
        ),
    ];
    for (name, transition) in &transitions {
        let renderer = session.app.__test_window_renderer_mut(main).ok_or("main renderer")?;
        transition(renderer);
        ensure!(
            session.app.__test_field_caret_rect(main).is_none(),
            "{name}: caret invalid until presented"
        );
        let anchor = session.app.__test_field_ime_anchor(main);
        ensure!(anchor == ("pending", None), "{name}: IME waits, never the terminal: {anchor:?}");
        render(&mut session.app, active, main);
        ensure!(
            session.app.__test_field_caret_rect(main).is_some(),
            "{name}: the next presented frame restores it"
        );
    }

    // A frame that does not present never commits its proposed geometry.
    let renderer = session.app.__test_window_renderer_mut(main).ok_or("main renderer")?;
    let presented = renderer.successful_frame_count();
    renderer.__inject_gpu_fault(GpuFaultKind::DestroyDevice);
    let after_fault = session.app.__test_field_caret_rect(main);
    session.app.__test_set_palette_query("lifecycle3");
    render(&mut session.app, active, main);
    let count = session
        .app
        .__test_window_renderer_mut(main)
        .ok_or("main renderer")?
        .successful_frame_count();
    ensure!(count == presented, "a stopped device presents no frame: {presented} -> {count}");
    let unpresented = session.app.__test_field_caret_rect(main);
    ensure!(unpresented.is_none(), "an unpresented frame committed geometry: {unpresented:?}");
    let anchor = session.app.__test_field_ime_anchor(main);
    ensure!(anchor == ("pending", None), "unpresented frame: IME waits: {anchor:?}");
    println!("lifecycle: caret right after DestroyDevice (before any edit) = {after_fault:?}");
    ensure!(
        after_fault.is_none(),
        "a stopped device must invalidate the presented caret: {after_fault:?}"
    );
    Ok(())
}
