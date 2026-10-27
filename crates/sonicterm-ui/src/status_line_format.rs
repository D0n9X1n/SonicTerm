//! Formatting terminal, resource, and application state into status segments.

use super::*;

/// The memory segment: `Mem: 412 MB ↑`, amber or red at the thresholds.
pub(super) fn format_mem(
    inputs: &StatusLineInputs,
    env: &FormatEnv<'_>,
) -> Option<FormattedSegment> {
    let Some(bytes) = inputs.mem.bytes else {
        // When: `inputs.mem.bytes` is None, `always` shows `n/a` and `active` hides.
        let unavailable = env.i18n.translate("status-value-na");
        return (env.show == ShowMode::Always).then(|| {
            FormattedSegment::plain(
                vec![
                    name_part(env, "status-name-mem"),
                    StatusPart::new(unavailable.clone(), StatusTone::Name),
                ],
                unavailable,
            )
        });
    };
    let figure = format_bytes(bytes, env.numbers);
    let mib = bytes / MIB;
    let (tone, level_icon) = if mib >= env.mem.critical_mib {
        (StatusTone::Error, Some(symbol(env.icons, "✗ ", "x ")))
    } else if mib >= env.mem.warn_mib {
        // When: `mib` reaches `warn_mib` but not `critical_mib`, amber with a warning symbol.
        (StatusTone::Warn, Some(symbol(env.icons, "⚠ ", "! ")))
    } else {
        // When: `mib` is under `warn_mib`, the figure needs no colour.
        (StatusTone::Value, None)
    };
    let mut parts = vec![name_part(env, "status-name-mem")];
    parts.extend(level_icon.map(|icon| StatusPart::new(icon, tone)));
    parts.push(StatusPart::new(figure.clone(), tone));
    let compact = parts.clone();
    let mut spoken = figure;
    let trend = env.mem.show_trend.then(|| mem_trend(inputs.mem.previous_bytes, bytes)).flatten();
    if let Some(trend) = trend {
        // The figure moved by at least 1%, show the arrow and say the direction.
        let (arrow, word) = match trend {
            Trend::Up => (symbol(env.icons, "↑", "^"), "status-spoken-rising"),
            Trend::Down => (symbol(env.icons, "↓", "v"), "status-spoken-falling"),
        };
        parts.push(StatusPart::new(format!(" {arrow}"), tone).droppable());
        spoken = format!("{spoken} {}", env.i18n.translate(word));
    }
    let line = env.mem.sparkline.then(|| sparkline(&inputs.mem.history, env.icons)).flatten();
    if let Some(line) = line {
        // The sparkline is configured and a completed bucket has a sample: draw it static and dim.
        parts.push(StatusPart::new(format!(" {line}"), StatusTone::Name).droppable());
    }
    Some(FormattedSegment {
        compact: (compact != parts).then_some(compact),
        default_click: true,
        ..FormattedSegment::plain(parts, spoken)
    })
}

