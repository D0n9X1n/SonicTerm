use super::*;
use crate::i18n::test_translator as translator;
use crate::i18n::SHIPPED_LOCALES;
use sonicterm_cfg::status_line::{ResolvedSegment, StatusLineConfig};

/// The default (developer) resolved table with the strip enabled.
fn developer() -> ResolvedStatusLine {
    StatusLineConfig::default().resolve().unwrap()
}

/// Concatenated text of a segment's parts.
fn text_of(parts: &[StatusPart]) -> String {
    parts.iter().map(|part| part.text.as_str()).collect()
}

/// The displayed text of segment `id` in `view`, if present.
fn shown(view: &StatusLineView, id: StatusSegmentId) -> Option<String> {
    view.segments.iter().find(|segment| segment.id == id).map(|segment| text_of(&segment.parts))
}

/// A monospace measure: one unit per display column.
fn columns(text: &str) -> f32 {
    text.width() as f32
}

/// Fit metrics with no padding or gaps, so widths are plain column counts.
fn tight(width: f32) -> FitMetrics {
    FitMetrics { width, edge_pad: 0.0, segment_gap: 1.0, chip_pad: 0.0, chip_gap: 0.0 }
}

/// Inputs with a 412 MiB memory figure and nothing else to say.
fn idle_inputs() -> StatusLineInputs {
    StatusLineInputs {
        mem: MemInput {
            bytes: Some(412 * MIB),
            previous_bytes: Some(412 * MIB),
            ..Default::default()
        },
        ..Default::default()
    }
}

#[test]
fn every_catalog_id_is_registered_with_a_glyph_free_spoken_name() {
    // Every id has metadata, a registry entry and a non-empty spoken name with no symbols, in every locale.
    let plain = |text: &str| {
        text.chars().all(|character| {
            character.is_alphanumeric()
                || character.is_whitespace()
                // CJK commas are punctuation a screen reader pauses on, not symbols.
                || "(),.:-'，、".contains(character)
        })
    };
    for locale in SHIPPED_LOCALES {
        let i18n = translator(locale);
        for id in StatusSegmentId::ALL {
            let _spec = segment_spec(id);
            let name = i18n.translate(id.meta().spoken_name_key);
            assert!(!name.is_empty() && name != id.meta().spoken_name_key, "{locale} {id:?}");
            assert!(plain(&name), "{locale} {id:?}: {name}");
        }
        let inputs = StatusLineInputs {
            mode: ModeInput { copy: true, broadcast_receivers: Some(3), mouse_captured: true },
            scroll_lines: 340,
            sel: Some(SelInput::Capped { rows: 12_345 }),
            health: HealthInput {
                config: Some(ReloadFailure { line: Some(12) }),
                render: RenderHealth::SoftwareAdapter,
                gpu_recovering: true,
                ..Default::default()
            },
            ..idle_inputs()
        };
        let view = build_view(&developer(), false, &inputs, &i18n, NumberStyle::POINT);
        for segment in &view.segments {
            assert!(
                plain(&segment.spoken.value),
                "{locale} {:?}: {}",
                segment.id,
                segment.spoken.value
            );
        }
    }
}

#[test]
fn quiet_idle_shows_only_memory() {
    // With the default preset an idle window shows exactly `Mem: 412 MB`.
    let view =
        build_view(&developer(), false, &idle_inputs(), &translator("en"), NumberStyle::POINT);
    assert_eq!(view.segments.len(), 1);
    assert_eq!(shown(&view, StatusSegmentId::Mem).unwrap(), "Mem: 412 MB");
    assert_eq!(view.segments[0].click, Some(SegmentClick::Action(Action::ShowMemoryUsage)));
    assert_eq!(
        view.segments[0].spoken,
        SpokenSegment { name: "Memory".into(), value: "412 MB".into() }
    );
}

#[test]
fn always_shows_idle_text_and_active_hides() {
    // `show = "always"` keeps the slot with idle text or `n/a`; `active` hides without a value.
    let mut resolved = developer();
    for segment in &mut resolved.segments {
        if segment.id != StatusSegmentId::Link {
            segment.show = ShowMode::Always;
        }
    }
    let inputs = StatusLineInputs::default();
    let view = build_view(&resolved, false, &inputs, &translator("en"), NumberStyle::POINT);
    assert_eq!(shown(&view, StatusSegmentId::Mem).unwrap(), "Mem: n/a");
    assert_eq!(shown(&view, StatusSegmentId::Scroll).unwrap(), "Scroll: bottom");
    assert_eq!(shown(&view, StatusSegmentId::Mode).unwrap(), "normal");
    assert_eq!(shown(&view, StatusSegmentId::Health).unwrap(), "Health: normal");
    assert_eq!(shown(&view, StatusSegmentId::Sel).unwrap(), "Sel: none");
    let scroll =
        view.segments.iter().find(|segment| segment.id == StatusSegmentId::Scroll).unwrap();
    assert_eq!(scroll.click, None, "nothing to scroll back to");
    let active = build_view(&developer(), false, &inputs, &translator("en"), NumberStyle::POINT);
    let ids: Vec<StatusSegmentId> = active.segments.iter().map(|segment| segment.id).collect();
    assert_eq!(ids, [StatusSegmentId::Mem], "only `mem` is `always` in the developer preset");
}

