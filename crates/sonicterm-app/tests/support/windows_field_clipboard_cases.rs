//! Cases for the existing integration target; native event-loop ownership stays in its root.

use super::*;

pub(super) fn setup_commands(
    session: &mut Session,
    active: &ActiveEventLoop,
) -> Result<(), String> {
    session.key_child = false;
    // The seeded logical main id differs from its HWND id, so `None` (main fallback) routes like production.
    session.app.__test_set_frontmost_window(None);
    session.app.__test_enable_pty_write_log();
    ensure!(session.app.run_action(&Action::OpenCommandPalette), "open palette");
    session.app.__test_set_memory_clipboard(PASTE);
    render(&mut session.app, active, session.main);
    ensure!(session.state()?.0.is_empty(), "commands query starts empty");
    Ok(())
}

pub(super) fn setup_rename_tab(
    session: &mut Session,
    active: &ActiveEventLoop,
) -> Result<(), String> {
    session.key_child = false;
    ensure!(session.app.run_action(&Action::RenameTab), "rename tab");
    session.app.__test_set_memory_clipboard(PASTE);
    render(&mut session.app, active, session.main);
    ensure!(!session.state()?.0.is_empty(), "rename tab seeds the active title body");
    Ok(())
}

pub(super) fn setup_rename_window(
    session: &mut Session,
    active: &ActiveEventLoop,
) -> Result<(), String> {
    session.key_child = false;
    ensure!(session.app.run_action(&Action::RenameWindow), "rename window");
    session.app.__test_set_memory_clipboard(PASTE);
    render(&mut session.app, active, session.main);
    ensure!(session.state()?.0.is_empty(), "rename window starts empty");
    Ok(())
}

pub(super) fn setup_child_search(
    session: &mut Session,
    active: &ActiveEventLoop,
) -> Result<(), String> {
    session.key_child = true;
    session.app.__test_enable_pty_write_log();
    ensure!(session.app.run_action_for_window(&Action::EnterCopyMode, session.child), "copy mode");
    ensure!(session.app.__test_child_read_only(session.child) == Some(true), "child is READONLY");
    session.app.run_action_for_window(&Action::OpenSearch, session.child);
    ensure!(
        session.app.__test_field_state(session.child).is_some(),
        "OpenSearch must open search in the READONLY child"
    );
    session.app.__test_set_memory_clipboard(PASTE);
    render(&mut session.app, active, session.child);
    ensure!(session.state()?.0.is_empty(), "child search starts empty");
    Ok(())
}

pub(super) fn check_pasted(session: &mut Session, active: &ActiveEventLoop) -> Result<(), String> {
    let state = session.state()?;
    ensure!(
        state == (PASTE.to_string(), None, PASTE.len()),
        "paste replaced the selection: {state:?}"
    );
    check_ime_follows_presented_caret(session, active)
}

pub(super) fn check_extended(session: &mut Session, _: &ActiveEventLoop) -> Result<(), String> {
    let state = session.state()?;
    ensure!(state.1 == Some(5..PASTE.len()), "Shift+Left selects back over the ▏: {state:?}");
    Ok(())
}

pub(super) fn check_copied(session: &mut Session, _: &ActiveEventLoop) -> Result<(), String> {
    let copied = session.app.__test_memory_clipboard();
    ensure!(copied.as_deref() == Some("▏beta"), "copy writes only the selection: {copied:?}");
    Ok(())
}

pub(super) fn check_typed(session: &mut Session, active: &ActiveEventLoop) -> Result<(), String> {
    let state = session.state()?;
    ensure!(state == ("alphax".to_string(), None, 6), "typing replaced the selection: {state:?}");
    check_ime_follows_presented_caret(session, active)?;
    session.app.__test_set_memory_clipboard(&long_query());
    Ok(())
}

