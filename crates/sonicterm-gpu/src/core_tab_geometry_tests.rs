//! Renderer tab geometry contracts, sharing one root fixture owner.

use super::super::core_tests::*;
use super::*;

#[test]
fn unfocused_window_dims_the_active_panel_marker_rather_than_hiding_it() {
    // The accent answers "which tab is active", which is true of the window
    // whether or not it holds keyboard focus. Suppressing it on blur made an
    // unfocused window say nothing about its own state. It now dims instead,
    // so both facts stay readable at once.
    let mut tabs = sonicterm_render_model::boundary::ui::tabs::TabBar::new();
    tabs.push(sonicterm_render_model::boundary::ui::tabs::Tab::new("one"));
    tabs.push(sonicterm_render_model::boundary::ui::tabs::Tab::new("two"));
    tabs.set_active_custom_color("#fabd2f");
    tabs.activate(0);
    tabs.set_active_custom_color("#83a598");

    let layout =
        sonicterm_render_model::boundary::ui::tabbar_view::TabBarLayout::compute_with_height(
            &tabs, 400.0, 40.0,
        );

    let emit = |alpha: f32| {
        let mut quads = Vec::new();
        emit_tab_bar_quads(
            &mut quads,
            &layout,
            &TabBarQuadParams {
                accent: [1.0, 0.0, 0.0, 1.0],
                separator: [0.5, 0.5, 0.5, 1.0],
                border: [0.0, 0.0, 0.0, 1.0],
                hover_tab_idx: u32::MAX,
                surface: (400.0, 80.0),
                active_panel_marker_alpha: alpha,
            },
        );
        quads
    };

    let focused = emit(ACTIVE_PANEL_MARKER_ALPHA_FOCUSED);
    let unfocused = emit(ACTIVE_PANEL_MARKER_ALPHA_UNFOCUSED);

    // Asserting only that a quad exists would pass if the dimming were
    // dropped, and asserting only the count would pass if the color were
    // wrong. Both the presence and the difference have to hold.
    assert_eq!(
        unfocused.len(),
        focused.len(),
        "the unfocused bar must still be drawn, not omitted"
    );

    // Located by geometry, not by position in the list: the separator quad is
    // pushed after the accent, so `.last()` returns the separator and would
    // compare two identical greys that never change with focus — passing
    // whatever the accent did.
    let accent_rect = {
        let t = layout.tabs.iter().find(|t| layout.active == Some(t.idx)).expect("an active tab");
        let scale = (t.bg_rect.h
            / (sonicterm_render_model::boundary::ui::tabbar_view::TAB_BAR_HEIGHT
                - 2.0 * sonicterm_render_model::boundary::ui::tabbar_view::TAB_VERT_INSET))
            .max(0.1);
        let inset =
            sonicterm_render_model::boundary::ui::tabbar_view::ACTIVE_TOP_ACCENT_INSET * scale;
        px_to_ndc(
            t.bg_rect.x + inset,
            t.bg_rect.y + 1.0 * scale,
            (t.bg_rect.w - inset * 2.0).max(0.0),
            sonicterm_render_model::boundary::ui::tabbar_view::ACTIVE_TOP_ACCENT_H * scale,
            400.0,
            80.0,
        )
    };
    let find_accent = |quads: &[QuadInstance]| {
        *quads.iter().find(|q| q.rect == accent_rect).expect("an accent quad at the accent rect")
    };

    let focused_accent = find_accent(&focused);
    let unfocused_accent = find_accent(&unfocused);
    assert_ne!(
        focused_accent.color, unfocused_accent.color,
        "the unfocused bar must be visibly dimmer, not identical"
    );

    // Premultiplied blending: every channel scales together. Alpha alone
    // would leave the bar brighter than its alpha claims.
    assert_eq!(focused_accent.color, [0.226_965_87, 0.376_262_13, 0.313_988_72, 1.0]);
    assert_eq!(
        unfocused_accent.color,
        scale_premultiplied_alpha(focused_accent.color, ACTIVE_PANEL_MARKER_ALPHA_UNFOCUSED)
    );
}

