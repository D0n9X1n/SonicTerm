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
    FitMetrics { width, edge_pad: 0.0, segment_gap: 1.0, separator_width: 0.0 }
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
                // CJK commas and colons are punctuation a screen reader pauses on, not symbols.
                || "(),.:-'，、：".contains(character)
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
            paste_raw: true,
            ime: Some(ImeInput::DiscardedInCopyMode),
            history_trimmed: Some(1_280),
            font: Some(FontInput::from_sizes(13.5, 14.0)),
            size: Some((120, 40)),
            zoomed: true,
            alerts: AlertsInput {
                tabs: vec![
                    (2, crate::tabs::TabMarks { bell: true, ..Default::default() }),
                    (3, crate::tabs::TabMarks { finished_secs: Some(134), ..Default::default() }),
                ],
                other_windows: 1,
            },
            clip: Some(ClipInput {
                outcome: ClipOutcome::ReadRefused,
                attempts: 1_000,
                source: ClipSource::Tab(2),
            }),
            input: Some(InputState::Held { secs: 4, on_reply: true, queued_bytes: 12 << 20 }),
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
        paste_raw: false,
        ime: None,
        history_trimmed: None,
        font: None,
        size: None,
        zoomed: false,
        alerts: AlertsInput::default(),
        clip: None,
        input: None,
        glyphs: None,
        cpu_percent: None,
        frame: None,
        out_rate: None,
        path_check: None,
        tab_mem: None,
    };
    let view = build_view(&developer(), false, &inputs, &translator("en"), NumberStyle::POINT);
    assert_eq!(shown(&view, StatusSegmentId::Mode).unwrap(), "⇶ 3 COPY MOUSE");
    let mode = view.segments.iter().find(|segment| segment.id == StatusSegmentId::Mode).unwrap();
    assert_eq!(text_of(mode.compact.as_ref().unwrap()), "⇶ 3 COPY", "MOUSE drops with its space");
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
        paste_raw: false,
        ime: None,
        history_trimmed: None,
        font: None,
        size: None,
        zoomed: false,
        alerts: AlertsInput::default(),
        clip: None,
        input: None,
        glyphs: None,
        cpu_percent: None,
        frame: None,
        out_rate: None,
        path_check: None,
        tab_mem: None,
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
fn chips_measure_as_their_text() {
    // A chip has no background, so it takes exactly its text's width, like any value; the space
    // part between two chips is the only thing that separates them.
    let mut view = fit_view(&[(StatusSegmentId::Mode, StatusSide::Left, 800, false, "COPY")]);
    view.segments[0].parts[0].tone = StatusTone::Chip;
    view.segments[0].parts.push(StatusPart::new(" ", StatusTone::Name));
    view.segments[0].parts.push(StatusPart::new("MOUSE", StatusTone::Chip));
    let metrics =
        FitMetrics { width: 100.0, edge_pad: 2.0, segment_gap: 3.0, separator_width: 0.0 };
    let placed = fit(&view, &metrics, columns);
    assert_eq!(
        placed[0].parts[0],
        PlacedPart { text: "COPY".into(), tone: StatusTone::Chip, left: 2.0, width: 4.0 }
    );
    assert_eq!((placed[0].parts[2].left, placed[0].parts[2].width), (7.0, 5.0));
    assert_eq!(placed[0].width, 10.0);
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

#[test]
fn paste_and_ime_show_only_problems() {
    // `paste` warns only for a raw paste; `ime` names a dropped commit or a non-pane destination,
    // and neither has a click. `ime` is not in the developer preset, so it is listed here.
    let i18n = translator("en");
    let mut resolved = developer();
    resolved.segments.push(ResolvedSegment {
        id: StatusSegmentId::Ime,
        side: StatusSide::Left,
        show: ShowMode::Active,
        priority: 500,
        click: ClickBinding::Default,
    });
    let quiet = build_view(&resolved, false, &idle_inputs(), &i18n, NumberStyle::POINT);
    assert_eq!(shown(&quiet, StatusSegmentId::Paste), None);
    assert_eq!(shown(&quiet, StatusSegmentId::Ime), None);
    let cases = [
        (
            ImeInput::DiscardedInCopyMode,
            "IME: ⚠ discarded (copy mode)",
            "input discarded in copy mode",
        ),
        (ImeInput::ToSearch, "IME: → search", "composing into search"),
        (ImeInput::ToPalette, "IME: → palette", "composing into the command palette"),
    ];
    for (ime, text, spoken) in cases {
        let inputs = StatusLineInputs { paste_raw: true, ime: Some(ime), ..idle_inputs() };
        let view = build_view(&resolved, true, &inputs, &i18n, NumberStyle::POINT);
        let segment =
            view.segments.iter().find(|segment| segment.id == StatusSegmentId::Ime).unwrap();
        assert_eq!(
            (text_of(&segment.parts).as_str(), segment.spoken.value.as_str()),
            (text, spoken)
        );
        assert_eq!(segment.click, None);
        let paste =
            view.segments.iter().find(|segment| segment.id == StatusSegmentId::Paste).unwrap();
        assert_eq!(text_of(&paste.parts), "Paste: ⚠ raw");
        assert_eq!(paste.parts[1].tone, StatusTone::Warn);
        assert_eq!(paste.spoken.value, "paste is unbracketed and may run lines");
        assert_eq!(paste.click, None);
    }
}

#[test]
fn history_font_and_size_show_only_active_display() {
    // `history` warns after a budget trim and acknowledges on click; `font`, which the developer
    // preset lists, shows the live size as a percent of the configured one only when that is not
    // 100% and resets on click; opt-in `size` shows the grid and is inert.
    let i18n = translator("en");
    let mut resolved = developer();
    resolved.segments.push(ResolvedSegment {
        id: StatusSegmentId::Size,
        side: StatusSide::Right,
        show: ShowMode::Active,
        priority: StatusSegmentId::Size.meta().default_priority,
        click: ClickBinding::Default,
    });
    let unchanged =
        StatusLineInputs { font: Some(FontInput::from_sizes(14.0, 14.0)), ..idle_inputs() };
    let quiet = build_view(&resolved, false, &unchanged, &i18n, NumberStyle::POINT);
    for id in [StatusSegmentId::History, StatusSegmentId::Font, StatusSegmentId::Size] {
        assert_eq!(shown(&quiet, id), None, "{id:?}");
    }
    let by_id = |inputs: &StatusLineInputs, id| {
        let view = build_view(&resolved, false, inputs, &i18n, NumberStyle::POINT);
        view.segments.into_iter().find(|segment| segment.id == id).unwrap()
    };
    let busy = StatusLineInputs {
        history_trimmed: Some(1_280),
        font: Some(FontInput::from_sizes(16.0, 14.0)),
        size: Some((120, 40)),
        ..idle_inputs()
    };
    let history = by_id(&busy, StatusSegmentId::History);
    assert_eq!(text_of(&history.parts), "History: trimmed");
    assert_eq!(history.parts[1].tone, StatusTone::Warn);
    assert_eq!(history.spoken.value, "trimmed, 1,280 rows dropped by the memory budget");
    assert_eq!(history.click, Some(SegmentClick::Handler(SegmentHandler::AcknowledgeHistory)));
    let font = by_id(&busy, StatusSegmentId::Font);
    assert_eq!(text_of(&font.parts), "Scale: 114%");
    assert_eq!(font.spoken.name, "Font scale");
    assert_eq!(font.spoken.value, "114 percent");
    assert_eq!(font.click, Some(SegmentClick::Action(Action::ResetFontSize)));
    let size = by_id(&busy, StatusSegmentId::Size);
    assert_eq!((text_of(&size.parts).as_str(), size.click.clone()), ("Size: 120×40", None));
    assert_eq!(size.spoken.value, "120 columns by 40 rows");
    let smaller =
        StatusLineInputs { font: Some(FontInput::from_sizes(13.5, 14.0)), ..idle_inputs() };
    let font = by_id(&smaller, StatusSegmentId::Font);
    assert_eq!(text_of(&font.parts), "Scale: 96%");
    assert_eq!(font.spoken.value, "96 percent");
    // 161 tenths over 140 is the maintainer's 115%; a change that rounds to 100% stays hidden.
    let target =
        StatusLineInputs { font: Some(FontInput::from_sizes(16.1, 14.0)), ..idle_inputs() };
    assert_eq!(text_of(&by_id(&target, StatusSegmentId::Font).parts), "Scale: 115%");
    let rounded =
        StatusLineInputs { font: Some(FontInput::from_sizes(140.1, 140.0)), ..idle_inputs() };
    let rounded = build_view(&resolved, false, &rounded, &i18n, NumberStyle::POINT);
    assert_eq!(shown(&rounded, StatusSegmentId::Font), None, "100% is hidden");
    // `zoom` is opt-in: a chip with no name while a split pane is zoomed, toggling zoom on click.
    let mut zoom_table = developer();
    zoom_table.segments.push(ResolvedSegment {
        id: StatusSegmentId::Zoom,
        side: StatusSide::Left,
        show: ShowMode::Active,
        priority: 260,
        click: ClickBinding::Default,
    });
    let unzoomed = build_view(&zoom_table, false, &idle_inputs(), &i18n, NumberStyle::POINT);
    assert_eq!(shown(&unzoomed, StatusSegmentId::Zoom), None);
    let zoomed = StatusLineInputs { zoomed: true, ..idle_inputs() };
    let view = build_view(&zoom_table, false, &zoomed, &i18n, NumberStyle::POINT);
    let zoom =
        view.segments.into_iter().find(|segment| segment.id == StatusSegmentId::Zoom).unwrap();
    assert_eq!((text_of(&zoom.parts).as_str(), zoom.parts[0].tone), ("ZOOM", StatusTone::Chip));
    assert_eq!(zoom.spoken.value, "pane zoomed");
    assert_eq!(zoom.click, Some(SegmentClick::Action(Action::TogglePaneZoom)));
}

#[test]
fn alerts_lists_marked_tabs_and_names_each_source() {
    // `Alerts: #2 #3` in tab order, `+N` past four tabs, `+1 window` for marks elsewhere; the
    // spoken value names each source and never says a program is waiting. It is protected and
    // its click goes to the next marked tab.
    use crate::tabs::TabMarks;
    let i18n = translator("en");
    let bell = TabMarks { bell: true, ..Default::default() };
    let finished = TabMarks { finished_secs: Some(134), ..Default::default() };
    let alerts_view = |alerts: AlertsInput| {
        let inputs = StatusLineInputs { alerts, ..idle_inputs() };
        let view = build_view(&developer(), false, &inputs, &i18n, NumberStyle::POINT);
        view.segments.into_iter().find(|segment| segment.id == StatusSegmentId::Alerts)
    };
    assert!(alerts_view(AlertsInput::default()).is_none(), "nothing marked");
    let two = alerts_view(AlertsInput { tabs: vec![(2, bell), (3, finished)], other_windows: 0 })
        .unwrap();
    assert_eq!(text_of(&two.parts), "Alerts: #2 #3");
    assert_eq!(two.spoken.value, "tab 2: bell. tab 3: finished after 2m14s");
    assert!(two.protected);
    assert_eq!(two.click, Some(SegmentClick::Handler(SegmentHandler::NextAlert)));
    let many = AlertsInput {
        tabs: [2, 3, 5, 6, 7, 9].into_iter().map(|number| (number, bell)).collect(),
        other_windows: 1,
    };
    assert_eq!(text_of(&alerts_view(many).unwrap().parts), "Alerts: #2 #3 #5 #6 +2 +1 window");
    let elsewhere = alerts_view(AlertsInput { tabs: Vec::new(), other_windows: 2 }).unwrap();
    assert_eq!(text_of(&elsewhere.parts), "Alerts: +2 windows");
    assert_eq!(elsewhere.spoken.value, "2 other windows with alerts");
}

#[test]
fn clip_shows_each_outcome_with_its_source() {
    // Each outcome has its text and tone; `×N` only above one attempt; the source drops first and a
    // closed source cannot be clicked. No clipboard text is ever part of it.
    let i18n = translator("en");
    let clip_view = |outcome, attempts, source| {
        let inputs = StatusLineInputs {
            clip: Some(ClipInput { outcome, attempts, source }),
            ..idle_inputs()
        };
        let view = build_view(&developer(), false, &inputs, &i18n, NumberStyle::POINT);
        view.segments.into_iter().find(|segment| segment.id == StatusSegmentId::Clip).unwrap()
    };
    let tab = ClipSource::Tab(2);
    let cases = [
        (ClipOutcome::Written(2_150), 1, tab, "Clip: 2.10 KB by #2", StatusTone::Value),
        (ClipOutcome::Failed, 1, tab, "Clip: ✗ failed by #2", StatusTone::Error),
        (
            ClipOutcome::ReadRefused,
            1_000,
            tab,
            "Clip: ⚠ read refused ×1,000 by #2",
            StatusTone::Warn,
        ),
        (
            ClipOutcome::SelectionRefused('p'),
            1,
            ClipSource::OtherWindow,
            "Clip: refused (selection p) by another window",
            StatusTone::Name,
        ),
        (ClipOutcome::TooLarge, 1, tab, "Clip: refused (too large) by #2", StatusTone::Name),
        (
            ClipOutcome::Invalid,
            1,
            ClipSource::Closed,
            "Clip: refused (invalid) by a closed tab",
            StatusTone::Name,
        ),
    ];
    for (outcome, attempts, source, text, tone) in cases {
        let segment = clip_view(outcome, attempts, source);
        assert_eq!(text_of(&segment.parts), text, "{outcome:?}");
        assert_eq!(segment.parts[1].tone, tone, "{outcome:?}");
        assert!(segment.parts.last().unwrap().droppable, "the source drops first");
        let clickable = source != ClipSource::Closed;
        let expected = clickable.then_some(SegmentClick::Handler(SegmentHandler::ClipSource));
        assert_eq!(segment.click, expected, "{outcome:?}");
    }
    let read = clip_view(ClipOutcome::ReadRefused, 1_000, tab);
    assert_eq!(read.spoken.value, "clipboard read refused, 1,000 requests, from tab 2");
}

#[test]
fn input_shows_each_state_and_never_claims_a_hang() {
    // Each state reads `Input: …` with its tone; the parenthetical or queued figure drops first; the
    // segment has no click. `held` states only an age.
    let i18n = translator("en");
    let input_view = |state| {
        let inputs = StatusLineInputs { input: Some(state), ..idle_inputs() };
        let view = build_view(&developer(), false, &inputs, &i18n, NumberStyle::POINT);
        view.segments.into_iter().find(|segment| segment.id == StatusSegmentId::Input).unwrap()
    };
    let cases = [
        (InputState::NotSent(NotSentReason::Busy), "Input: ✗ not sent (busy)", StatusTone::Error),
        (
            InputState::NotSent(NotSentReason::Closed),
            "Input: ✗ not sent (closed)",
            StatusTone::Error,
        ),
        (
            InputState::NotSent(NotSentReason::TooLarge),
            "Input: ✗ not sent (too large)",
            StatusTone::Error,
        ),
        (InputState::WriteFailed(3), "Input: ✗ write failed (3 not sent)", StatusTone::Error),
        (
            InputState::PasteRefused { refused: 1, attempted: 1 },
            "Input: ⚠ paste refused",
            StatusTone::Warn,
        ),
        (
            InputState::PasteRefused { refused: 2, attempted: 5 },
            "Input: ⚠ paste refused (2/5)",
            StatusTone::Warn,
        ),
        (InputState::Blocked(1), "Input: ⊘ blocked (read-only)", StatusTone::Name),
        (InputState::Blocked(2), "Input: ⊘ blocked 2 panes (read-only)", StatusTone::Name),
        (InputState::KeyNotSent, "Input: ✗ key not sent", StatusTone::Error),
        (
            InputState::Held { secs: 4, on_reply: false, queued_bytes: 10 },
            "Input: ⚠ held 4s",
            StatusTone::Warn,
        ),
        (
            InputState::Held { secs: 4, on_reply: false, queued_bytes: 12 << 20 },
            "Input: ⚠ held 4s · 12.0 MB queued",
            StatusTone::Warn,
        ),
    ];
    for (state, text, tone) in cases {
        let segment = input_view(state);
        assert_eq!(text_of(&segment.parts), text, "{state:?}");
        assert_eq!(segment.parts[1].tone, tone, "{state:?}");
        assert_eq!(segment.click, None);
        if segment.parts.len() > 2 {
            assert!(segment.parts[2].droppable, "the detail drops first");
        }
    }
    let held = input_view(InputState::Held { secs: 4, on_reply: true, queued_bytes: 0 });
    assert_eq!(held.spoken.value, "held 4s, behind a terminal reply");
}

#[test]
fn glyphs_shows_a_proven_count_and_never_an_unproven_one() {
    // 0 hides; an exact count, a lower bound and `count capped` each read as such, dim with `⚠`;
    // the spoken value has no glyphs; a click lists the codepoints.
    let i18n = translator("en");
    let glyphs_view = |glyphs| {
        let inputs = StatusLineInputs { glyphs: Some(glyphs), ..idle_inputs() };
        let view = build_view(&developer(), false, &inputs, &i18n, NumberStyle::POINT);
        view.segments.into_iter().find(|segment| segment.id == StatusSegmentId::Glyphs)
    };
    assert!(glyphs_view(GlyphsInput::Exact(0)).is_none());
    let cases = [
        (GlyphsInput::Exact(3), "Glyphs: ⚠ 3 missing", "3 characters have no font"),
        (
            GlyphsInput::AtLeast(1000),
            "Glyphs: ⚠ ≥ 1,000 missing",
            "at least 1,000 characters have no font",
        ),
        (GlyphsInput::Capped, "Glyphs: ⚠ count capped", "count capped"),
    ];
    for (glyphs, text, spoken) in cases {
        let segment = glyphs_view(glyphs).unwrap();
        assert_eq!(text_of(&segment.parts), text);
        assert!(!text.contains("999+"));
        assert_eq!(segment.parts[1].tone, StatusTone::Name);
        assert_eq!(segment.spoken.value, spoken);
        assert_eq!(segment.click, Some(SegmentClick::Handler(SegmentHandler::ListGlyphs)));
    }
}

#[test]
fn hysteresis_appears_at_the_threshold_and_hides_only_after_held_low_samples() {
    // Appears at `min`; between 0.7× and 1× keeps showing; below 0.7× hides after
    // `hold_samples` consecutive samples, and a sample back above resets the count.
    let mut hysteresis = Hysteresis::default();
    assert!(!hysteresis.step(4, 5, 700, 2));
    assert!(hysteresis.step(5, 5, 700, 2));
    assert!(hysteresis.step(4, 5, 700, 2), "0.8× keeps it");
    assert!(hysteresis.step(3, 5, 700, 2), "first low sample");
    assert!(hysteresis.step(4, 5, 700, 2), "a sample above the hide level resets the run");
    assert!(hysteresis.step(1, 5, 700, 2));
    assert!(hysteresis.step(1, 5, 700, 2));
    assert!(!hysteresis.step(1, 5, 700, 2), "hidden after two held low samples");
    let mut immediate = Hysteresis::default();
    assert!(immediate.step(9, 5, 700, 0));
    assert!(!immediate.step(0, 5, 700, 0), "hold_samples 0 hides at once");
}

#[test]
fn cpu_percent_and_byte_rate_follow_their_scales() {
    // One busy core is 100% of a core and 25% of four; an empty interval is 0.
    let second = Duration::from_secs(1);
    assert_eq!(cpu_percent(Duration::ZERO, second, false, 4), 0);
    assert_eq!(cpu_percent(second, second, false, 4), 100);
    assert_eq!(cpu_percent(second * 2, second, false, 4), 200);
    assert_eq!(cpu_percent(second, second, true, 4), 25);
    assert_eq!(cpu_percent(second, Duration::ZERO, false, 4), 0);
    assert_eq!(byte_rate(1023 * 1024, second), 1023 * 1024);
    assert_eq!(format_bytes(byte_rate(1023 * 1024, second), NumberStyle::POINT), "1023 KB");
    assert_eq!(format_bytes(byte_rate(1_047_552 + 600, second), NumberStyle::POINT), "1.00 MB");
}

#[test]
fn diagnostics_segments_format_their_figures() {
    // Each diagnostics segment's text; `cpu` hides without a figure, `frame` without content
    // frames; `path_check` warns past its limit; `tab_mem` names a stale charge's age and never
    // uses `≥`.
    let i18n = translator("en");
    let config = StatusLineConfig {
        preset: sonicterm_cfg::status_line::StatusPreset::Diagnostics,
        ..Default::default()
    };
    let resolved = config.resolve().unwrap();
    let text = |inputs: StatusLineInputs, id| {
        let view = build_view(&resolved, false, &inputs, &i18n, NumberStyle::POINT);
        view.segments
            .into_iter()
            .find(|segment| segment.id == id)
            .map(|segment| text_of(&segment.parts))
    };
    let frame = |frames| FrameInput {
        frames,
        mean_frame_us: 4_120,
        fps: 58,
        presenter: PresenterKind::Gdi,
    };
    let tab = |bytes, stale_secs| StatusLineInputs {
        tab_mem: Some(TabMemInput { bytes, stale_secs }),
        ..idle_inputs()
    };
    let cases = [
        (
            StatusLineInputs { cpu_percent: Some(14), ..idle_inputs() },
            StatusSegmentId::Cpu,
            Some("CPU: 14%"),
        ),
        (StatusLineInputs { cpu_percent: None, ..idle_inputs() }, StatusSegmentId::Cpu, None),
        (
            StatusLineInputs { out_rate: Some(14 << 20), ..idle_inputs() },
            StatusSegmentId::Out,
            Some("Out: 14.0 MB/s"),
        ),
        (
            StatusLineInputs { frame: Some(frame(3)), ..idle_inputs() },
            StatusSegmentId::Frame,
            Some("Frame: 4.1 ms · GDI"),
        ),
        (StatusLineInputs { frame: Some(frame(0)), ..idle_inputs() }, StatusSegmentId::Frame, None),
        (
            StatusLineInputs {
                path_check: Some(PathCheckInput { secs: 3, warn: false, queued: 0 }),
                ..idle_inputs()
            },
            StatusSegmentId::PathCheck,
            Some("Path check: 3s"),
        ),
        (
            StatusLineInputs {
                path_check: Some(PathCheckInput { secs: 47, warn: true, queued: 0 }),
                ..idle_inputs()
            },
            StatusSegmentId::PathCheck,
            Some("Path check: ⚠ 47s"),
        ),
        (
            StatusLineInputs {
                path_check: Some(PathCheckInput { secs: 3, warn: false, queued: 2 }),
                ..idle_inputs()
            },
            StatusSegmentId::PathCheck,
            Some("Path check: 3s · 2 queued"),
        ),
        (tab(38 << 20, None), StatusSegmentId::TabMem, Some("Tab mem: 38.0 MB")),
        (tab(84 << 20, Some(40)), StatusSegmentId::TabMem, Some("Tab mem: 84.0 MB · 40s ago")),
    ];
    for (inputs, id, expected) in cases {
        let shown = text(inputs, id);
        assert_eq!(shown.as_deref(), expected, "{id:?}");
        assert!(!shown.unwrap_or_default().contains('≥'));
    }
}

#[test]
fn ellipsize_middle_bytes_keeps_both_ends_on_grapheme_boundaries() {
    // A notification part over its byte cap keeps its start and end around one ellipsis,
    // never splitting a grapheme, and never exceeds the cap.
    assert_eq!(ellipsize_middle_bytes("short", 10), "short");
    let cut = ellipsize_middle_bytes("abcdefghijklmnop", 9);
    assert!(cut.len() <= 9 && cut.starts_with("ab") && cut.ends_with('p'), "{cut}");
    let flags = "🇯🇵".repeat(10);
    let cut = ellipsize_middle_bytes(&flags, 20);
    assert!(cut.len() <= 20);
    assert!(cut.replace('…', "").chars().count().is_multiple_of(2), "no flag is split: {cut}");
    assert_eq!(ellipsize_middle_bytes("abcdef", 2), "", "no room for the ellipsis");
}

/// Committed hit boxes for `ids`, laid out left to right, 100 px apart and 80 px wide; `clickable`
/// is taken from the view, as the presented strip records it.
fn committed(view: &StatusLineView, ids: &[StatusSegmentId]) -> Vec<CommittedSegment> {
    ids.iter()
        .enumerate()
        .map(|(index, id)| CommittedSegment {
            id: *id,
            left_px: index as f32 * 100.0,
            width: 80.0,
            clickable: view
                .segments
                .iter()
                .any(|segment| segment.id == *id && segment.click.is_some()),
        })
        .collect()
}

/// The accessible strip in English for `committed` segments of `view`.
fn accessible(view: &StatusLineView, committed: &[CommittedSegment]) -> AccessibleStrip {
    accessible_strip(committed, view, false, &translator("en"))
}

#[test]
fn every_catalog_variant_is_listed_and_has_an_accessible_name_in_every_locale() {
    // A new segment id does not compile until this match lists it, and the listed ids are exactly
    // `ALL`, so every catalog segment in the build has a spoken name, enforced per locale.
    let listed = |id: StatusSegmentId| match id {
        StatusSegmentId::Alerts
        | StatusSegmentId::Health
        | StatusSegmentId::Mem
        | StatusSegmentId::Mode
        | StatusSegmentId::Input
        | StatusSegmentId::Clip
        | StatusSegmentId::PathCheck
        | StatusSegmentId::Link
        | StatusSegmentId::Paste
        | StatusSegmentId::Sel
        | StatusSegmentId::Ime
        | StatusSegmentId::Scroll
        | StatusSegmentId::Cpu
        | StatusSegmentId::Frame
        | StatusSegmentId::Out
        | StatusSegmentId::TabMem
        | StatusSegmentId::Glyphs
        | StatusSegmentId::History
        | StatusSegmentId::Font
        | StatusSegmentId::Zoom
        | StatusSegmentId::Size => true,
    };
    assert!(StatusSegmentId::ALL.iter().all(|id| listed(*id)));
    for locale in SHIPPED_LOCALES {
        let i18n = translator(locale);
        for key in
            ["status-accessible-strip", "status-accessible-warning", "status-accessible-error"]
        {
            assert_ne!(i18n.translate(key), key, "{locale} {key}");
        }
        for id in StatusSegmentId::ALL {
            let name = i18n.translate(id.meta().spoken_name_key);
            assert!(!name.trim().is_empty(), "{locale} {id:?}");
        }
    }
}

#[test]
fn names_are_plain_words_with_warning_cues_and_no_sparkline() {
    // A segment's name is its spoken name and value; a warning says so in words, and the
    // diagnostics sparkline never reaches a name.
    let i18n = translator("en");
    let mut resolved = StatusLineConfig {
        preset: sonicterm_cfg::status_line::StatusPreset::Diagnostics,
        ..Default::default()
    }
    .resolve()
    .unwrap();
    resolved.mem.sparkline = true;
    let mut history = [None; SPARK_CELLS];
    history[6] = Some(400 * MIB);
    history[7] = Some(412 * MIB);
    let inputs = StatusLineInputs {
        mem: MemInput { bytes: Some(412 * MIB), previous_bytes: Some(300 * MIB), history },
        health: HealthInput { render: RenderHealth::SoftwareForced, ..Default::default() },
        ..Default::default()
    };
    let view = build_view(&resolved, false, &inputs, &i18n, NumberStyle::POINT);
    assert!(
        shown(&view, StatusSegmentId::Mem).unwrap().contains('▇')
            || shown(&view, StatusSegmentId::Mem).unwrap().contains('█')
    );
    let strip =
        accessible(&view, &committed(&view, &[StatusSegmentId::Health, StatusSegmentId::Mem]));
    let name = |id| strip.segments.iter().find(|segment| segment.id == id).unwrap().name.clone();
    assert_eq!(name(StatusSegmentId::Mem), "Memory 412 MB rising");
    assert!(
        name(StatusSegmentId::Health).starts_with("Warning: Health"),
        "{}",
        name(StatusSegmentId::Health)
    );
    for segment in &strip.segments {
        assert!(
            !segment.name.chars().any(|character| "▁▂▃▄▅▆▇█↑↓⚠✗".contains(character)),
            "{}",
            segment.name
        );
    }
}

#[test]
fn sensitive_segments_are_absent_while_hidden_even_when_committed() {
    // `hide_sensitive` removes the link from the tree, not only from the screen, even before a
    // frame drawn without it is committed.
    let inputs = StatusLineInputs {
        link: Some(LinkInput::from_url("https://a.example/b")),
        ..idle_inputs()
    };
    let view = build_view(&developer(), false, &inputs, &translator("en"), NumberStyle::POINT);
    let layout = committed(&view, &[StatusSegmentId::Link, StatusSegmentId::Mem]);
    let shown_strip = accessible_strip(&layout, &view, false, &translator("en"));
    assert!(shown_strip.segments.iter().any(|segment| segment.id == StatusSegmentId::Link));
    let hidden = accessible_strip(&layout, &view, true, &translator("en"));
    assert!(hidden.segments.iter().all(|segment| segment.id != StatusSegmentId::Link));
    assert_eq!(hidden.segments.len(), 1);
}

#[test]
fn structure_follows_the_committed_layout_and_values_follow_the_inputs() {
    // Structure (which segments, their order and bounds) comes only from the committed layout,
    // and each id is the segment's own, whatever its position; values come from the current view.
    let i18n = translator("en");
    let inputs = StatusLineInputs { scroll_lines: 340, ..idle_inputs() };
    let view = build_view(&developer(), false, &inputs, &i18n, NumberStyle::POINT);
    let ids = |strip: &AccessibleStrip| {
        strip.segments.iter().map(|segment| segment.id).collect::<Vec<_>>()
    };
    let ordered =
        accessible(&view, &committed(&view, &[StatusSegmentId::Scroll, StatusSegmentId::Mem]));
    assert_eq!(ids(&ordered), [StatusSegmentId::Scroll, StatusSegmentId::Mem]);
    let mut reversed = committed(&view, &[StatusSegmentId::Mem, StatusSegmentId::Scroll]);
    reversed.reverse();
    let reordered = accessible(&view, &reversed);
    assert_eq!(ids(&reordered), [StatusSegmentId::Mem, StatusSegmentId::Scroll], "visual order");
    // A fitted strip that dropped `scroll` exposes only `mem`, with the same id.
    let fitted = accessible(&view, &committed(&view, &[StatusSegmentId::Mem]));
    assert_eq!(ids(&fitted), [StatusSegmentId::Mem]);
    // A segment with a value but never drawn is absent: structure is only the committed layout.
    let never_drawn = accessible(&view, &committed(&view, &[StatusSegmentId::Mem]));
    assert!(never_drawn.segments.iter().all(|segment| segment.id != StatusSegmentId::Scroll));
    // A drawn segment the current inputs no longer format keeps its node, named by its spoken
    // name alone, so structure never depends on the latest visual view.
    let quiet = build_view(&developer(), false, &idle_inputs(), &i18n, NumberStyle::POINT);
    let stale =
        accessible(&quiet, &committed(&view, &[StatusSegmentId::Scroll, StatusSegmentId::Mem]));
    assert_eq!(ids(&stale), [StatusSegmentId::Scroll, StatusSegmentId::Mem]);
    let scroll_name = i18n.translate(StatusSegmentId::Scroll.meta().spoken_name_key);
    assert_eq!(stale.segments[0].name, scroll_name);
    // The same layout with a new memory figure changes the name and the hash.
    let rising = StatusLineInputs {
        mem: MemInput {
            bytes: Some(500 * MIB),
            previous_bytes: Some(412 * MIB),
            ..Default::default()
        },
        ..inputs
    };
    let later = build_view(&developer(), false, &rising, &i18n, NumberStyle::POINT);
    let layout = committed(&view, &[StatusSegmentId::Scroll, StatusSegmentId::Mem]);
    let before = accessible(&view, &layout);
    let after = accessible(&later, &layout);
    assert_ne!(before.segments[1].name, after.segments[1].name);
    assert_ne!(before.hash, after.hash);
}

#[test]
fn the_hash_ignores_color_and_clickable_segments_are_buttons() {
    // A color-only change keeps the hash; only clickable segments are buttons, and every segment
    // carries its bounds.
    let i18n = translator("en");
    let inputs = StatusLineInputs { scroll_lines: 3, ..idle_inputs() };
    let view = build_view(&developer(), false, &inputs, &i18n, NumberStyle::POINT);
    let layout = committed(&view, &[StatusSegmentId::Scroll, StatusSegmentId::Mem]);
    let base = accessible(&view, &layout);
    let mut recolored = view.clone();
    for segment in &mut recolored.segments {
        for part in &mut segment.parts {
            part.tone = StatusTone::Name;
        }
    }
    let recolored = StatusLineView::new(recolored.segments);
    assert_ne!(recolored.pixel_hash, view.pixel_hash);
    assert_eq!(accessible(&recolored, &layout).hash, base.hash);
    let mut inert = layout.clone();
    inert[0].clickable = false;
    let inert_strip = accessible(&view, &inert);
    assert_ne!(inert_strip.hash, base.hash, "an action change is announced");
    assert_eq!(base.segments[0].role, AccessibleRole::Button);
    assert_eq!(inert_strip.segments[0].role, AccessibleRole::Label);
    for segment in &inert_strip.segments {
        assert!(segment.width > 0.0, "{:?} has bounds", segment.id);
    }
    assert_eq!((inert_strip.segments[1].left_px, inert_strip.segments[1].width), (100.0, 80.0));
}

/// The percent comes from the real sizes, not sizes rounded to tenths first: 13.34 pt zoomed by
/// one point to 14.34 pt is 107.5%, shown as 107%, where tenths (143 / 133) would give 108%.
#[test]
fn font_scale_uses_the_unrounded_sizes() {
    let font = FontInput::from_sizes(14.34, 13.34);
    let inputs = StatusLineInputs { font: Some(font), ..idle_inputs() };
    let view = build_view(&developer(), false, &inputs, &translator("en"), NumberStyle::POINT);
    assert_eq!(shown(&view, StatusSegmentId::Font).unwrap(), "Scale: 107%");
    assert_eq!(FontInput::from_sizes(13.0, 0.0).scale_percent, None, "no scale of a zero size");
}

/// Two segments on each side and one in the center, with a 2-column gap and a 1-column `|`.
fn separated_view() -> (StatusLineView, FitMetrics) {
    use StatusSegmentId as Seg;
    let view = fit_view(&[
        (Seg::Mode, StatusSide::Left, 800, false, "aa"),
        (Seg::Scroll, StatusSide::Left, 400, false, "bb"),
        (Seg::Link, StatusSide::Center, 600, false, "cc"),
        (Seg::Health, StatusSide::Right, 950, false, "dd"),
        (Seg::Mem, StatusSide::Right, 900, false, "ee"),
    ]);
    (view, FitMetrics { width: 40.0, edge_pad: 0.0, segment_gap: 2.0, separator_width: 1.0 })
}

/// Each separator is a `|` in the name tone that sits between two presented segments of one group
/// and overlaps neither; returns the side of each, for counting.
fn separator_sides(
    view: &StatusLineView,
    placed: &[PlacedSegment],
    found: &[PlacedPart],
) -> Vec<StatusSide> {
    found
        .iter()
        .map(|separator| {
            assert_eq!((separator.text.as_str(), separator.tone), ("|", StatusTone::Name));
            let center = separator.left + separator.width / 2.0;
            let before =
                placed.iter().filter(|segment| segment.left + segment.width <= separator.left);
            let after =
                placed.iter().filter(|segment| segment.left >= separator.left + separator.width);
            let before = before.max_by(|first, second| first.left.total_cmp(&second.left)).unwrap();
            let after = after.min_by(|first, second| first.left.total_cmp(&second.left)).unwrap();
            let side = view.segments[before.index].side;
            assert_eq!(side, view.segments[after.index].side, "a separator never joins two groups");
            let gap_center = (before.left + before.width + after.left) / 2.0;
            assert!(
                (center - gap_center).abs() < 1e-4,
                "centered in its gap: {center} {gap_center}"
            );
            side
        })
        .collect()
}

/// A `|` sits between adjacent shown segments of a group, centered in their gap; none at a group
/// edge or between groups, and a group of one has none.
#[test]
fn separators_sit_only_between_adjacent_segments_of_a_group() {
    let (view, metrics) = separated_view();
    let placed = fit(&view, &metrics, columns);
    assert_eq!(placed.len(), 5);
    let found = separators(&view, &placed, &metrics);
    assert_eq!(separator_sides(&view, &placed, &found), [StatusSide::Left, StatusSide::Right]);
    // Left: `aa` 0..2, the 3-column gap holds `|` at 3, `bb` 5..7.
    assert_eq!((found[0].left, placed[1].left), (3.0, 5.0));
}

/// Fitting counts each separator, so dropping a segment drops its separator and leaves no doubled
/// or dangling `|`.
#[test]
fn dropping_a_segment_drops_its_separator() {
    let (view, mut metrics) = separated_view();
    // Everything needs 7 + 2 + 7 columns plus two 2-column gaps between groups: 20.
    metrics.width = 20.0;
    assert_eq!(fit(&view, &metrics, columns).len(), 5, "the separators fit exactly");
    metrics.width = 19.0;
    let placed = fit(&view, &metrics, columns);
    use StatusSegmentId as Seg;
    assert_eq!(presented(&view, &placed), [Seg::Mode, Seg::Link, Seg::Health, Seg::Mem]);
    let found = separators(&view, &placed, &metrics);
    assert_eq!(
        separator_sides(&view, &placed, &found),
        [StatusSide::Right],
        "left has one segment"
    );
}

/// A separator is no segment's hit box: a press on it finds nothing.
#[test]
fn a_press_on_a_separator_hits_no_segment() {
    let (view, metrics) = separated_view();
    let placed = fit(&view, &metrics, columns);
    for separator in separators(&view, &placed, &metrics) {
        let center = separator.left + separator.width / 2.0;
        let hit = placed
            .iter()
            .find(|segment| center >= segment.left && center < segment.left + segment.width);
        assert_eq!(hit, None, "{center}");
    }
}

/// A segment that fitting empties is not shown: `mode` with only `MOUSE` loses it to its compact
/// form, and then neither a `|` nor a separator gap is left before `Mem: 412 MB`.
#[test]
fn a_segment_fitting_empties_takes_no_separator() {
    use StatusSegmentId as Seg;
    let mut resolved = developer();
    resolved.segments = [Seg::Mode, Seg::Mem]
        .into_iter()
        .map(|id| ResolvedSegment {
            id,
            side: StatusSide::Left,
            show: if id == Seg::Mem { ShowMode::Always } else { ShowMode::Active },
            priority: id.meta().default_priority,
            click: ClickBinding::Default,
        })
        .collect();
    let inputs = StatusLineInputs {
        mode: ModeInput { mouse_captured: true, ..Default::default() },
        ..idle_inputs()
    };
    let view = build_view(&resolved, false, &inputs, &translator("en"), NumberStyle::POINT);
    let metrics =
        |width| FitMetrics { width, edge_pad: 0.0, segment_gap: 2.0, separator_width: 1.0 };
    let wide = fit(&view, &metrics(19.0), columns);
    assert_eq!(presented(&view, &wide), [Seg::Mode, Seg::Mem], "MOUSE | Mem: 412 MB fits in 19");
    assert_eq!(separators(&view, &wide, &metrics(19.0)).len(), 1);
    let narrow = fit(&view, &metrics(14.0), columns);
    assert_eq!(presented(&view, &narrow), [Seg::Mem], "the emptied mode is not shown");
    assert!(separators(&view, &narrow, &metrics(14.0)).is_empty(), "no dangling `|`");
    assert_eq!(narrow[0].left, 0.0, "no separator width reserved before Mem");
}
