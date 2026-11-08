//! Cursor-glide cases for the class-damage target; native event-loop ownership stays in its root.

use std::time::{Duration, Instant};

use super::*;

/// One glide state a case presents: the cursor's target cell and the glide clock's offset from the
/// case's start, or `None` for a hidden cursor that cancels the glide.
type GlideStep = (&'static str, Option<(u16, u16)>, u64);

/// A glide from the parked cursor down two rows, sampled twice mid-way, then landed and cancelled:
/// every frame damages only the rows its sample crosses and equals a full repaint.
const DOWN_AND_LAND: [GlideStep; 4] = [
    ("start", Some((CURSOR_ROW + 2, CURSOR_COL + 4)), 20),
    ("mid-way", Some((CURSOR_ROW + 2, CURSOR_COL + 4)), 50),
    ("landed", Some((CURSOR_ROW + 2, CURSOR_COL + 4)), 400),
    ("cancelled", None, 400),
];

/// A glide up onto the wide row's first CJK character, two columns wide, then landed.
const ONTO_WIDE: [GlideStep; 3] = [
    ("onto wide start", Some((WIDE_ROW, 0)), 20),
    ("onto wide mid-way", Some((WIDE_ROW, 0)), 45),
    ("onto wide landed", Some((WIDE_ROW, 0)), 400),
];

/// Present each of `steps` on `scene` with the glide clock pinned, and require every frame narrow
/// and equal to a full repaint of its state.
fn glide_steps(
    renderer: &mut GpuRenderer,
    layout: &Layout,
    scene: &mut Scene,
    case: &str,
    steps: &[GlideStep],
) -> Result<(), String> {
    let start = Instant::now();
    renderer.__pin_glide_clock(Some(start));
    baseline(renderer, layout, scene)?;
    for (step, target, offset_ms) in steps {
        renderer.__pin_glide_clock(Some(start + Duration::from_millis(*offset_ms)));
        match target {
            Some((row, col)) => {
                scene.cursor_visible = true;
                scene.grid.goto(*row, *col);
            }
            None => scene.cursor_visible = false,
        }
        narrow_matches_full(renderer, layout, scene, &format!("{case}: {step}"))?;
    }
    scene.cursor_visible = true;
    Ok(())
}

/// Test 9: a smooth cursor glide's frames, mid-way, landing and cancelled, on the primary and the
/// alternate screen, for every cursor shape and onto a wide cell, each equal to a full repaint.
pub(super) fn glide_parity(renderer: &mut GpuRenderer, layout: &Layout) -> Result<(), String> {
    renderer.set_cursor_glide_allowed(true);
    let mut failures = Vec::new();
    for shape in [CursorShape::Block, CursorShape::Bar, CursorShape::Underline] {
        renderer.set_cursor_shape(shape);
        let mut primary = scene(layout);
        if let Err(error) =
            glide_steps(renderer, layout, &mut primary, &format!("{shape:?}"), &DOWN_AND_LAND)
        {
            failures.push(error);
        }
    }
    renderer.set_cursor_shape(CursorShape::Block);
    let mut wide = scene(layout);
    if let Err(error) = glide_steps(renderer, layout, &mut wide, "wide", &ONTO_WIDE) {
        failures.push(error);
    }
    // The alternate screen holds its own dense text, written after entering it, so the glide
    // crosses ink exactly as on the primary screen.
    let mut alternate = scene(layout);
    alternate.grid.enter_alt_screen();
    for row in 0..layout.rows {
        alternate.grid.goto(row, 0);
        for character in "MW".repeat(usize::from(layout.cols) / 2).chars() {
            alternate.grid.put_char(character, Color::Default, Color::Default, CellFlags::empty());
        }
    }
    alternate.grid.goto(CURSOR_ROW, CURSOR_COL);
    if let Err(error) = glide_steps(renderer, layout, &mut alternate, "alternate", &DOWN_AND_LAND) {
        failures.push(error);
    }
    renderer.__pin_glide_clock(None);
    renderer.set_cursor_glide_allowed(false);
    if failures.is_empty() {
        Ok(())
    } else {
        Err(failures.join("; "))
    }
}