/// Reload-error components, in display order.
fn reload_components(health: &HealthInput) -> [(&'static str, Option<ReloadFailure>); 3] {
    [
        ("status-name-config", health.config),
        ("status-name-theme", health.theme),
        ("status-name-keymap", health.keymap),
    ]
}

/// The health segment: reload errors, software rendering, GPU recovery.
pub(super) fn format_health(
    inputs: &StatusLineInputs,
    env: &FormatEnv<'_>,
) -> Option<FormattedSegment> {
    let health = &inputs.health;
    let mut pairs: Vec<(Vec<StatusPart>, String, bool)> = Vec::new();
    for (key, failure) in reload_components(health) {
        let Some(failure) = failure else {
            // When: `failure` is None, this component's last reload succeeded and adds nothing.
            continue;
        };
        let value = match failure.line {
            Some(line) => {
                env.i18n.t_args("status-value-line", Some(&[("line", &line.to_string())]))
            }
            None => env.i18n.translate("status-value-error"),
        };
        let name = env.i18n.translate(key);
        pairs.push((
            vec![
                name_part(env, key),
                StatusPart::new(symbol(env.icons, "✗ ", "x "), StatusTone::Error),
                StatusPart::new(value.clone(), StatusTone::Error),
            ],
            format!("{name} {value}"),
            true,
        ));
    }
    let render_key = match health.render {
        RenderHealth::Hardware => None,
        RenderHealth::SoftwareGdi => Some("status-value-software-gdi"),
        RenderHealth::SoftwareAdapter => Some("status-value-software-adapter"),
        RenderHealth::SoftwareForced => Some("status-value-software-forced"),
    };
    if let Some(value_key) = render_key {
        // The window renders in software, name which kind.
        let value = env.i18n.translate(value_key);
        pairs.push((
            vec![
                name_part(env, "status-name-render"),
                StatusPart::new(symbol(env.icons, "⚠ ", "! "), StatusTone::Warn),
                StatusPart::new(value.clone(), StatusTone::Warn),
            ],
            format!("{} {value}", env.i18n.translate("status-name-render")),
            false,
        ));
    }
    if health.gpu_recovering {
        // This window waits on GPU recovery, say so.
        let value = env.i18n.translate("status-value-recovering");
        pairs.push((
            vec![
                name_part(env, "status-name-gpu"),
                StatusPart::new(symbol(env.icons, "⚠ ", "! "), StatusTone::Warn),
                StatusPart::new(value.clone(), StatusTone::Warn),
            ],
            format!("{} {value}", env.i18n.translate("status-name-gpu")),
            false,
        ));
    }
    if pairs.is_empty() {
        // When: `pairs` is empty, nothing is wrong; `always` shows `normal` and `active` hides.
        let normal = env.i18n.translate("status-value-normal");
        return (env.show == ShowMode::Always).then(|| {
            FormattedSegment::plain(
                vec![
                    StatusPart::new(
                        format!("{}: ", env.i18n.translate("status-spoken-health")),
                        StatusTone::Name,
                    ),
                    StatusPart::new(normal.clone(), StatusTone::Name),
                ],
                normal,
            )
        });
    }
    let has_error = pairs.iter().any(|(_, _, error)| *error);
    let spoken = pairs.iter().map(|(_, words, _)| words.as_str()).collect::<Vec<_>>().join(", ");
    let joined = |selected: Vec<&Vec<StatusPart>>| {
        let mut parts = Vec::new();
        for (index, pair) in selected.into_iter().enumerate() {
            if index > 0 {
                parts.push(StatusPart::new("  ", StatusTone::Name));
            }
            parts.extend(pair.iter().cloned());
        }
        parts
    };
    let parts = joined(pairs.iter().map(|(parts, _, _)| parts).collect());
    // The compact form keeps the errors, or the first warning when none.
    let compact_pairs: Vec<&Vec<StatusPart>> = if has_error {
        pairs.iter().filter(|(_, _, error)| *error).map(|(parts, _, _)| parts).collect()
    } else {
        // When: `has_error` is false, the compact form keeps the first warning.
        pairs.iter().take(1).map(|(parts, _, _)| parts).collect()
    };
    let compact = joined(compact_pairs);
    Some(FormattedSegment {
        compact: (compact != parts).then_some(compact),
        protected: has_error,
        default_click: has_error,
        ..FormattedSegment::plain(parts, spoken)
    })
}

/// The mode segment: `COPY`, broadcast `⇶ 3`, `MOUSE` chips; no names.
pub(super) fn format_mode(
    inputs: &StatusLineInputs,
    env: &FormatEnv<'_>,
) -> Option<FormattedSegment> {
    let mode = inputs.mode;
    let mut parts = Vec::new();
    let mut spoken = Vec::new();
    if let Some(count) = mode.broadcast_receivers {
        // Broadcast input is on, show it first with its receiver count.
        let count_text = format_count(count as u64, env.numbers);
        parts.push(StatusPart::new(
            format!("{} {count_text}", symbol(env.icons, "⇶", ">>")),
            StatusTone::ChipWarn,
        ));
        spoken.push(env.i18n.t_args("status-spoken-broadcast", Some(&[("count", &count_text)])));
    }
    if mode.copy {
        // A space separates chips now that no background sets them apart.
        if !parts.is_empty() {
            parts.push(StatusPart::new(" ", StatusTone::Name));
        }
        parts.push(StatusPart::new(env.i18n.translate("status-chip-copy"), StatusTone::Chip));
        spoken.push(env.i18n.translate("status-spoken-copy-mode"));
    }
    let compact = parts.clone();
    if mode.mouse_captured {
        // A program captured the mouse, explain why clicks go to it; its space drops with it.
        if !parts.is_empty() {
            parts.push(StatusPart::new(" ", StatusTone::Name).droppable());
        }
        parts.push(
            StatusPart::new(env.i18n.translate("status-chip-mouse"), StatusTone::Chip).droppable(),
        );
        spoken.push(env.i18n.translate("status-spoken-mouse-captured"));
    }
    if parts.is_empty() {
        // When: `parts` is empty, no mode is active; `always` shows `normal` and `active` hides.
        let normal = env.i18n.translate("status-value-normal");
        return (env.show == ShowMode::Always).then(|| {
            FormattedSegment::plain(vec![StatusPart::new(normal.clone(), StatusTone::Name)], normal)
        });
    }
    Some(FormattedSegment {
        compact: (compact != parts && !compact.is_empty()).then_some(compact),
        protected: mode.broadcast_receivers.is_some(),
        ..FormattedSegment::plain(parts, spoken.join(", "))
    })
}

/// The scroll segment: `Scroll: ↑ 340` while scrolled back.
pub(super) fn format_scroll(
    inputs: &StatusLineInputs,
    env: &FormatEnv<'_>,
) -> Option<FormattedSegment> {
    if inputs.scroll_lines == 0 {
        // When: `scroll_lines` is 0, the pane is at the bottom; `always` shows `bottom`.
        let bottom = env.i18n.translate("status-value-bottom");
        return (env.show == ShowMode::Always).then(|| {
            FormattedSegment::plain(
                vec![
                    name_part(env, "status-name-scroll"),
                    StatusPart::new(bottom.clone(), StatusTone::Name),
                ],
                bottom,
            )
        });
    }
    let count = format_count(inputs.scroll_lines, env.numbers);
    let parts = vec![
        name_part(env, "status-name-scroll"),
        StatusPart::new(format!("{} {count}", symbol(env.icons, "↑", "^")), StatusTone::Value),
    ];
    let spoken = env.i18n.t_args("status-spoken-lines-up", Some(&[("count", &count)]));
    Some(FormattedSegment { default_click: true, ..FormattedSegment::plain(parts, spoken) })
}

/// The selection segment: `Sel: 3 lines`, `Sel: 142 chars`, a capped row
/// span, or the result of the last requested copy.
pub(super) fn format_sel(
    inputs: &StatusLineInputs,
    env: &FormatEnv<'_>,
) -> Option<FormattedSegment> {
    let Some(sel) = inputs.sel else {
        // When: `inputs.sel` is None there is no selection; `always` shows `none`.
        let none = env.i18n.translate("status-value-none");
        return (env.show == ShowMode::Always).then(|| {
            FormattedSegment::plain(
                vec![
                    name_part(env, "status-name-sel"),
                    StatusPart::new(none.clone(), StatusTone::Name),
                ],
                none,
            )
        });
    };
    let count_args = |key: &str, count: u64| {
        env.i18n.t_args(key, Some(&[("count", &format_count(count, env.numbers))]))
    };
    let mut parts = vec![name_part(env, "status-name-sel")];
    let spoken = match sel {
        SelInput::Extent { lines, chars } => {
            let (value, spoken) = if lines > 1 {
                (
                    count_args("status-value-sel-lines", lines),
                    count_args("status-spoken-sel-lines", lines),
                )
            } else if chars == 1 {
                // When: chars is 1; the catalogs carry no plural selectors, so it has its own words.
                (
                    env.i18n.translate("status-value-sel-char"),
                    env.i18n.translate("status-spoken-sel-char"),
                )
            } else {
                // When: `lines` is 1 and `chars` is not 1, a single line shows its character count.
                (
                    count_args("status-value-sel-chars", chars),
                    count_args("status-spoken-sel-chars", chars),
                )
            };
            parts.push(StatusPart::new(value, StatusTone::Value));
            spoken
        }
        SelInput::Capped { rows } => {
            parts.push(StatusPart::new(
                count_args("status-value-sel-rows", rows),
                StatusTone::Value,
            ));
            parts.push(
                StatusPart::new(symbol(env.icons, " · ", " - "), StatusTone::Name).droppable(),
            );
            parts.push(
                StatusPart::new(env.i18n.translate("status-value-count-capped"), StatusTone::Name)
                    .droppable(),
            );
            count_args("status-spoken-sel-capped", rows)
        }
        SelInput::Copied => {
            let copied = env.i18n.translate("status-value-copied");
            parts.push(StatusPart::new(
                format!("{} {copied}", symbol(env.icons, "✓", "ok")),
                StatusTone::Value,
            ));
            copied
        }
        SelInput::CopyFailed => {
            let failed = env.i18n.translate("status-value-copy-failed");
            parts.push(StatusPart::new(
                format!("{} {failed}", symbol(env.icons, "✗", "x")),
                StatusTone::Error,
            ));
            failed
        }
    };
    // Only a live selection has something for the `default` click to copy.
    let default_click = matches!(sel, SelInput::Extent { .. } | SelInput::Capped { .. });
    Some(FormattedSegment { default_click, ..FormattedSegment::plain(parts, spoken) })
}

/// The paste segment: `Paste: ⚠ raw` while a paste would be unbracketed at a confirmed prompt.
pub(super) fn format_paste(
    inputs: &StatusLineInputs,
    env: &FormatEnv<'_>,
) -> Option<FormattedSegment> {
    if !inputs.paste_raw {
        // When: `paste_raw` is false, bracketed paste is on or the pane is not at a prompt.
        return None;
    }
    let raw = env.i18n.translate("status-value-raw");
    let parts = vec![
        name_part(env, "status-name-paste"),
        StatusPart::new(format!("{} {raw}", symbol(env.icons, "⚠", "!")), StatusTone::Warn),
    ];
    Some(FormattedSegment::plain(parts, env.i18n.translate("status-spoken-paste-raw")))
}

/// The input-method segment: a dropped commit, or a composition bound away from the pane.
pub(super) fn format_ime(
    inputs: &StatusLineInputs,
    env: &FormatEnv<'_>,
) -> Option<FormattedSegment> {
    let ime = inputs.ime?;
    let (text, tone, spoken_key) = match ime {
        ImeInput::DiscardedInCopyMode => (
            format!(
                "{} {}",
                symbol(env.icons, "⚠", "!"),
                env.i18n.translate("status-value-discarded-copy-mode")
            ),
            StatusTone::Warn,
            "status-spoken-ime-discarded",
        ),
        ImeInput::ToSearch => (
            format!(
                "{} {}",
                symbol(env.icons, "→", "->"),
                env.i18n.translate("status-value-search")
            ),
            StatusTone::Value,
            "status-spoken-ime-search",
        ),
        ImeInput::ToPalette => (
            format!(
                "{} {}",
                symbol(env.icons, "→", "->"),
                env.i18n.translate("status-value-palette")
            ),
            StatusTone::Value,
            "status-spoken-ime-palette",
        ),
    };
    let parts = vec![name_part(env, "status-name-ime"), StatusPart::new(text, tone)];
    Some(FormattedSegment::plain(parts, env.i18n.translate(spoken_key)))
}

/// The history segment: `History: trimmed` after the byte budget dropped rows from the focused
/// pane's scrollback, until the trim is acknowledged.
pub(super) fn format_history(
    inputs: &StatusLineInputs,
    env: &FormatEnv<'_>,
) -> Option<FormattedSegment> {
    let rows = inputs.history_trimmed?;
    let parts = vec![
        name_part(env, "status-name-history"),
        StatusPart::new(env.i18n.translate("status-value-trimmed"), StatusTone::Warn),
    ];
    let count = format_count(rows, env.numbers);
    let spoken = env.i18n.t_args("status-spoken-history-trimmed", Some(&[("count", &count)]));
    Some(FormattedSegment { default_click: true, ..FormattedSegment::plain(parts, spoken) })
}

/// The font segment: `Scale: 115%`, the live font size as a percent of the configured size,
/// shown only while that percent is not 100.
pub(super) fn format_font(
    inputs: &StatusLineInputs,
    env: &FormatEnv<'_>,
) -> Option<FormattedSegment> {
    let percent =
        inputs.font.and_then(|font| font.scale_percent).filter(|percent| *percent != 100)?;
    let value = format_count(u64::from(percent), env.numbers);
    let parts = vec![
        name_part(env, "status-name-font"),
        StatusPart::new(format!("{value}%"), StatusTone::Value),
    ];
    let spoken = env.i18n.t_args("status-value-font-scale", Some(&[("percent", &value)]));
    Some(FormattedSegment { default_click: true, ..FormattedSegment::plain(parts, spoken) })
}

/// The size segment: `Size: 120×40` for a while after the focused pane's grid was resized.
pub(super) fn format_size(
    inputs: &StatusLineInputs,
    env: &FormatEnv<'_>,
) -> Option<FormattedSegment> {
    let (cols, rows) = inputs.size?;
    let parts = vec![
        name_part(env, "status-name-size"),
        StatusPart::new(format!("{cols}{}{rows}", symbol(env.icons, "×", "x")), StatusTone::Value),
    ];
    let spoken = env.i18n.t_args(
        "status-spoken-size-grid",
        Some(&[("cols", &cols.to_string()), ("rows", &rows.to_string())]),
    );
    Some(FormattedSegment::plain(parts, spoken))
}

/// The alerts segment: `Alerts: #2 #3`, `#2 #3 #5 #6 +2`, and `+1 window` for marks elsewhere.
/// The spoken value names each source; it never claims a program is waiting for input.
pub(super) fn format_alerts(
    inputs: &StatusLineInputs,
    env: &FormatEnv<'_>,
) -> Option<FormattedSegment> {
    let alerts = &inputs.alerts;
    if alerts.tabs.is_empty() && alerts.other_windows == 0 {
        // When: alerts.tabs is empty and other_windows is 0, no tab anywhere is marked.
        return None;
    }
    let mut words: Vec<String> =
        alerts.tabs.iter().take(ALERT_TABS_SHOWN).map(|(number, _)| format!("#{number}")).collect();
    if alerts.tabs.len() > ALERT_TABS_SHOWN {
        words.push(format!("+{}", alerts.tabs.len() - ALERT_TABS_SHOWN));
    }
    let mut parts = vec![name_part(env, "status-name-alerts")];
    if !words.is_empty() {
        parts.push(StatusPart::new(words.join(" "), StatusTone::Value));
    }
    let count = format_count(alerts.other_windows as u64, env.numbers);
    if alerts.other_windows > 0 {
        let key = if alerts.other_windows == 1 {
            "status-value-other-window"
        } else {
            // More than one other window takes the plural form with its count.
            "status-value-other-windows"
        };
        let separator = if words.is_empty() { "" } else { " " };
        let text = format!("{separator}{}", env.i18n.t_args(key, Some(&[("count", &count)])));
        parts.push(StatusPart::new(text, StatusTone::Value).droppable());
    }
    let mut spoken: Vec<String> = alerts
        .tabs
        .iter()
        .map(|(number, marks)| {
            let mut sources = Vec::new();
            if marks.bell {
                sources.push(env.i18n.translate("status-spoken-alert-bell"));
            }
            if marks.notify {
                sources.push(env.i18n.translate("status-spoken-alert-notify"));
            }
            if let Some(secs) = marks.finished_secs {
                let duration = format_duration(secs);
                sources.push(
                    env.i18n
                        .t_args("status-spoken-alert-finished", Some(&[("duration", &duration)])),
                );
            }
            env.i18n.t_args(
                "status-spoken-alert-tab",
                Some(&[("tab", &number.to_string()), ("sources", &sources.join(", "))]),
            )
        })
        .collect();
    if alerts.other_windows > 0 {
        spoken.push(env.i18n.t_args("status-spoken-alert-windows", Some(&[("count", &count)])));
    }
    Some(FormattedSegment {
        protected: true,
        default_click: true,
        ..FormattedSegment::plain(parts, spoken.join(". "))
    })
}

/// The glyphs segment: distinct characters drawn as boxes because no font SonicTerm can find
/// covers them. Dim, with `⚠`; it never shows an unproven figure.
pub(super) fn format_glyphs(
    inputs: &StatusLineInputs,
    env: &FormatEnv<'_>,
) -> Option<FormattedSegment> {
    let glyphs = inputs.glyphs?;
    let count = |key: &str, count: u64| {
        env.i18n.t_args(key, Some(&[("count", &format_count(count, env.numbers))]))
    };
    let (value, spoken) = match glyphs {
        GlyphsInput::Exact(0) => {
            // When: glyphs is Exact(0), nothing is missing, so the segment hides.
            return None;
        }
        GlyphsInput::Exact(missing) => (
            count("status-value-glyphs-missing", missing),
            count("status-spoken-glyphs-missing", missing),
        ),
        GlyphsInput::AtLeast(missing) => (
            count("status-value-glyphs-at-least", missing),
            count("status-spoken-glyphs-at-least", missing),
        ),
        GlyphsInput::Capped => (
            env.i18n.translate("status-value-glyphs-capped"),
            env.i18n.translate("status-value-glyphs-capped"),
        ),
    };
    let parts = vec![
        name_part(env, "status-name-glyphs"),
        StatusPart::new(format!("{} {value}", symbol(env.icons, "⚠", "!")), StatusTone::Name),
    ];
    Some(FormattedSegment { default_click: true, ..FormattedSegment::plain(parts, spoken) })
}

/// The input segment: input SonicTerm refused or blocked, or an accepted write that has not
/// completed. It never claims the program is hung or waiting, and shows no input bytes.
pub(super) fn format_input(
    inputs: &StatusLineInputs,
    env: &FormatEnv<'_>,
) -> Option<FormattedSegment> {
    let state = inputs.input?;
    let cross = symbol(env.icons, "✗", "x");
    let warn = symbol(env.icons, "⚠", "!");
    let translate = |key: &str| env.i18n.translate(key);
    let count = |key: &str, count: u64| {
        env.i18n.t_args(key, Some(&[("count", &format_count(count, env.numbers))]))
    };
    // The outcome, its tone, an optional parenthetical dropped first, and the spoken value.
    let (outcome, tone, detail, spoken) = match state {
        InputState::NotSent(reason) => {
            let key = match reason {
                NotSentReason::Busy => "status-value-input-busy",
                NotSentReason::Closed => "status-value-input-closed",
                NotSentReason::TooLarge => "status-value-input-too-large",
            };
            let detail = translate(key);
            let spoken = format!("{}, {detail}", translate("status-value-input-not-sent"));
            (
                format!("{cross} {}", translate("status-value-input-not-sent")),
                StatusTone::Error,
                Some(detail),
                spoken,
            )
        }
        InputState::WriteFailed(lost) => {
            let detail = count("status-value-input-lost", u64::from(lost));
            let spoken = format!("{}, {detail}", translate("status-value-input-write-failed"));
            (
                format!("{cross} {}", translate("status-value-input-write-failed")),
                StatusTone::Error,
                Some(detail),
                spoken,
            )
        }
        InputState::PasteRefused { refused, attempted } => {
            let detail = (attempted > 1).then(|| format!("{refused}/{attempted}"));
            let spoken = translate("status-value-input-paste-refused");
            (format!("{warn} {spoken}"), StatusTone::Warn, detail, spoken)
        }
        InputState::Blocked(panes) => {
            let blocked = if panes > 1 {
                count("status-value-input-blocked-panes", panes as u64)
            } else {
                // When: panes is at most 1, the gesture's own pane is the one blocked.
                translate("status-value-input-blocked")
            };
            let detail = translate("status-value-input-read-only");
            let spoken = format!("{blocked}, {detail}");
            (
                format!("{} {blocked}", symbol(env.icons, "⊘", "-")),
                StatusTone::Name,
                Some(detail),
                spoken,
            )
        }
        InputState::KeyNotSent => {
            let spoken = translate("status-value-input-key-not-sent");
            (format!("{cross} {spoken}"), StatusTone::Error, None, spoken)
        }
        InputState::Held { secs, on_reply, queued_bytes } => {
            let age = format_duration(secs);
            let held = env.i18n.t_args("status-value-input-held", Some(&[("age", &age)]));
            let detail = (queued_bytes >= 1 << 20).then(|| {
                let size = format_bytes(queued_bytes, env.numbers);
                env.i18n.t_args("status-value-input-queued", Some(&[("size", &size)]))
            });
            let mut spoken = held.clone();
            if on_reply {
                spoken = format!("{spoken}, {}", translate("status-spoken-input-behind-reply"));
            }
            (format!("{warn} {held}"), StatusTone::Warn, detail, spoken)
        }
    };
    let mut parts = vec![name_part(env, "status-name-input"), StatusPart::new(outcome, tone)];
    if let Some(detail) = detail {
        let held = matches!(state, InputState::Held { .. });
        let text = if held {
            format!(" {} {detail}", symbol(env.icons, "·", "-"))
        } else {
            // When: held is false, the detail is a parenthetical.
            format!(" ({detail})")
        };
        parts.push(StatusPart::new(text, tone).droppable());
    }
    Some(FormattedSegment::plain(parts, spoken))
}

/// The clipboard segment: what SonicTerm did with the latest OSC 52 request and which tab sent it,
/// `Clip: 2.1 KB by #2` or `Clip: ⚠ read refused ×1000 by #2`. It shows no clipboard text and does
/// not claim the clipboard still holds what was written.
pub(super) fn format_clip(
    inputs: &StatusLineInputs,
    env: &FormatEnv<'_>,
) -> Option<FormattedSegment> {
    let clip = inputs.clip?;
    let (text, tone, spoken) = match clip.outcome {
        ClipOutcome::Written(bytes) => {
            let size = format_bytes(bytes, env.numbers);
            let spoken = env.i18n.t_args("status-spoken-clip-written", Some(&[("size", &size)]));
            (size, StatusTone::Value, spoken)
        }
        ClipOutcome::Failed => (
            format!(
                "{} {}",
                symbol(env.icons, "✗", "x"),
                env.i18n.translate("status-value-clip-failed")
            ),
            StatusTone::Error,
            env.i18n.translate("status-spoken-clip-failed"),
        ),
        ClipOutcome::ReadRefused => (
            format!(
                "{} {}",
                symbol(env.icons, "⚠", "!"),
                env.i18n.translate("status-value-clip-read-refused")
            ),
            StatusTone::Warn,
            env.i18n.translate("status-spoken-clip-read-refused"),
        ),
        ClipOutcome::SelectionRefused(selection) => {
            let selection = selection.to_string();
            let args = [("selection", selection.as_str())];
            (
                env.i18n.t_args("status-value-clip-selection-refused", Some(&args)),
                StatusTone::Name,
                env.i18n.t_args("status-spoken-clip-selection-refused", Some(&args)),
            )
        }
        ClipOutcome::TooLarge => (
            env.i18n.translate("status-value-clip-too-large"),
            StatusTone::Name,
            env.i18n.translate("status-spoken-clip-too-large"),
        ),
        ClipOutcome::Invalid => (
            env.i18n.translate("status-value-clip-invalid"),
            StatusTone::Name,
            env.i18n.translate("status-spoken-clip-invalid"),
        ),
    };
    let mut parts = vec![name_part(env, "status-name-clip"), StatusPart::new(text, tone)];
    let mut spoken = vec![spoken];
    if clip.attempts > 1 {
        let count = format_count(clip.attempts, env.numbers);
        parts.push(StatusPart::new(format!(" \u{00D7}{count}"), tone).droppable());
        spoken.push(env.i18n.t_args("status-spoken-clip-attempts", Some(&[("count", &count)])));
    }
    let (source_text, source_spoken) = match clip.source {
        ClipSource::Tab(number) => {
            let tab = number.to_string();
            (
                env.i18n.t_args("status-value-clip-by-tab", Some(&[("tab", &tab)])),
                env.i18n.t_args("status-spoken-clip-by-tab", Some(&[("tab", &tab)])),
            )
        }
        ClipSource::OtherWindow => (
            env.i18n.translate("status-value-clip-by-window"),
            env.i18n.translate("status-spoken-clip-by-window"),
        ),
        ClipSource::Closed => (
            env.i18n.translate("status-value-clip-by-closed"),
            env.i18n.translate("status-spoken-clip-by-closed"),
        ),
    };
    // The source goes first when narrowed, then the count; the outcome always stays.
    parts.push(StatusPart::new(format!(" {source_text}"), StatusTone::Name).droppable());
    spoken.push(source_spoken);
    // Only a tab that still exists can be switched to.
    let default_click = !matches!(clip.source, ClipSource::Closed);
    Some(FormattedSegment { default_click, ..FormattedSegment::plain(parts, spoken.join(", ")) })
}

/// The zoom segment: a `ZOOM` chip with no name while a split pane is zoomed.
pub(super) fn format_zoom(
    inputs: &StatusLineInputs,
    env: &FormatEnv<'_>,
) -> Option<FormattedSegment> {
    if !inputs.zoomed {
        // When: `zoomed` is false, every pane of the active tab is drawn.
        return None;
    }
    let parts = vec![StatusPart::new(env.i18n.translate("status-chip-zoom"), StatusTone::Chip)];
    let spoken = env.i18n.translate("status-spoken-pane-zoomed");
    Some(FormattedSegment { default_click: true, ..FormattedSegment::plain(parts, spoken) })
}

/// Formatter of a catalog id whose segment has not landed: shows nothing.
/// The `frame` segment: `Frame: 4.1 ms · GPU`, the mean CPU-side time of content frames, with the
/// rate in the spoken value; `always` reads `idle · GPU` with no content frames.
pub(super) fn format_frame(
    inputs: &StatusLineInputs,
    env: &FormatEnv<'_>,
) -> Option<FormattedSegment> {
    let frame = inputs.frame?;
    let presenter = env.i18n.translate(match frame.presenter {
        PresenterKind::Hardware => "status-value-presenter-gpu",
        PresenterKind::SoftwareAdapter => "status-value-presenter-software",
        PresenterKind::Gdi => "status-value-presenter-gdi",
    });
    let separator = symbol(env.icons, " · ", " - ");
    let (value, spoken) = if frame.frames == 0 {
        // When: frames is 0, the interval presented no content frame.
        if env.show != ShowMode::Always {
            // When: frames is 0 and show is active, an idle window shows nothing.
            return None;
        }
        let idle = env.i18n.translate("status-value-idle");
        (format!("{idle}{separator}{presenter}"), format!("{idle}, {presenter}"))
    } else {
        // When: frames is nonzero, the interval presented content.
        let tenths = (frame.mean_frame_us + 50) / 100;
        let millis = format!("{}{}{} ms", tenths / 10, env.numbers.decimal, tenths % 10);
        let fps = env.i18n.t_args(
            "status-spoken-frame-rate",
            Some(&[("count", &format_count(frame.fps, env.numbers))]),
        );
        (format!("{millis}{separator}{presenter}"), format!("{millis}, {fps}, {presenter}"))
    };
    let parts =
        vec![name_part(env, "status-name-frame"), StatusPart::new(value, StatusTone::Value)];
    Some(FormattedSegment::plain(parts, spoken))
}

/// The `path_check` segment: `Path check: 3s`, amber with `⚠` past `warn_after_secs`.
pub(super) fn format_path_check(
    inputs: &StatusLineInputs,
    env: &FormatEnv<'_>,
) -> Option<FormattedSegment> {
    let check = inputs.path_check?;
    let age = format_duration(check.secs);
    let (value, tone) = if check.warn {
        (format!("{} {age}", symbol(env.icons, "⚠", "!")), StatusTone::Warn)
    } else {
        // When: warn is false, the check is still within warn_after_secs.
        (age.clone(), StatusTone::Value)
    };
    let mut parts = vec![name_part(env, "status-name-path-check"), StatusPart::new(value, tone)];
    let mut spoken = age;
    if check.queued > 0 {
        let queued = env.i18n.t_args(
            "status-value-path-check-queued",
            Some(&[("count", &format_count(check.queued, env.numbers))]),
        );
        spoken = format!("{spoken}, {queued}");
        parts.push(
            StatusPart::new(format!("{}{queued}", symbol(env.icons, " · ", " - ")), tone)
                .droppable(),
        );
    }
    Some(FormattedSegment::plain(parts, spoken))
}

/// The `cpu` segment: `CPU: 14%`; `always` reads `0%` before any figure.
pub(super) fn format_cpu(
    inputs: &StatusLineInputs,
    env: &FormatEnv<'_>,
) -> Option<FormattedSegment> {
    let percent = match inputs.cpu_percent {
        Some(percent) => percent,
        None if env.show == ShowMode::Always => 0,
        None => {
            // When: cpu_percent is None and show is active, there is no figure to show.
            return None;
        }
    };
    let value = format!("{}%", format_count(percent, env.numbers));
    let parts =
        vec![name_part(env, "status-name-cpu"), StatusPart::new(value.clone(), StatusTone::Value)];
    Some(FormattedSegment { default_click: true, ..FormattedSegment::plain(parts, value) })
}

/// The `out` segment: `Out: 14.0 MB/s` of PTY output before parsing.
pub(super) fn format_out(
    inputs: &StatusLineInputs,
    env: &FormatEnv<'_>,
) -> Option<FormattedSegment> {
    let rate = match inputs.out_rate {
        Some(rate) => rate,
        None if env.show == ShowMode::Always => 0,
        None => {
            // When: out_rate is None and show is active, there is no rate to show.
            return None;
        }
    };
    let value = format!("{}/s", format_bytes(rate, env.numbers));
    let parts =
        vec![name_part(env, "status-name-out"), StatusPart::new(value.clone(), StatusTone::Value)];
    Some(FormattedSegment::plain(parts, value))
}

/// The `tab_mem` segment: `Tab mem: 38 MB`, or `84 MB · 40s ago` when some charge is stale. It
/// is the terminal state SonicTerm keeps, not the program's memory, and never reads `≥`.
pub(super) fn format_tab_mem(
    inputs: &StatusLineInputs,
    env: &FormatEnv<'_>,
) -> Option<FormattedSegment> {
    let tab = inputs.tab_mem?;
    let size = format_bytes(tab.bytes, env.numbers);
    let mut parts = vec![
        name_part(env, "status-name-tab-mem"),
        StatusPart::new(size.clone(), StatusTone::Value),
    ];
    let mut spoken = size;
    if let Some(stale_secs) = tab.stale_secs {
        let ago =
            env.i18n.t_args("status-value-ago", Some(&[("age", &format_duration(stale_secs))]));
        spoken = format!("{spoken}, {ago}");
        parts.push(
            StatusPart::new(format!("{}{ago}", symbol(env.icons, " · ", " - ")), StatusTone::Name)
                .droppable(),
        );
    }
    Some(FormattedSegment::plain(parts, spoken))
}

#[cfg(test)]
#[path = "status_line_format_tests.rs"]
mod status_line_format_tests;