/// Quad opacity changes must use helpers that rescale RGB with premultiplied alpha.
#[test]
fn authored_quad_sources_do_not_mutate_alpha_channels_directly() {
    for (name, source) in [("core", crate::core::CORE_SOURCE), ("quad", include_str!("quad.rs"))] {
        assert!(!source.contains("[3] ="), "{name} directly replaces quad alpha");
        assert!(!source.contains("[3] *="), "{name} directly scales only quad alpha");
    }
}

#[test]
fn a_fully_transparent_marker_alpha_emits_no_accent_quad() {
    // The alpha is not a visibility flag, but zero still has to mean absent
    // rather than an invisible quad occupying a draw slot.
    let mut tabs = sonicterm_render_model::boundary::ui::tabs::TabBar::new();
    tabs.push(sonicterm_render_model::boundary::ui::tabs::Tab::new("one"));
    tabs.push(sonicterm_render_model::boundary::ui::tabs::Tab::new("two"));
    tabs.activate(0);

    let layout =
        sonicterm_render_model::boundary::ui::tabbar_view::TabBarLayout::compute_with_height(
            &tabs, 400.0, 40.0,
        );
    let mut quads = Vec::new();
    emit_tab_bar_quads(
        &mut quads,
        &layout,
        &TabBarQuadParams {
            accent: [1.0, 0.0, 0.0, 1.0],
            separator: [0.5, 0.5, 0.5, 1.0],
            border: [0.0, 0.0, 0.0, 1.0],
            hover_tab_idx: u32::MAX,
            surface: (400.0, 80.0),
            active_panel_marker_alpha: 0.0,
        },
    );

    assert_eq!(quads.len(), 3);
}

#[test]
fn custom_tab_color_emits_focused_panel_marker_once() {
    let mut tabs = sonicterm_render_model::boundary::ui::tabs::TabBar::new();
    tabs.push(sonicterm_render_model::boundary::ui::tabs::Tab::new("one"));
    tabs.push(sonicterm_render_model::boundary::ui::tabs::Tab::new("two"));
    tabs.set_active_custom_color("#fabd2f");
    tabs.activate(0);
    tabs.set_active_custom_color("#83a598");

    let layout =
        sonicterm_render_model::boundary::ui::tabbar_view::TabBarLayout::compute_with_height(
            &tabs, 400.0, 40.0,
        );
    let mut quads = Vec::new();
    emit_tab_bar_quads(
        &mut quads,
        &layout,
        &TabBarQuadParams {
            accent: [1.0, 0.0, 0.0, 1.0],
            separator: [0.5, 0.5, 0.5, 1.0],
            border: [0.0, 0.0, 0.0, 1.0],
            hover_tab_idx: u32::MAX,
            surface: (400.0, 80.0),
            active_panel_marker_alpha: ACTIVE_PANEL_MARKER_ALPHA_FOCUSED,
        },
    );

    assert_eq!(quads.len(), 4);
}

#[test]
fn default_tab_color_brightens_on_hover() {
    // No custom colour: an inactive tab uses inactive_fg, but hovering it
    // (or activating it) swaps to active_fg — the historical default.
    let inactive = tab_title_color(None, false, false, false, active_fg(), inactive_fg());
    assert_eq!(inactive, inactive_fg());

    let hovered = tab_title_color(None, false, true, false, active_fg(), inactive_fg());
    assert_eq!(hovered, active_fg(), "default tab must brighten under the cursor");

    let active = tab_title_color(None, true, false, true, active_fg(), inactive_fg());
    assert_eq!(active, active_fg());
}