/// Long-query clipping, reverse/forward pointer drags, Shift-extend, and highlight pixels.
pub(super) fn check_pointer(session: &mut Session, active: &ActiveEventLoop) -> Result<(), String> {
    // Reuse one screen position: hosted desktops need not fit two windows side by side.
    // Raise only this test HWND without stealing keyboard focus before reading visible GDI pixels.
    let target = hwnd(session.field_window())?;
    // SAFETY: target is owned by the live test session; flags preserve its bounds and activation.
    unsafe {
        SetWindowPos(
            target,
            Some(HWND_TOPMOST),
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_SHOWWINDOW,
        )
    }
    .map_err(|error| format!("show field for native pixel verification: {error}"))?;
    let id = session.field_id();
    // The last key held Ctrl+Shift; pointer presses below must not read it as Shift-extend.
    modifiers(&mut session.app, active, id, ModifiersState::empty());
    let long = long_query();
    ensure!(session.state()?.0 == long, "long paste: {:?}", session.state()?);
    render(&mut session.app, active, id);
    let end = session.caret()?;
    let width = WINDOW_SIZE.0 as f32;
    ensure!(end.x >= 0.0 && end.x + end.w <= width, "long query caret is clipped inside: {end:?}");
    let row_y = caret_center(end);
    // This sample stays strictly between the drag point and the anchor on every field.
    let reverse_sample = (end.x - 60.0) as u32;
    let before_reverse = session
        .app
        .__test_window_software_frame_pixel_bgra(id, reverse_sample, row_y as u32)
        .ok_or("reverse sample pixel")?;

    // Reverse drag at a fixed visible point: redraw must not move text under it.
    let press = (end.x - 1.0, row_y);
    let Some(FieldHit::Offset(press_offset)) =
        session.app.__test_field_hit(id, press, FieldHitMode::Press)
    else {
        return Err(format!("press at {press:?} misses the presented field"));
    };
    pointer_at(&mut session.app, active, id, press);
    button(&mut session.app, active, id, ElementState::Pressed);
    ensure!(session.app.__test_field_pointer_capture() == Some((id, true)), "press starts a drag");
    let fixed_point = (end.x - 120.0, row_y);
    let fixed_hit = session.app.__test_field_hit(id, fixed_point, FieldHitMode::Drag);
    pointer_at(&mut session.app, active, id, fixed_point);
    let (_, reverse, caret) = session.state()?;
    let Some(reverse) = reverse else {
        return Err("reverse drag selected nothing".into());
    };
    ensure!(
        reverse.end == press_offset && reverse.start == caret,
        "reverse keeps the press anchor"
    );
    ensure!(reverse.start > 0, "a long query's left edge is clipped, not offset 0: {reverse:?}");
    ensure!(long.is_char_boundary(reverse.start), "drag offsets stay on char boundaries");
    for _ in 0..4 {
        render(&mut session.app, active, id);
        let hit = session.app.__test_field_hit(id, fixed_point, FieldHitMode::Drag);
        ensure!(hit == fixed_hit, "reverse redraw moves the fixed hit: {fixed_hit:?} -> {hit:?}");
        pointer_at(&mut session.app, active, id, fixed_point);
        ensure!(session.state()?.1 == Some(reverse.clone()), "stationary drag changes selection");
    }
    let reverse_pixel = session
        .app
        .__test_window_software_frame_pixel_bgra(id, reverse_sample, row_y as u32)
        .ok_or("selected reverse sample pixel")?;
    let reverse_native = gdi_pixel(session.field_window(), reverse_sample, row_y as u32)?;
    ensure!(reverse_pixel != before_reverse && reverse_native == colorref(reverse_pixel),
        "fixed reverse highlight not visible: before={before_reverse:?}, selected={reverse_pixel:?}, HWND={reverse_native:#08x}");
    button(&mut session.app, active, id, ElementState::Released);
    ensure!(session.app.__test_field_pointer_capture().is_none(), "release ends the drag");
    println!("reverse fixed[{id:?}]: hit={fixed_hit:?} selection={reverse:?} sample=({reverse_sample},{row_y}) HWND={reverse_native:#08x}");
    // Highlight proof at one query, caret and scroll: select leftward of the caret, which
    // stays on screen, then compare with the same caret collapsed. Only the selection differs.
    render(&mut session.app, active, id);
    let anchor_caret = session.caret()?;
    let caret_offset = session.state()?.2;
    let mut tried = Vec::new();
    let mut highlight_press = None;
    for delta in [200.0, 160.0, 120.0, 80.0] {
        let point = (anchor_caret.x - delta, row_y);
        let hit = session.app.__test_field_hit(id, point, FieldHitMode::Press);
        tried.push((delta, hit));
        if let Some(FieldHit::Offset(offset)) = hit {
            if offset + 4 <= caret_offset {
                highlight_press = Some((point, offset));
                break;
            }
        }
    }
    let Some((highlight_point, highlight_start)) = highlight_press else {
        return Err(format!("no on-screen point left of caret {anchor_caret:?}: {tried:?}"));
    };
    pointer_at(&mut session.app, active, id, highlight_point);
    button(&mut session.app, active, id, ElementState::Pressed);
    pointer_at(&mut session.app, active, id, (anchor_caret.x + 0.5, row_y));
    button(&mut session.app, active, id, ElementState::Released);
    let selected_state = session.state()?;
    ensure!(
        selected_state.1 == Some(highlight_start..caret_offset) && selected_state.2 == caret_offset,
        "on-screen selection {highlight_start}..{caret_offset}: {selected_state:?}"
    );
    render(&mut session.app, active, id);
    let selected_caret = session.caret()?;
    let selected = frame_row(&session.app, id, row_y as u32)?;
    let native_selected: Vec<u32> = (0..WINDOW_SIZE.0)
        .map(|pixel_x| gdi_pixel(session.field_window(), pixel_x, row_y as u32))
        .collect::<Result<_, _>>()?;
    ensure!(session.app.__test_collapse_field_selection(id), "collapse keeps the caret");
    let collapsed_state = session.state()?;
    ensure!(
        collapsed_state.1.is_none() && collapsed_state.2 == caret_offset,
        "collapsed at the caret: {collapsed_state:?}"
    );
    render(&mut session.app, active, id);
    ensure!(session.caret()? == selected_caret, "collapse kept caret and scroll unchanged");
    let collapsed_row = frame_row(&session.app, id, row_y as u32)?;
    let changed: Vec<u32> = (0..WINDOW_SIZE.0)
        .filter(|&pixel_x| selected[pixel_x as usize] != collapsed_row[pixel_x as usize])
        .collect();
    ensure!(changed.len() >= 40, "selection highlight changed only {} pixels", changed.len());
    let (span_left, span_right) =
        (highlight_point.0 - 20.0, selected_caret.x + selected_caret.w + 2.0);
    ensure!(
        changed.iter().all(|&pixel_x| (span_left..=span_right).contains(&(pixel_x as f32))),
        "highlight pixels outside {span_left}..{span_right}: {changed:?}"
    );
    let sample = changed[changed.len() / 2];
    let native_collapsed = gdi_pixel(session.field_window(), sample, row_y as u32)?;
    let native = native_selected[sample as usize];
    ensure!(
        native == colorref(selected[sample as usize])
            && native_collapsed == colorref(collapsed_row[sample as usize])
            && native != native_collapsed,
        "HWND ({sample}, {row_y}) selected={native:#08x} collapsed={native_collapsed:#08x}"
    );
    let native_matches = changed
        .iter()
        .filter(|&&pixel_x| {
            native_selected[pixel_x as usize] == colorref(selected[pixel_x as usize])
        })
        .count();
    ensure!(
        native_matches == changed.len(),
        "HWND shows {native_matches}/{} highlight px",
        changed.len()
    );

    // Outside-edge motion remains stable through redraws instead of auto-scrolling.
    let start = session.caret()?;
    pointer_at(&mut session.app, active, id, (start.x + 1.0, row_y));
    button(&mut session.app, active, id, ElementState::Pressed);
    for outside_x in [-100.0, width + 100.0] {
        let point = (outside_x, row_y);
        let expected = session.app.__test_field_hit(id, point, FieldHitMode::Drag);
        pointer_at(&mut session.app, active, id, point);
        let state = session.state()?;
        for _ in 0..4 {
            render(&mut session.app, active, id);
            ensure!(
                session.app.__test_field_hit(id, point, FieldHitMode::Drag) == expected,
                "outside edge {outside_x} moved after redraw"
            );
            pointer_at(&mut session.app, active, id, point);
            ensure!(session.state()? == state, "outside edge {outside_x} changed the selection");
        }
    }
    button(&mut session.app, active, id, ElementState::Released);

    // Forward drag: return to the visible interior anchor, then drag past the right edge.
    let press = (start.x + 1.0, caret_center(start));
    let Some(FieldHit::Offset(forward_anchor)) =
        session.app.__test_field_hit(id, press, FieldHitMode::Press)
    else {
        return Err(format!("forward press at {press:?} misses the field"));
    };
    pointer_at(&mut session.app, active, id, press);
    button(&mut session.app, active, id, ElementState::Pressed);
    pointer_at(&mut session.app, active, id, (width - 2.0, press.1));
    let forward = session.state()?.1;
    ensure!(
        forward
            .as_ref()
            .is_some_and(|range| range.start == forward_anchor && range.end > forward_anchor),
        "forward drag selects from its press: {forward:?} anchor={forward_anchor}"
    );
    button(&mut session.app, active, id, ElementState::Released);

    // Shift+press extends from the caret instead of collapsing it.
    pointer_at(&mut session.app, active, id, press);
    button(&mut session.app, active, id, ElementState::Pressed);
    button(&mut session.app, active, id, ElementState::Released);
    render(&mut session.app, active, id);
    let collapsed = session.state()?;
    ensure!(collapsed.1.is_none(), "a plain click collapses: {collapsed:?}");
    let caret = session.caret()?;
    // Pick a presented point on either side of the caret that maps to a different offset.
    let mut tried = Vec::new();
    let mut found = None;
    for delta in [60.0, -60.0, 30.0, -30.0, 120.0, -120.0] {
        let point = (caret.x + delta, caret_center(caret));
        let hit = session.app.__test_field_hit(id, point, FieldHitMode::Press);
        tried.push((delta, hit));
        if let Some(FieldHit::Offset(offset)) = hit {
            if offset != collapsed.2 {
                found = Some((point, offset));
                break;
            }
        }
    }
    let Some((extend_point, extend_to)) = found else {
        return Err(format!("no Shift press point near caret {caret:?}: {tried:?}"));
    };
    modifiers(&mut session.app, active, id, ModifiersState::SHIFT);
    pointer_at(&mut session.app, active, id, extend_point);
    button(&mut session.app, active, id, ElementState::Pressed);
    button(&mut session.app, active, id, ElementState::Released);
    modifiers(&mut session.app, active, id, ModifiersState::empty());
    let extended = session.state()?.1;
    ensure!(
        extended == Some(collapsed.2.min(extend_to)..collapsed.2.max(extend_to))
            && extend_to != collapsed.2,
        "Shift press extends from caret {} to {extend_to}: {extended:?}",
        collapsed.2
    );
    println!(
        "pointer[{id:?}]: reverse={reverse:?} forward={forward:?} shift={extended:?} \
         highlight_px={} gdi_sample=({sample},{row_y})={native:#08x}/{native_collapsed:#08x}",
        changed.len()
    );
    Ok(())
}