#[test]
fn core_segments_format_as_name_value_and_chips() {
    // Values read `Name: Value`; states are chips without names.
    let inputs = StatusLineInputs {
        mode: ModeInput { copy: true, broadcast_receivers: Some(3), mouse_captured: true },
        scroll_lines: 12_345,
        health: HealthInput {
            config: Some(ReloadFailure { line: Some(12) }),
            render: RenderHealth::SoftwareGdi,
            gpu_recovering: true,
            ..Default::default()
        },
        mem: MemInput {
            bytes: Some(500 * MIB),
            previous_bytes: Some(400 * MIB),
            ..Default::default()
        },
        link: Some(LinkInput::from_url("https://github.com/D0n9X1n/SonicTerm/pull/1670")),
        sel: None,
    };
    let view = build_view(&developer(), false, &inputs, &translator("en"), NumberStyle::POINT);
    assert_eq!(shown(&view, StatusSegmentId::Mode).unwrap(), "⇶ 3COPYMOUSE");
    assert_eq!(shown(&view, StatusSegmentId::Scroll).unwrap(), "Scroll: ↑ 12,345");
    assert_eq!(
        shown(&view, StatusSegmentId::Health).unwrap(),
        "Config: ✗ line 12  Render: ⚠ software (GDI)  GPU: ⚠ recovering"
    );
    assert_eq!(shown(&view, StatusSegmentId::Mem).unwrap(), "Mem: 500 MB ↑");
    assert_eq!(
        shown(&view, StatusSegmentId::Link).unwrap(),
        "Link: github.com/D0n9X1n/SonicTerm/pull/1670"
    );
    let by_id =
        |id| view.segments.iter().find(|segment: &&StatusSegmentView| segment.id == id).unwrap();
    assert!(by_id(StatusSegmentId::Mode).protected, "broadcast chip is protected");
    assert!(by_id(StatusSegmentId::Health).protected, "health errors are protected");
    assert_eq!(
        by_id(StatusSegmentId::Health).click,
        Some(SegmentClick::Action(Action::EditConfigFile))
    );
    assert_eq!(
        by_id(StatusSegmentId::Scroll).click,
        Some(SegmentClick::Handler(SegmentHandler::ScrollToBottom))
    );
    assert_eq!(by_id(StatusSegmentId::Mem).spoken.value, "500 MB rising");
    assert_eq!(by_id(StatusSegmentId::Scroll).spoken.value, "12,345 lines up");
    let ordered: Vec<StatusSegmentId> = view.segments.iter().map(|segment| segment.id).collect();
    use StatusSegmentId as Seg;
    assert_eq!(ordered, [Seg::Mode, Seg::Scroll, Seg::Link, Seg::Health, Seg::Mem]);
}

#[test]
fn health_without_errors_is_not_protected_or_clickable() {
    // Software rendering alone is a caution: not protected, and the default click stays inert.
    let inputs = StatusLineInputs {
        health: HealthInput { render: RenderHealth::SoftwareForced, ..Default::default() },
        ..Default::default()
    };
    let view = build_view(&developer(), false, &inputs, &translator("en"), NumberStyle::POINT);
    let health = &view.segments[0];
    assert_eq!(text_of(&health.parts), "Render: ⚠ software (forced)");
    assert!(!health.protected);
    assert_eq!(health.click, None);
}

#[test]
fn memory_thresholds_and_trend_deadband() {
    // Amber at warn, red at critical, each with a symbol; an under-1% move shows no arrow.
    let resolved = developer();
    let mem_text = |bytes: u64, previous: u64| {
        let inputs = StatusLineInputs {
            mem: MemInput {
                bytes: Some(bytes),
                previous_bytes: Some(previous),
                ..Default::default()
            },
            ..Default::default()
        };
        let view = build_view(&resolved, false, &inputs, &translator("en"), NumberStyle::POINT);
        (shown(&view, StatusSegmentId::Mem).unwrap(), view.segments[0].parts.last().unwrap().tone)
    };
    assert_eq!(mem_text(1024 * MIB, 1024 * MIB), ("Mem: ⚠ 1.00 GB".into(), StatusTone::Warn));
    assert_eq!(mem_text(2048 * MIB, 2048 * MIB), ("Mem: ✗ 2.00 GB".into(), StatusTone::Error));
    assert_eq!(mem_text(400 * MIB, 404 * MIB).0, "Mem: 400 MB");
    assert_eq!(mem_text(400 * MIB, 405 * MIB).0, "Mem: 400 MB ↓");
    assert_eq!(mem_trend(None, 5), None);
    assert_eq!(mem_trend(Some(100), 101), Some(Trend::Up));
    assert_eq!(mem_trend(Some(100), 100), None);
}