#[test]
fn custom_tab_color_brightens_on_hover() {
    // Regression: a user-set custom title colour must light up on hover just
    // like a default tab, instead of staying dimmed to 0.55 alpha.
    let custom = "#83a598";
    let full = hex_to_chrome_color(custom);

    // Inactive, unhovered, unfocused panel → dimmed.
    let dimmed = tab_title_color(Some(custom), false, false, false, active_fg(), inactive_fg());
    assert!(dimmed.a() < 255, "inactive custom tab should be dimmed");
    assert_eq!((dimmed.r(), dimmed.g(), dimmed.b()), (full.r(), full.g(), full.b()));

    // Hovered → full strength (the fix).
    let hovered = tab_title_color(Some(custom), false, true, false, active_fg(), inactive_fg());
    assert_eq!(hovered, full, "hovered custom tab must paint at full alpha");

    // Active tab → full strength regardless of hover.
    let active = tab_title_color(Some(custom), true, false, true, active_fg(), inactive_fg());
    assert_eq!(active, full);

    // Focused panel keeps a custom inactive tab at full strength (unchanged).
    let focused = tab_title_color(Some(custom), false, false, true, active_fg(), inactive_fg());
    assert_eq!(focused, full);
}

/// Changing process privilege must invalidate otherwise-identical retained tab chrome.
#[test]
fn process_privilege_change_invalidates_the_frame_key() {
    let ordinary = revision_plan(7, 1).key;
    let mut privileged = ordinary.clone();
    privileged.window.process_privileged = true;

    assert_ne!(ordinary, privileged);
}

/// Process elevation marks every tab, while foreground elevation marks only its owning tab.
#[test]
fn privilege_badge_combines_process_and_per_tab_foreground_state() {
    let mut tabs = TabBar::new();
    tabs.push(sonicterm_render_model::boundary::ui::tabs::Tab::new("#1 regular"));
    tabs.push(sonicterm_render_model::boundary::ui::tabs::Tab::new("#2 gsudo"));
    tabs.set_foreground_privileged(1, true);
    let now = Instant::now();
    let marked = |process_privileged: bool| -> Vec<bool> {
        tabs.tabs()
            .iter()
            .map(|tab| TabContent::of(tab, now, false, process_privileged).privileged)
            .collect()
    };

    assert_eq!(marked(false), [false, true]);
    assert_eq!(marked(true), [true, true]);
}

/// A foreground elevation-only change participates in the tab hash and invalidates the frame.
#[test]
fn foreground_privilege_change_invalidates_tab_chrome() {
    let mut ordinary = TabBar::new();
    ordinary.push(sonicterm_render_model::boundary::ui::tabs::Tab::new("#1 shell"));
    let mut privileged = TabBar::new();
    privileged.push(sonicterm_render_model::boundary::ui::tabs::Tab::new("#1 shell"));
    privileged.set_active_foreground_privileged(true);

    assert_ne!(tab_bar_hash(&ordinary, Instant::now()), tab_bar_hash(&privileged, Instant::now()));
}

/// Privileged title layout reserves a bounded lock slot while ordinary titles keep the whole rect.
#[test]
fn privilege_badge_reserves_title_width_without_leaving_the_title_rect() {
    let rect = TabTitleRect { x: 20.0, y: 4.0, w: 180.0, h: 36.0 };
    let reserve = privilege_marker_reserve_px(true, 1.0);
    let ordinary_placement = tab_title_block_placement(rect, 92.0, false, 1.0);
    let placement = tab_title_block_placement(rect, 92.0, true, 1.0);
    let badge = placement.badge_rect.expect("privileged title has a badge");

    assert_eq!(privilege_marker_reserve_px(false, 1.0), 0.0);
    assert_eq!(reserve, PRIVILEGE_BADGE_SIZE_PX + PRIVILEGE_BADGE_GAP_PX);
    assert_eq!(privilege_marker_reserve_px(true, 2.0), reserve * 2.0);
    assert_eq!(ordinary_placement.badge_rect, None);
    assert_eq!(ordinary_placement.text_x, 64.0);
    assert_eq!(ordinary_placement.text_clip, rect);
    assert!(badge.x >= rect.x && badge.y >= rect.y);
    assert!(badge.x + badge.w <= rect.x + rect.w);
    assert!(badge.y + badge.h <= rect.y + rect.h);
    assert!(placement.text_x >= badge.x + badge.w);
    assert!(placement.text_clip.x >= badge.x + badge.w);
    assert!(placement.text_clip.x + placement.text_clip.w <= rect.x + rect.w);
    // A title as wide as the rect less the reserve fits beside the marker unclipped.
    let snug = tab_title_block_placement(rect, rect.w - reserve, true, 1.0);
    assert!(snug.text_clip.w >= rect.w - reserve - 0.001);
}