pub(super) fn teardown_commands(session: &mut Session, _: &ActiveEventLoop) -> Result<(), String> {
    ensure!(!session.app.__test_palette_open(), "native Escape closes the palette");
    ensure!(session.app.__test_pty_write_log().is_empty(), "field keys never reach the PTY");
    Ok(())
}

pub(super) fn teardown_rename_tab(
    session: &mut Session,
    _: &ActiveEventLoop,
) -> Result<(), String> {
    ensure!(!session.app.__test_palette_open(), "Enter commits the rename");
    let title = session
        .app
        .main_tabs()
        .and_then(|tabs| tabs.active())
        .and_then(|tab| tab.custom_title.clone());
    ensure!(title == Some(long_query()), "tab title keeps the literal ▏: {title:?}");
    ensure!(session.app.__test_pty_write_log().is_empty(), "field keys never reach the PTY");
    Ok(())
}

pub(super) fn teardown_rename_window(
    session: &mut Session,
    _: &ActiveEventLoop,
) -> Result<(), String> {
    ensure!(!session.app.__test_palette_open(), "Enter commits the window name");
    let name = session.app.__test_window_custom_name(session.main).map(str::to_owned);
    ensure!(name == Some(long_query()), "window name keeps the literal ▏: {name:?}");
    ensure!(session.app.__test_pty_write_log().is_empty(), "field keys never reach the PTY");
    Ok(())
}

pub(super) fn teardown_child_search(
    session: &mut Session,
    _: &ActiveEventLoop,
) -> Result<(), String> {
    ensure!(session.app.__test_field_state(session.child).is_none(), "native Escape closes search");
    ensure!(session.app.__test_child_read_only(session.child) == Some(true), "READONLY is kept");
    ensure!(
        session.app.__test_pty_write_log().is_empty(),
        "the READONLY child attempted no PTY write"
    );
    Ok(())
}