#[test]
fn ascii_icons_change_symbols_only() {
    // `icons = "ascii"` swaps every symbol and keeps every word.
    let mut resolved = developer();
    resolved.icons = StatusIcons::Ascii;
    let inputs = StatusLineInputs {
        mode: ModeInput { broadcast_receivers: Some(2), ..Default::default() },
        scroll_lines: 5,
        health: HealthInput { keymap: Some(ReloadFailure { line: None }), ..Default::default() },
        mem: MemInput {
            bytes: Some(300 * MIB),
            previous_bytes: Some(200 * MIB),
            ..Default::default()
        },
        link: None,
        sel: Some(SelInput::CopyFailed),
    };
    let view = build_view(&resolved, false, &inputs, &translator("en"), NumberStyle::POINT);
    assert_eq!(shown(&view, StatusSegmentId::Sel).unwrap(), "Sel: x copy failed");
    assert_eq!(shown(&view, StatusSegmentId::Mode).unwrap(), ">> 2");
    assert_eq!(shown(&view, StatusSegmentId::Scroll).unwrap(), "Scroll: ^ 5");
    assert_eq!(shown(&view, StatusSegmentId::Health).unwrap(), "Keymap: x error");
    assert_eq!(shown(&view, StatusSegmentId::Mem).unwrap(), "Mem: 300 MB ^");
    for segment in &view.segments {
        assert!(text_of(&segment.parts).is_ascii(), "{:?}", segment.id);
    }
}

#[test]
fn link_forms_cover_mismatch_paths_and_failures() {
    // Each detection outcome reads as one link value; the mismatch warns with both hosts.
    let format = |link: LinkInput| {
        let inputs = StatusLineInputs { link: Some(link), ..Default::default() };
        let view = build_view(&developer(), false, &inputs, &translator("en"), NumberStyle::POINT);
        shown(&view, StatusSegmentId::Link).unwrap()
    };
    assert_eq!(
        format(LinkInput::Mismatch {
            target_host: "evil.example".into(),
            label_host: "bank.example".into()
        }),
        "Link: ⚠ evil.example ≠ bank.example"
    );
    assert_eq!(
        format(LinkInput::Path { path: "~/proj/src".into(), action: PathAction::Reveal }),
        "Path: ~/proj/src · reveal"
    );
    assert_eq!(format(LinkInput::Checking { path: "~/a".into() }), "Path: ~/a · checking");
    assert_eq!(format(LinkInput::NotFound { path: "~/gone".into() }), "Path: ✗ ~/gone · not found");
    assert_eq!(
        format(LinkInput::Blocked {
            path: "/x".into(),
            reason: "no shell-reported directory".into()
        }),
        "Path: ⚠ /x · blocked: no shell-reported directory"
    );
    assert_eq!(
        LinkInput::from_url("mailto:me"),
        LinkInput::Url { host: "mailto:me".into(), rest: String::new() }
    );
    assert_eq!(
        LinkInput::from_url("https://host\u{202E}.example/a\u{7}b"),
        LinkInput::Url { host: "host.example".into(), rest: "/ab".into() }
    );
}

#[test]
fn hide_sensitive_removes_the_link_and_its_bounds() {
    // The link is whole-sensitive: hidden, it has no text, slot or click.
    let inputs = StatusLineInputs {
        link: Some(LinkInput::from_url("https://a.example/b")),
        ..idle_inputs()
    };
    let shown_view =
        build_view(&developer(), false, &inputs, &translator("en"), NumberStyle::POINT);
    assert!(shown(&shown_view, StatusSegmentId::Link).is_some());
    let hidden = build_view(&developer(), true, &inputs, &translator("en"), NumberStyle::POINT);
    assert!(shown(&hidden, StatusSegmentId::Link).is_none());
    assert_eq!(hidden.segments.len(), 1);
}