/// A tab's stored width is the marker reserve plus the drawn width of its badge and title, so
/// CJK and emoji titles are measured by their drawn advance rather than by scalar count.
#[test]
fn tab_content_width_is_the_marker_reserve_plus_the_drawn_badge_and_title() {
    let _lock = font_fixture_lock();
    let stack = crate::lib_tests::tracked_font_stack(15.0);
    let mut tabs = TabBar::new();
    for title in ["#1 zsh", "#2 任务完成", "#3 \u{1f469}\u{200d}\u{1f4bb} ship it"] {
        tabs.push(sonicterm_render_model::boundary::ui::tabs::Tab::new(title));
    }
    let now = Instant::now();
    for tab in tabs.tabs() {
        for privileged in [false, true] {
            let content = TabContent::of(tab, now, false, privileged);
            let drawn = drawn_tab_title_px(&stack, &content.display_text(), 15.0);
            let measured = tab_content_width_px(Some(&stack), &content, 15.0, 1.0)
                .expect("the tracked font shapes every title");
            let reserve = privilege_marker_reserve_px(privileged, 1.0);
            assert!(
                (measured - reserve - drawn).abs() < 0.01,
                "{:?}: measured {measured}, drawn {drawn}, reserve {reserve}",
                content.title
            );
        }
    }

    let until = now + std::time::Duration::from_secs(5);
    tabs.set_command_status(
        0,
        sonicterm_render_model::boundary::ui::tabs::CommandStatus::Done { exit: Some(1), until },
    );
    let failed = TabContent::of(&tabs.tabs()[0], now, false, false);
    assert_eq!(failed.display_text(), "✗ #1 zsh");
    let measured = tab_content_width_px(Some(&stack), &failed, 15.0, 1.0).expect("shaped");
    assert!((measured - drawn_tab_title_px(&stack, "✗ #1 zsh", 15.0)).abs() < 0.01);
    let expired = TabContent::of(&tabs.tabs()[0], until, false, false);
    assert_eq!(expired.display_text(), "#1 zsh");
    let no_font = TabContent { privileged: true, ..expired };
    assert_eq!(
        tab_content_width_px(None, &no_font, 15.0, 1.0),
        Some(privilege_marker_reserve_px(true, 1.0))
    );
}

