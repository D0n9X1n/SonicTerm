//! Cursor-glide cases for the class-damage target; native event-loop ownership stays in its root.

use std::time::{Duration, Instant};

use super::*;

/// One glide state a case presents: the cursor's target cell and the glide clock's offset from the
/// case's start, or `None` for a hidden cursor that cancels the glide, and whether the frame must
/// draw the cursor mid-glide.
type GlideStep = (&'static str, Option<(u16, u16)>, u64, bool);

/// A glide from the parked cursor down two rows, sampled twice mid-way and landed; then a second
/// glide back up, cancelled mid-way by hiding the cursor, which erases the fractional sample's ink,
/// and the cursor shown again on its cell: every frame damages only the rows its samples cross and
/// equals a full repaint.
const DOWN_AND_LAND: [GlideStep; 7] = [
    ("start", Some((CURSOR_ROW + 2, CURSOR_COL + 4)), 20, true),
    ("mid-way", Some((CURSOR_ROW + 2, CURSOR_COL + 4)), 50, true),
    ("landed", Some((CURSOR_ROW + 2, CURSOR_COL + 4)), 400, false),
    ("back start", Some((CURSOR_ROW, CURSOR_COL)), 420, true),
    ("back mid-way", Some((CURSOR_ROW, CURSOR_COL)), 445, true),
    ("cancelled mid-way", None, 450, false),
    ("shown again", Some((CURSOR_ROW, CURSOR_COL)), 900, false),
];

/// A glide up onto the wide row's first CJK character, two columns wide, then landed.
const ONTO_WIDE: [GlideStep; 3] = [
    ("onto wide start", Some((WIDE_ROW, 0)), 20, true),
    ("onto wide mid-way", Some((WIDE_ROW, 0)), 45, true),
    ("onto wide landed", Some((WIDE_ROW, 0)), 400, false),
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
    for (step, target, offset_ms, gliding) in steps {
        let name = format!("{case}: {step}");
        renderer.__pin_glide_clock(Some(start + Duration::from_millis(*offset_ms)));
        match target {
            Some((row, col)) => {
                scene.cursor_visible = true;
                scene.grid.goto(*row, *col);
            }
            None => scene.cursor_visible = false,
        }
        let (damage, ..) = narrow_matches_full(renderer, layout, scene, &name)?;
        // The full twin re-samples the same pinned instant, so the glide state is this frame's.
        check(
            renderer.cursor_gliding() == *gliding,
            &format!("{name}: the cursor {} mid-glide", if *gliding { "is" } else { "is not" }),
        )?;
        // The previous and current samples cross at most four rows, plus each strip's ink pad of
        // about one font height: well under half of this pane, which whole-pane damage, the
        // behaviour this narrows, would cover entirely.
        check(
            damage.damage.h < layout.pane.h / 2,
            &format!("{name}: damage {:?} spans only the glide's rows", damage.damage),
        )?;
    }
    scene.cursor_visible = true;
    Ok(())
}

/// Test 9: a smooth cursor glide's frames, mid-way, landing and cancelled mid-way, on the primary
/// and the alternate screen, for every cursor shape and onto a wide cell, at a fractional cell
/// height: each frame glides when it should, damages only the glide's rows, and equals a full
/// repaint. A preedit or any other overlay disables the glide and repaints the whole surface
/// (`overlay_active` in `core_assemble.rs`), so it needs no narrow case.
pub(super) fn glide_parity(renderer: &mut GpuRenderer, layout: &Layout) -> Result<(), String> {
    // The compressed pitch makes the cell height fractional, so samples land between pixel rows.
    check(
        layout.cell_h.fract() != 0.0,
        &format!(
            "the cell height {} is fractional, so glide edges fall between pixels",
            layout.cell_h
        ),
    )?;
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