#[test]
fn click_none_and_explicit_actions_override_the_default() {
    // `click = "none"` keeps the segment but makes it inert; an action replaces the default handler.
    let mut resolved = developer();
    let set_click = |resolved: &mut ResolvedStatusLine, id, click| {
        let segment: &mut ResolvedSegment =
            resolved.segments.iter_mut().find(|segment| segment.id == id).unwrap();
        segment.click = click;
    };
    set_click(&mut resolved, StatusSegmentId::Mem, ClickBinding::None);
    set_click(&mut resolved, StatusSegmentId::Scroll, ClickBinding::Action(Action::TogglePaneZoom));
    let inputs = StatusLineInputs { scroll_lines: 3, ..idle_inputs() };
    let view = build_view(&resolved, false, &inputs, &translator("en"), NumberStyle::POINT);
    let by_id =
        |id| view.segments.iter().find(|segment: &&StatusSegmentView| segment.id == id).unwrap();
    assert_eq!(by_id(StatusSegmentId::Mem).click, None);
    assert_eq!(
        by_id(StatusSegmentId::Scroll).click,
        Some(SegmentClick::Action(Action::TogglePaneZoom))
    );
}

#[test]
fn pixel_hash_tracks_pixels_not_spoken_text_or_clicks() {
    // Same inputs hash the same; a color-only change differs; spoken or click changes do not.
    let i18n = translator("en");
    let base = build_view(&developer(), false, &idle_inputs(), &i18n, NumberStyle::POINT);
    assert_eq!(
        base.pixel_hash,
        build_view(&developer(), false, &idle_inputs(), &i18n, NumberStyle::POINT).pixel_hash
    );
    let mut recolored = base.segments.clone();
    recolored[0].parts[1].tone = StatusTone::Warn;
    assert_ne!(StatusLineView::new(recolored).pixel_hash, base.pixel_hash);
    let mut respoken = base.segments.clone();
    respoken[0].spoken.value = "other".into();
    respoken[0].click = None;
    assert_eq!(StatusLineView::new(respoken).pixel_hash, base.pixel_hash);
}

/// A view of named single-part segments for fitting tests.
fn fit_view(segments: &[(StatusSegmentId, StatusSide, u16, bool, &str)]) -> StatusLineView {
    StatusLineView::new(
        segments
            .iter()
            .map(|(id, side, priority, protected, text)| StatusSegmentView {
                id: *id,
                side: *side,
                priority: *priority,
                protected: *protected,
                parts: vec![StatusPart::new(*text, StatusTone::Value)],
                compact: None,
                click: None,
                spoken: SpokenSegment::default(),
            })
            .collect(),
    )
}

/// Ids of the presented segments.
fn presented(view: &StatusLineView, placed: &[PlacedSegment]) -> Vec<StatusSegmentId> {
    placed.iter().map(|segment| view.segments[segment.index].id).collect()
}

#[test]
fn fitting_drops_whole_segments_by_priority_down_to_empty() {
    // Narrowing drops lowest priority first, protected last, and never overflows.
    use StatusSegmentId as Seg;
    let view = fit_view(&[
        (Seg::Mode, StatusSide::Left, 800, false, "COPY"),
        (Seg::Scroll, StatusSide::Left, 400, false, "Scroll: ↑ 3"),
        (Seg::Health, StatusSide::Right, 950, true, "Config: ✗ line 1"),
        (Seg::Mem, StatusSide::Right, 900, false, "Mem: 1 MB"),
    ]);
    let full = 4.0 + 1.0 + 11.0 + 1.0 + 16.0 + 1.0 + 9.0;
    assert_eq!(presented(&view, &fit(&view, &tight(full), columns)).len(), 4);
    assert_eq!(
        presented(&view, &fit(&view, &tight(full - 1.0), columns)),
        [Seg::Mode, Seg::Health, Seg::Mem]
    );
    assert_eq!(
        presented(&view, &fit(&view, &tight(16.0 + 1.0 + 9.0), columns)),
        [Seg::Health, Seg::Mem]
    );
    assert_eq!(
        presented(&view, &fit(&view, &tight(16.0), columns)),
        [Seg::Health],
        "protected outlives priority 900"
    );
    assert!(fit(&view, &tight(3.0), columns).is_empty());
    for width in 0..60 {
        let placed = fit(&view, &tight(width as f32), columns);
        for segment in &placed {
            assert!(
                segment.left >= 0.0 && segment.left + segment.width <= width as f32 + 0.001,
                "width {width}"
            );
        }
    }
}