/// A title that fits its stored width is drawn whole; a longer one is cut at a grapheme
/// boundary with `…`, and the drawn text stays inside its tab for ASCII, CJK and emoji titles.
#[test]
fn tab_titles_fit_their_stored_width_whole_or_cut_at_a_grapheme_boundary() {
    let _lock = font_fixture_lock();
    let stack = crate::lib_tests::tracked_font_stack(15.0);
    let coder = "\u{1f469}\u{200d}\u{1f4bb}";
    let titles = [
        "#1 zsh".to_string(),
        "#2 ~/work/fun-code/sonicterm/crates/sonicterm-app/src".to_string(),
        format!("#3 {}", "任务完成".repeat(4)),
        format!("#4 {}", coder.repeat(24)),
    ];
    let mut tabs = TabBar::new();
    for title in &titles {
        tabs.push(sonicterm_render_model::boundary::ui::tabs::Tab::new(title.as_str()));
    }
    let now = Instant::now();
    tabs.refresh_content_widths(now, false, 1, false, |content| {
        tab_content_width_px(Some(&stack), content, 15.0, 1.0)
    });
    let layout = TabBarLayout::compute_with_height(&tabs, 1600.0, 40.0);

    assert_eq!(layout.tabs.len(), titles.len());
    let mut cut = Vec::new();
    for widget in &layout.tabs {
        let tab = &tabs.tabs()[widget.idx];
        let content = TabContent::of(tab, now, layout.active == Some(widget.idx), false);
        let display = content.display_text();
        let fitted =
            crate::chrome_cache::fit_title_run(&stack, &display, 15.0, widget.title_rect.w);
        assert!(fitted.complete, "the tracked font shapes every title");
        let drawn = drawn_tab_title_px(&stack, &fitted.text, 15.0);
        assert!(
            drawn <= widget.title_rect.w + TITLE_FIT_TOLERANCE_PX,
            "{:?} draws {drawn} px in a {} px title rect",
            fitted.text,
            widget.title_rect.w
        );
        let is_cut = fitted.text != display;
        if is_cut {
            let kept = fitted.text.strip_suffix('…').expect("a cut title ends with an ellipsis");
            assert!(display.starts_with(kept), "{kept:?} is not a prefix of {display:?}");
            assert!(!kept.ends_with('\u{200d}'), "{kept:?} splits a joined emoji");
            assert!(!display[kept.len()..].starts_with('\u{200d}'), "{kept:?} splits an emoji");
        } else {
            assert_eq!(fitted.text, display);
        }
        cut.push(is_cut);
    }
    assert!(!cut[0], "a short title that fits is drawn whole");
    assert!(cut[1], "a path wider than the maximum is cut");
}

/// A stored width is part of the tab hash, so a released held width repaints the retained
/// strip even when no title changed between the two frames.
#[test]
fn stored_tab_widths_invalidate_the_retained_tab_strip() {
    let now = Instant::now();
    let mut tabs = TabBar::new();
    tabs.push(sonicterm_render_model::boundary::ui::tabs::Tab::new("zsh"));
    tabs.push(sonicterm_render_model::boundary::ui::tabs::Tab::new("vim"));
    tabs.refresh_content_widths(now, false, 1, false, ten_px_per_char);
    let id = tabs.tabs()[0].id;
    tabs.set_title(id, "cargo build --release");
    tabs.refresh_content_widths(now, false, 1, true, ten_px_per_char);
    let held = tab_bar_hash(&tabs, now);

    tabs.refresh_content_widths(now, false, 1, false, ten_px_per_char);

    assert_ne!(tab_bar_hash(&tabs, now), held, "a released width must repaint the strip");
}

/// Either width limit moves idle tabs, so a `tab_min_width` or `tab_max_width` change
/// repaints the retained strip even when no title or stored width changed.
#[test]
fn tab_width_limits_invalidate_the_retained_tab_strip() {
    let now = Instant::now();
    let mut tabs = TabBar::new();
    tabs.push(sonicterm_render_model::boundary::ui::tabs::Tab::new("zsh"));
    tabs.refresh_content_widths(now, false, 1, false, ten_px_per_char);
    let defaults = tab_bar_hash_with_limits(&tabs, now, 240.0, 320.0);

    assert_eq!(tab_bar_hash_with_limits(&tabs, now, 240.0, 320.0), defaults);
    assert_ne!(tab_bar_hash_with_limits(&tabs, now, 200.0, 320.0), defaults, "tab_min_width");
    assert_ne!(tab_bar_hash_with_limits(&tabs, now, 240.0, 400.0), defaults, "tab_max_width");
}

