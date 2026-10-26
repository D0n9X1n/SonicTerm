use super::*;
use crate::i18n::test_translator as translator;
use crate::i18n::SHIPPED_LOCALES;
use sonicterm_cfg::status_line::{ResolvedSegment, StatusLineConfig};

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

/// The default (developer) resolved table with the strip enabled.
pub(super) fn developer() -> ResolvedStatusLine {
    StatusLineConfig::default().resolve().unwrap()
}

/// Concatenated text of a segment's parts.
pub(super) fn text_of(parts: &[StatusPart]) -> String {
    parts.iter().map(|part| part.text.as_str()).collect()
}

/// The displayed text of segment `id` in `view`, if present.
pub(super) fn shown(view: &StatusLineView, id: StatusSegmentId) -> Option<String> {
    view.segments.iter().find(|segment| segment.id == id).map(|segment| text_of(&segment.parts))
}

/// A monospace measure: one unit per display column.
pub(super) fn columns(text: &str) -> f32 {
    text.width() as f32
}

/// Fit metrics with no padding or gaps, so widths are plain column counts.
pub(super) fn tight(width: f32) -> FitMetrics {
    FitMetrics { width, edge_pad: 0.0, segment_gap: 1.0, separator_width: 0.0 }
}

/// Inputs with a 412 MiB memory figure and nothing else to say.
pub(super) fn idle_inputs() -> StatusLineInputs {
    StatusLineInputs {
        mem: MemInput {
            bytes: Some(412 * MIB),
            previous_bytes: Some(412 * MIB),
            ..Default::default()
        },
        ..Default::default()
    }
}