#[test]
fn equal_priorities_drop_the_segment_nearest_the_center_first() {
    // Ties go to the segment nearest the center: the center list, then the inner side segments.
    use StatusSegmentId as Seg;
    let view = fit_view(&[
        (Seg::Mode, StatusSide::Left, 500, false, "aaaa"),
        (Seg::Scroll, StatusSide::Left, 500, false, "bbbb"),
        (Seg::Link, StatusSide::Center, 500, false, "cccc"),
        (Seg::Mem, StatusSide::Right, 500, false, "dddd"),
    ]);
    assert_eq!(
        presented(&view, &fit(&view, &tight(14.0), columns)),
        [Seg::Mode, Seg::Scroll, Seg::Mem]
    );
    assert_eq!(presented(&view, &fit(&view, &tight(9.0), columns)), [Seg::Mode, Seg::Mem]);
}

#[test]
fn compact_forms_and_ellipsis_precede_dropping() {
    // A compact form is used before any drop; a text value shortens in the middle before it goes.
    use StatusSegmentId as Seg;
    let mut view = fit_view(&[(Seg::Mem, StatusSide::Right, 900, false, "Mem: 1 MB ↑")]);
    view.segments[0].compact = Some(vec![StatusPart::new("Mem: 1 MB", StatusTone::Value)]);
    let placed = fit(&view, &tight(9.0), columns);
    assert_eq!(placed[0].parts[0].text, "Mem: 1 MB");
    let mut link = fit_view(&[(Seg::Link, StatusSide::Center, 600, true, "Link: ")]);
    link.segments[0].parts.push(StatusPart::new("github.com", StatusTone::Value));
    link.segments[0]
        .parts
        .push(StatusPart::new("/D0n9X1n/SonicTerm/pull/1670", StatusTone::Value).ellipsized());
    let placed = fit(&link, &tight(30.0), columns);
    let text: String = placed[0].parts.iter().map(|part| part.text.as_str()).collect();
    assert!(text.starts_with("Link: github.com/") && text.contains('…'), "{text}");
    assert!(placed[0].width <= 30.0);
    assert!(fit(&link, &tight(18.0), columns).is_empty(), "the host is never cut");
}

#[test]
fn center_is_centered_and_shifts_between_the_sides() {
    // Center content sits in the middle until a side group pushes it aside.
    use StatusSegmentId as Seg;
    let view = fit_view(&[(Seg::Link, StatusSide::Center, 600, false, "xxxx")]);
    assert_eq!(fit(&view, &tight(20.0), columns)[0].left, 8.0);
    let crowded = fit_view(&[
        (Seg::Mode, StatusSide::Left, 800, false, "aaaaaaaaaa"),
        (Seg::Link, StatusSide::Center, 600, false, "xxxx"),
    ]);
    let placed = fit(&crowded, &tight(20.0), columns);
    assert_eq!(placed[1].left, 11.0);
}

#[test]
fn chips_take_padding_and_gaps() {
    // Chip parts include their background padding, with a gap between adjacent chips.
    let mut view = fit_view(&[(StatusSegmentId::Mode, StatusSide::Left, 800, false, "COPY")]);
    view.segments[0].parts[0].tone = StatusTone::Chip;
    view.segments[0].parts.push(StatusPart::new("MOUSE", StatusTone::Chip));
    let metrics =
        FitMetrics { width: 100.0, edge_pad: 2.0, segment_gap: 3.0, chip_pad: 1.0, chip_gap: 4.0 };
    let placed = fit(&view, &metrics, columns);
    assert_eq!(
        placed[0].parts[0],
        PlacedPart { text: "COPY".into(), tone: StatusTone::Chip, left: 2.0, width: 6.0 }
    );
    assert_eq!(placed[0].parts[1].left, 12.0);
    assert_eq!(placed[0].width, 17.0);
}

#[test]
fn byte_formatting_uses_three_significant_figures() {
    // Binary units, three significant figures, rollover at 1024 and the locale's decimal separator.
    let point = NumberStyle::POINT;
    assert_eq!(format_bytes(512, point), "512 B");
    assert_eq!(format_bytes(412 * MIB, point), "412 MB");
    assert_eq!(format_bytes(1023 * MIB, point), "1023 MB");
    assert_eq!(format_bytes(1024 * MIB - 1024 * 300, point), "1.00 GB");
    assert_eq!(format_bytes(1024 * MIB, point), "1.00 GB");
    assert_eq!(format_bytes(1045 * MIB, point), "1.02 GB");
    assert_eq!(format_bytes(15 * MIB + MIB / 2, point), "15.5 MB");
    assert_eq!(format_bytes(1045 * MIB, NumberStyle::for_locale("de-DE")), "1,02 GB");
}