/// Any change to the family, size, weight, DPI scale or tab font measures every tab again,
/// even on a held bar.
#[test]
fn every_font_and_scale_input_changes_the_tab_font_key() {
    let base = tab_font_key("Rec Mono St.Helens", 14.0, 1.0, 2.0, true);
    assert_eq!(base, tab_font_key("Rec Mono St.Helens", 14.0, 1.0, 2.0, true));
    for other in [
        tab_font_key("Menlo", 14.0, 1.0, 2.0, true),
        tab_font_key("Rec Mono St.Helens", 15.0, 1.0, 2.0, true),
        tab_font_key("Rec Mono St.Helens", 14.0, 1.25, 2.0, true),
        tab_font_key("Rec Mono St.Helens", 14.0, 1.0, 1.0, true),
        tab_font_key("Rec Mono St.Helens", 14.0, 1.0, 2.0, false),
    ] {
        assert_ne!(other, base);
    }
}

/// Every privileged tab emits one bounded vector lock made from the same quad stream as other chrome.
#[test]
fn privilege_badge_emits_one_vector_lock_per_tab() {
    let mut quads = Vec::new();
    let first = TabTitleRect { x: 10.0, y: 2.0, w: 18.0, h: 18.0 };
    let second = TabTitleRect { x: 40.0, y: 2.0, w: 18.0, h: 18.0 };
    let danger = [0.8, 0.05, 0.02, 1.0];

    emit_privilege_badge_quads(&mut quads, first, danger, 1.0, (200.0, 40.0));
    emit_privilege_badge_quads(&mut quads, second, danger, 0.5, (200.0, 40.0));

    assert_eq!(quads.len(), PRIVILEGE_BADGE_QUAD_COUNT * 2);
    assert_eq!(quads[PRIVILEGE_BADGE_QUAD_COUNT].color[3], 0.5);
    assert_eq!(quads[PRIVILEGE_BADGE_QUAD_COUNT + 1].color[3], 0.5);
    for part in privilege_lock_rects(first) {
        assert!(part.x >= first.x && part.y >= first.y);
        assert!(part.x + part.w <= first.x + first.w);
        assert!(part.y + part.h <= first.y + first.h);
    }
}

/// Black-or-white lock geometry keeps at least WCAG AA contrast against varied danger colors.
#[test]
fn privilege_lock_color_contrasts_with_dark_light_and_high_contrast_badges() {
    for danger in [
        [0.8, 0.05, 0.02, 1.0],
        [0.08, 0.01, 0.01, 1.0],
        [1.0, 0.72, 0.72, 1.0],
        [1.0, 0.0, 0.0, 1.0],
    ] {
        let lock = privilege_lock_color(danger);
        assert!(linear_contrast_ratio(danger, lock) >= 4.5);
    }
}

/// Hover, activity, custom title color, and focus do not enter privilege-badge resolution.
#[test]
fn privilege_badge_uses_only_process_state_theme_danger_and_drag_alpha() {
    // The call stays outside tab-title color resolution so tab presentation cannot recolor the warning.
    const CORE: &str = crate::core::CORE_SOURCE;
    let start = CORE.find("if let Some(badge) = placement.badge_rect").expect("badge branch");
    let call = &CORE[start..start + 500];

    assert!(call.contains("ui_palette.danger"));
    assert!(call.contains("badge_alpha"));
    for excluded in ["active_panel_focused", "hovered", "custom_color"] {
        assert!(!call.contains(excluded), "badge unexpectedly depends on {excluded}");
    }
}

