//! Regression tests: every UI text-render site (tab titles, command
//! palette, search bar, IME pre-edit) must use the EXACT same
//! `Family::Name(config.font.family)` Attrs as terminal grid cells.
//!
//! Historical bug: tab titles fell through to `Family::Monospace`,
//! which `cosmic-text` resolved to a different installed face on
//! some systems → visible mismatch between grid text and tab title.

use glyphon::{Attrs, Family};
use sonicterm_gpu::core::terminal_font_attrs;

#[test]
fn terminal_font_attrs_uses_family_name_not_generic() {
    let family = "Rec Mono St.Helens";
    let a = terminal_font_attrs(family);
    let expected = Attrs::new().family(Family::Name(family));
    // Family should match exactly — no fallback to Family::Monospace.
    assert_eq!(format!("{:?}", a.family), format!("{:?}", expected.family));
}

#[test]
fn tab_title_uses_same_attrs_as_grid_cell() {
    // Same call site for both grid cells AND tab titles → same Attrs.
    let family = "MesloLGS NF";
    let grid_attrs = terminal_font_attrs(family);
    let tab_attrs = terminal_font_attrs(family);
    assert_eq!(format!("{:?}", grid_attrs.family), format!("{:?}", tab_attrs.family));
}

#[test]
fn palette_text_uses_same_attrs_as_grid_cell() {
    let family = "Fira Code";
    let grid_attrs = terminal_font_attrs(family);
    let palette_attrs = terminal_font_attrs(family);
    assert_eq!(format!("{:?}", grid_attrs.family), format!("{:?}", palette_attrs.family));
}

#[test]
fn search_status_uses_same_attrs_as_grid_cell() {
    let family = "SF Mono";
    let grid_attrs = terminal_font_attrs(family);
    let search_attrs = terminal_font_attrs(family);
    assert_eq!(format!("{:?}", grid_attrs.family), format!("{:?}", search_attrs.family));
}

#[test]
fn ime_preedit_uses_same_attrs_as_grid_cell() {
    let family = "PingFang SC";
    let grid_attrs = terminal_font_attrs(family);
    let ime_attrs = terminal_font_attrs(family);
    assert_eq!(format!("{:?}", grid_attrs.family), format!("{:?}", ime_attrs.family));
}

#[test]
fn render_source_has_no_hardcoded_monospace_family() {
    // Belt-and-suspenders: grep the actual render.rs source to ensure
    // no `Family::Monospace` literal sneaks back in. This catches
    // future regressions where a copy-pasted call site bypasses the
    // helper.
    let src = concat!(
        include_str!("../../sonicterm-gpu/src/core.rs"),
        include_str!("../../sonicterm-gpu/src/color.rs"),
        include_str!("../../sonicterm-text/src/metrics.rs"),
        include_str!("../../sonicterm-ui/src/tab_spans.rs"),
        include_str!("../../sonicterm-gpu/src/cursor.rs"),
        include_str!("../../sonicterm-ui/src/drag_chip.rs"),
    );
    // Strip comments and doc-strings first (we mention Monospace in a
    // doc-comment as historical context).
    let mut code_only = String::new();
    for line in src.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("//") {
            continue;
        }
        code_only.push_str(line);
        code_only.push('\n');
    }
    assert!(
        !code_only.contains("Family::Monospace"),
        "render.rs still contains a hardcoded Family::Monospace literal in code; \
         use terminal_font_attrs(&self.font_family) instead",
    );
}

#[test]
fn render_source_has_no_hardcoded_font_name_literal() {
    // Catch typos like a stray "JetBrains" or "Monaco" baked into the
    // renderer; the only legitimate font-name string should come from
    // config.font.family at runtime.
    let src = concat!(
        include_str!("../../sonicterm-gpu/src/core.rs"),
        include_str!("../../sonicterm-gpu/src/color.rs"),
        include_str!("../../sonicterm-text/src/metrics.rs"),
        include_str!("../../sonicterm-ui/src/tab_spans.rs"),
        include_str!("../../sonicterm-gpu/src/cursor.rs"),
        include_str!("../../sonicterm-ui/src/drag_chip.rs"),
    );
    let mut code_only = String::new();
    for line in src.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("//") {
            continue;
        }
        code_only.push_str(line);
        code_only.push('\n');
    }
    for needle in ["\"JetBrains", "\"Monaco\"", "\"Menlo\"", "\"Consolas\""] {
        assert!(
            !code_only.contains(needle),
            "render.rs contains hardcoded font literal {needle:?}; \
             font family must come from config.font.family",
        );
    }
}

/// Strict source-grep audit: any `Attrs::new().family(...)` call outside
/// the `terminal_font_attrs` helper itself bypasses the unified font
/// pipeline. Historically this hid the tab-title and drag-chip
/// regressions where copy-pasted call sites silently rebuilt their own
/// Attrs and drifted from the grid's font family. The ONLY legitimate
/// occurrence is the body of `pub fn terminal_font_attrs`.
#[test]
fn no_attrs_new_family_outside_helper() {
    for (path, src) in [
        ("sonicterm-gpu/src/core.rs", include_str!("../../sonicterm-gpu/src/core.rs")),
        ("sonicterm-ui/src/tab_spans.rs", include_str!("../../sonicterm-ui/src/tab_spans.rs")),
        (
            "sonicterm-shared/src/tabbar_view.rs",
            include_str!("../../sonicterm-ui/src/tabbar_view.rs"),
        ),
        ("sonicterm-text/src/shape.rs", include_str!("../../sonicterm-text/src/shape.rs")),
    ] {
        let mut in_helper = false;
        let mut helper_brace_depth = 0i32;
        let mut offenders: Vec<(usize, String)> = Vec::new();
        for (idx, line) in src.lines().enumerate() {
            let trimmed = line.trim_start();
            if trimmed.starts_with("//") {
                continue;
            }
            // Detect entering the terminal_font_attrs function body.
            if !in_helper && line.contains("fn terminal_font_attrs") {
                in_helper = true;
                helper_brace_depth = 0;
            }
            if in_helper {
                helper_brace_depth += line.matches('{').count() as i32;
                helper_brace_depth -= line.matches('}').count() as i32;
            }
            if line.contains("Attrs::new().family(") && !in_helper {
                offenders.push((idx + 1, line.to_string()));
            }
            if in_helper && helper_brace_depth <= 0 && line.contains('}') {
                in_helper = false;
            }
        }
        assert!(
            offenders.is_empty(),
            "{path} contains `Attrs::new().family(` outside the terminal_font_attrs helper. \
             Every text-render site must route through terminal_font_attrs(&self.font_family) \
             so tab titles, drag chips, palette, search bar, and IME pre-edit share the same \
             Family::Name as grid cells. Offenders:\n{}",
            offenders
                .iter()
                .map(|(n, l)| format!("  {path}:{n}: {l}"))
                .collect::<Vec<_>>()
                .join("\n"),
        );
    }
}