#[test]
fn counts_group_per_locale_and_durations_never_localize() {
    // Digit grouping follows the locale; duration suffixes are fixed in every locale.
    assert_eq!(format_count(12_345, NumberStyle::for_locale("en")), "12,345");
    assert_eq!(format_count(12_345, NumberStyle::for_locale("zh-CN")), "12,345");
    assert_eq!(format_count(12_345, NumberStyle::for_locale("de")), "12.345");
    assert_eq!(format_count(12_345, NumberStyle::for_locale("fr-FR")), "12\u{202F}345");
    assert_eq!(format_count(999, NumberStyle::POINT), "999");
    assert_eq!(format_count(1_000_000, NumberStyle::POINT), "1,000,000");
    assert_eq!(
        [format_duration(42), format_duration(59), format_duration(60)],
        ["42s", "59s", "1m00s"]
    );
    assert_eq!(
        [format_duration(134), format_duration(3600), format_duration(3900)],
        ["2m14s", "1h00m", "1h05m"]
    );
}

#[test]
fn sanitizer_removes_controls_and_keeps_text() {
    // Controls, bidi and format characters go; ZWJ emoji, CJK and combining marks stay; caps hold.
    let limits = SanitizeLimits { max_bytes: 1024, max_columns: 200, collapse_whitespace: false };
    let hostile =
        "a\u{7}b\u{9B}c\u{61C}d\u{200E}e\u{202E}f\u{2066}g\u{2028}h\u{2029}i\u{FEFF}j\u{AD}k";
    assert_eq!(sanitize_display_text(hostile, SanitizePolicy::Ordinary, limits), "abcdefghijk");
    let family = "👨\u{200D}👩\u{200D}👧";
    assert_eq!(sanitize_display_text(family, SanitizePolicy::Ordinary, limits), family);
    assert_eq!(
        sanitize_display_text("漢字 e\u{301}", SanitizePolicy::Ordinary, limits),
        "漢字 e\u{301}"
    );
    assert_eq!(
        sanitize_display_text("a│b|c⟨d⟩e<f>g", SanitizePolicy::Ordinary, limits),
        "a│b|c⟨d⟩e<f>g"
    );
    assert_eq!(
        sanitize_display_text("a│b|c⟨d⟩e<f>g", SanitizePolicy::Program, limits),
        "a b c d e f g"
    );
    let big = "x".repeat(16 * 1024);
    let capped = sanitize_display_text(
        &big,
        SanitizePolicy::Ordinary,
        SanitizeLimits { max_bytes: 4096, max_columns: 10_000, collapse_whitespace: false },
    );
    assert_eq!(capped.len(), 4096);
    let collapsed = SanitizeLimits { collapse_whitespace: true, ..limits };
    assert_eq!(sanitize_display_text("  a \t\n b  ", SanitizePolicy::Ordinary, collapsed), "a b");
    let wide = sanitize_display_text(
        "漢字漢字漢字漢字",
        SanitizePolicy::Ordinary,
        SanitizeLimits { max_columns: 7, ..limits },
    );
    assert_eq!(wide, "漢…漢字");
    assert!(wide.width() <= 7);
    let byte_cut = sanitize_display_text(
        "ééé",
        SanitizePolicy::Ordinary,
        SanitizeLimits { max_bytes: 5, ..limits },
    );
    assert_eq!(byte_cut, "éé");
}

#[test]
fn width_ellipsis_keeps_grapheme_boundaries() {
    // Middle ellipsis never splits a grapheme and stops at the minimum kept length.
    let mut measure = columns;
    assert_eq!(ellipsize_middle_to_width("abcdefghij", 7.0, &mut measure), "abc…hij");
    assert_eq!(ellipsize_middle_to_width("abcdefghij", 1.0, &mut measure), "ab…ij");
    assert_eq!(ellipsize_middle_to_width("short", 1.0, &mut measure), "short");
    let combined = "e\u{301}".repeat(10);
    let cut = ellipsize_middle_to_width(&combined, 7.0, &mut measure);
    assert!(cut.graphemes(true).all(|grapheme| grapheme == "e\u{301}" || grapheme == "…"), "{cut}");
}

/// The heuristic label check: a different host or any userinfo mismatches, ports and case are
/// ignored, a word is plain, and a label that was not read whole is never a match.
#[test]
fn link_label_check_follows_the_heuristic() {
    use LinkLabelCheck::{Matches, Mismatch, NotChecked, Plain};
    let mismatch = |host: &str| Mismatch { label_host: host.into() };
    let cases = [
        ("https://bank.example", true, "https://evil.example", mismatch("bank.example")),
        ("bank.example@evil.example", true, "https://evil.example/", mismatch("bank.example")),
        ("Login", true, "https://bank.example@evil.example/", mismatch("bank.example")),
        ("docs", true, "https://example.com/docs", Plain),
        ("v1.2", true, "https://example.com/", Plain),
        ("example.com/a", true, "https://example.com/a/", Matches),
        ("https://example.com:443", true, "https://example.com", Matches),
        ("HTTPS://EXAMPLE.com./x", true, "https://example.com", Matches),
        ("https://example.com", false, "https://example.com", NotChecked),
        ("bank.example", true, "mailto:me@bank.example", Plain),
    ];
    for (label, complete, target, expected) in cases {
        assert_eq!(check_link_label(label, complete, target), expected, "{label} -> {target}");
    }
}

