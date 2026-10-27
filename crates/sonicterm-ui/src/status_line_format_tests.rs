//! Behavior contracts for format status presentation.

use super::*;
use crate::i18n::test_translator as translator;
use crate::status_line::status_line_tests::*;
use sonicterm_cfg::status_line::{ResolvedSegment, StatusLineConfig};

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