/// The Windows software compositor consumes the same vector badge quads as the GPU path.
#[cfg(target_os = "windows")]
#[test]
fn privilege_badge_quads_rasterize_on_the_windows_software_path() {
    use sonicterm_text::glyph_atlas::GlyphAtlas;

    let badge = TabTitleRect { x: 10.0, y: 10.0, w: 18.0, h: 18.0 };
    let mut quads = Vec::new();
    emit_privilege_badge_quads(&mut quads, badge, [1.0, 0.0, 0.0, 1.0], 1.0, (40.0, 40.0));
    let glyph_atlas = GlyphAtlas::new(1, 1);
    let image_atlas = GlyphAtlas::new(1, 1);
    let mut frame = crate::software_frame::SoftwareFrame::new(40, 40, [0.0, 0.0, 0.0, 1.0])
        .expect("valid software frame");

    frame.draw_layers(&glyph_atlas, &image_atlas, &quads, &[], &[], &[], &[]);

    assert_eq!(frame.pixel_bgra_at(12, 20), Some([0, 0, 255, 255]));
    assert_eq!(frame.pixel_bgra_at(18, 20), Some([0, 0, 0, 255]));
    assert_eq!(frame.pixel_bgra_at(0, 0), Some([0, 0, 0, 255]));
}

/// The command-status hash changes only when the drawn badge changes: an inactive running tab
/// keeps one hash until its badge appears past five seconds, an active running tab (never badged)
/// keeps one hash throughout, a finished badge changes at its expiry, and an idle tab differs from
/// a running one so a state change still repaints once.
#[test]
fn command_status_hash_follows_only_the_drawn_badge() {
    use sonicterm_render_model::boundary::ui::tabs::CommandStatus;
    use std::time::Duration;
    let started = Instant::now();
    let running = CommandStatus::Running(started);
    let at = |seconds: u64| started + Duration::from_secs(seconds);
    let inactive_early: Vec<u64> =
        (0..=5).map(|seconds| command_status_hash(&running, at(seconds), false)).collect();
    assert!(inactive_early.iter().all(|hash| *hash == inactive_early[0]), "{inactive_early:?}");
    let inactive_late = command_status_hash(&running, at(6), false);
    assert_ne!(inactive_late, inactive_early[0], "the badge appears past five seconds");
    assert_eq!(inactive_late, command_status_hash(&running, at(30), false));
    let active: Vec<u64> =
        (0..=30).map(|seconds| command_status_hash(&running, at(seconds), true)).collect();
    assert!(active.iter().all(|hash| *hash == active[0]), "an active tab draws no badge");

    let until = at(5);
    let done = CommandStatus::Done { exit: Some(0), until };
    for is_active in [false, true] {
        assert_eq!(
            command_status_hash(&done, at(1), is_active),
            command_status_hash(&done, at(4), is_active)
        );
        assert_ne!(
            command_status_hash(&done, at(4), is_active),
            command_status_hash(&done, until, is_active),
            "the finished badge expires at `until`"
        );
    }

    let idle = command_status_hash(&CommandStatus::Idle, at(1), true);
    assert_ne!(idle, command_status_hash(&running, at(1), true), "idle and running differ");
}

/// The tab-bar hash judges each tab's badge as drawn for its activity: a running active tab
/// leaves the bar's hash unchanged as seconds pass, so no frame is planned for an invisible tick.
#[test]
fn tab_bar_hash_ignores_the_running_seconds_of_an_unbadged_tab() {
    use sonicterm_render_model::boundary::ui::tabs::{CommandStatus, Tab};
    use std::time::Duration;
    let mut tabs = TabBar::new();
    tabs.push(Tab::new("active"));
    tabs.push(Tab::new("inactive"));
    tabs.activate(0);
    let started = Instant::now();
    tabs.set_command_status(0, CommandStatus::Running(started));
    tabs.set_command_status(1, CommandStatus::Running(started));
    let at = |seconds: u64| started + Duration::from_secs(seconds);
    let early = tab_bar_hash_with_limits(&tabs, at(1), 240.0, 320.0);
    assert_eq!(early, tab_bar_hash_with_limits(&tabs, at(4), 240.0, 320.0));
    assert_ne!(early, tab_bar_hash_with_limits(&tabs, at(6), 240.0, 320.0), "inactive badge");
}