/// A hovered OSC 8 link reads as a mismatch warning, an unchecked link, or the plain URL.
#[test]
fn hovered_links_show_mismatch_or_label_not_checked() {
    let shown = |link: LinkInput| {
        let inputs = StatusLineInputs { link: Some(link), ..Default::default() };
        let view = build_view(&developer(), false, &inputs, &translator("en"), NumberStyle::POINT);
        shown(&view, StatusSegmentId::Link).unwrap()
    };
    assert_eq!(
        shown(LinkInput::from_hover("https://evil.example/login", Some(("bank.example", true)))),
        "Link: ⚠ evil.example ≠ bank.example"
    );
    assert_eq!(
        shown(LinkInput::from_hover("https://example.com/a", Some(("https://exam", false)))),
        "Link: example.com/a · label not checked"
    );
    assert_eq!(
        shown(LinkInput::from_hover("https://example.com/a", Some(("Docs", true)))),
        "Link: example.com/a"
    );
    assert_eq!(shown(LinkInput::from_hover("https://example.com/a", None)), "Link: example.com/a");
}

/// Right-to-left path text: bidi controls are removed by the ordinary sanitizer, Arabic and Hebrew
/// letters and combining marks are kept, and the middle ellipsis cuts on grapheme boundaries.
#[test]
fn rtl_path_text_is_sanitized_and_ellipsized_by_grapheme() {
    let limits = SanitizeLimits { max_bytes: 4096, max_columns: 200, collapse_whitespace: false };
    let arabic = "~/مشروع/\u{202E}ملفات\u{202C}/تقرير\u{2067}.txt\u{2069}";
    let clean = sanitize_display_text(arabic, SanitizePolicy::Ordinary, limits);
    assert_eq!(clean, "~/مشروع/ملفات/تقرير.txt");
    assert!(!clean
        .chars()
        .any(|character| matches!(u32::from(character), 0x202A..=0x202E | 0x2066..=0x2069)));
    let hebrew = "/בית/\u{200F}מסמכים/דּוּחַ\u{200E}.md";
    assert_eq!(
        sanitize_display_text(hebrew, SanitizePolicy::Ordinary, limits),
        "/בית/מסמכים/דּוּחַ.md"
    );
    for text in [clean.as_str(), "/בית/מסמכים/דּוּחַ.md"] {
        let mut measure = columns;
        let cut = ellipsize_middle_to_width(text, 10.0, &mut measure);
        assert!(cut.contains('…') && cut.width() <= 10, "{cut}");
        let whole: Vec<&str> = text.graphemes(true).collect();
        assert!(
            cut.graphemes(true).all(|grapheme| grapheme == "…" || whole.contains(&grapheme)),
            "{cut}"
        );
        let column_cut = ellipsize_middle_columns(text, 9);
        assert!(
            column_cut.width() <= 9
                && column_cut
                    .graphemes(true)
                    .all(|grapheme| grapheme == "…" || whole.contains(&grapheme))
        );
    }
}

/// Each bucket keeps its maximum sample; only completed buckets are drawn; a span with no sample
/// stays blank; and the rollover is never at `wall_ms` itself.
#[test]
fn memory_history_buckets_by_wall_clock() {
    let bucket = SPARK_BUCKET_MS;
    let mut history = MemHistory::default();
    history.record(0, 100);
    history.record(5_000, 300);
    history.record(bucket, 200);
    assert_eq!(history.cells(bucket + 1), [None, None, None, None, None, None, None, Some(300)]);
    // A resume four buckets later: the slept buckets stay blank.
    history.record(5 * bucket, 150);
    let cells = history.cells(6 * bucket);
    assert_eq!(cells, [None, None, Some(300), Some(200), None, None, None, Some(150)]);
    assert_eq!(history.cells(6 * bucket + 14_999), cells, "unchanged until rollover");
    assert_ne!(history.cells(7 * bucket), cells, "shifted at rollover");
    assert_eq!(history.cells(20 * bucket), [None; SPARK_CELLS], "aged out");
    assert_eq!(MemHistory::next_rollover_ms(0), bucket);
    assert_eq!(MemHistory::next_rollover_ms(bucket - 1), 1);
    for _ in 0..30 {
        history.record(30 * bucket, 1);
    }
    assert!(history.buckets.len() <= SPARK_CELLS + 1, "bounded");
}

/// Cells scale between the visible minimum and maximum; blanks are spaces; ascii swaps symbols.
#[test]
fn sparkline_scales_and_leaves_gaps_blank() {
    let cells = [Some(100), None, Some(200), Some(300), None, None, None, Some(1100)];
    assert_eq!(sparkline(&cells, StatusIcons::Unicode).unwrap(), "▁ ▂▂   █");
    assert_eq!(sparkline(&cells, StatusIcons::Ascii).unwrap(), "  ..   #");
    assert_eq!(sparkline(&[Some(5); SPARK_CELLS], StatusIcons::Unicode).unwrap(), "▁▁▁▁▁▁▁▁");
    assert_eq!(sparkline(&[None; SPARK_CELLS], StatusIcons::Unicode), None);
}

/// The diagnostics preset draws the sparkline after the figure; the compact form drops it first;
/// it is never spoken.
#[test]
fn diagnostics_mem_shows_a_static_sparkline() {
    let resolved = StatusLineConfig {
        preset: sonicterm_cfg::status_line::StatusPreset::Diagnostics,
        ..Default::default()
    }
    .resolve()
    .unwrap();
    let mut history = [None; SPARK_CELLS];
    history[6] = Some(400 * MIB);
    history[7] = Some(412 * MIB);
    let inputs = StatusLineInputs {
        mem: MemInput { bytes: Some(412 * MIB), previous_bytes: Some(412 * MIB), history },
        ..Default::default()
    };
    let view = build_view(&resolved, false, &inputs, &translator("en"), NumberStyle::POINT);
    let mem = &view.segments[0];
    assert_eq!(text_of(&mem.parts), "Mem: 412 MB       ▁█");
    assert_eq!(text_of(mem.compact.as_ref().unwrap()), "Mem: 412 MB");
    assert_eq!(mem.spoken.value, "412 MB");
    let plain = build_view(&developer(), false, &inputs, &translator("en"), NumberStyle::POINT);
    assert_eq!(text_of(&plain.segments[0].parts), "Mem: 412 MB", "off outside diagnostics");
}

#[test]
fn sel_shows_lines_chars_capped_rows_and_copy_results() {
    // The selection reads `Sel: 3 lines`, one line its characters, past the bound its exact row
    // span with `count capped`, and after a copy the result; only a live selection copies on click.
    let i18n = translator("en");
    let sel_view = |sel: SelInput| {
        let inputs = StatusLineInputs { sel: Some(sel), ..idle_inputs() };
        let view = build_view(&developer(), false, &inputs, &i18n, NumberStyle::POINT);
        view.segments.into_iter().find(|segment| segment.id == StatusSegmentId::Sel).unwrap()
    };
    let cases = [
        (SelInput::Extent { lines: 3, chars: 142 }, "Sel: 3 lines", "3 lines selected", true),
        (
            SelInput::Extent { lines: 1, chars: 1_420 },
            "Sel: 1,420 chars",
            "1,420 characters selected",
            true,
        ),
        (SelInput::Extent { lines: 1, chars: 1 }, "Sel: 1 char", "1 character selected", true),
        (
            SelInput::Capped { rows: 100_000 },
            "Sel: 100,000 rows · count capped",
            "100,000 rows selected, count capped",
            true,
        ),
        (SelInput::Copied, "Sel: ✓ copied", "copied", false),
        (SelInput::CopyFailed, "Sel: ✗ copy failed", "copy failed", false),
    ];
    for (sel, text, spoken, copies) in cases {
        let segment = sel_view(sel);
        assert_eq!(text_of(&segment.parts), text, "{sel:?}");
        assert_eq!(segment.spoken.value, spoken, "{sel:?}");
        let expected = copies.then_some(SegmentClick::Action(Action::CopyToClipboard));
        assert_eq!(segment.click, expected, "{sel:?}");
    }
    let failed = sel_view(SelInput::CopyFailed);
    assert_eq!(failed.parts.last().unwrap().tone, StatusTone::Error);
    let capped = sel_view(SelInput::Capped { rows: 100_000 });
    assert!(capped.parts[2..].iter().all(|part| part.droppable), "the row span survives narrowing");
    // `sel` is not sensitive: it is only counts, so `hide_sensitive` keeps it.
    let inputs = StatusLineInputs { sel: Some(SelInput::Copied), ..idle_inputs() };
    let hidden = build_view(&developer(), true, &inputs, &i18n, NumberStyle::POINT);
    assert_eq!(shown(&hidden, StatusSegmentId::Sel).unwrap(), "Sel: ✓ copied");
}
