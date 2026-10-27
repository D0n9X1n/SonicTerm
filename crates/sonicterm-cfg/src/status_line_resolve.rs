//! Validate and resolve status-line TOML tables into ordered segment settings.

use super::*;

/// Keys every segment table accepts.
const COMMON_KEYS: &[&str] = &["show", "priority", "click"];
/// Extra keys of the memory segment table.
const MEM_KEYS: &[&str] = &["show_trend", "sparkline", "warn_mib", "critical_mib"];
/// Extra keys of the selection segment table.
const SEL_KEYS: &[&str] = &["copied_secs"];
/// Extra keys of the paste segment table.
const PASTE_KEYS: &[&str] = &["paste"];
/// Extra keys of the input-method segment table.
const IME_KEYS: &[&str] = &["hold_secs"];
/// Extra keys of the input segment table.
const INPUT_KEYS: &[&str] = &["hold_secs", "held_after_ms"];
/// Extra keys of the CPU segment table.
const CPU_KEYS: &[&str] = &["scale", "min_percent", "hide_ratio", "hold_samples"];
/// Extra keys of the output-rate segment table.
const OUT_KEYS: &[&str] = &["scope", "min_bytes_per_sec", "hide_ratio", "hold_samples"];
/// Extra keys of the path-check segment table.
const PATH_CHECK_KEYS: &[&str] = &["show_after_ms", "hold_ms", "warn_after_secs"];
/// Extra keys of the glyphs segment table.
const GLYPHS_KEYS: &[&str] = &["min_count", "ignore_private_use"];
/// Extra keys of the clipboard segment table.
const CLIP_KEYS: &[&str] = &["hold_secs"];
/// Extra keys of the alerts segment table.
const ALERTS_KEYS: &[&str] = &["min_secs", "sources"];
/// Extra keys of the size segment table.
const SIZE_KEYS: &[&str] = &["hold_ms"];

/// Keys a segment table accepts beyond [`COMMON_KEYS`].
fn segment_keys(id: StatusSegmentId) -> &'static [&'static str] {
    match id {
        StatusSegmentId::Mem => MEM_KEYS,
        StatusSegmentId::Sel => SEL_KEYS,
        StatusSegmentId::Paste => PASTE_KEYS,
        StatusSegmentId::Ime => IME_KEYS,
        StatusSegmentId::Size => SIZE_KEYS,
        StatusSegmentId::Alerts => ALERTS_KEYS,
        StatusSegmentId::Clip => CLIP_KEYS,
        StatusSegmentId::Input => INPUT_KEYS,
        StatusSegmentId::Glyphs => GLYPHS_KEYS,
        StatusSegmentId::Cpu => CPU_KEYS,
        StatusSegmentId::Out => OUT_KEYS,
        StatusSegmentId::PathCheck => PATH_CHECK_KEYS,
        _ => &[],
    }
}

impl StatusLineConfig {
    /// Check the table without keeping the result; the load path calls this
    /// so an invalid `[status_line]` fails the whole config.
    pub fn validate(&self) -> Result<()> {
        self.resolve().map(|_| ())
    }

    /// Apply the preset, explicit lists and segment tables, validating every
    /// value. Unknown keys are ignored here and listed by [`Self::unknown_keys`].
    pub fn resolve(&self) -> Result<ResolvedStatusLine> {
        if !UPDATE_INTERVAL_RANGE_MS.contains(&self.update_interval_ms) {
            // `update_interval_ms` is outside 500..=60000, sampling cadence is unusable.
            bail!(
                "status_line.update_interval_ms must be in 500..=60000, got {}",
                self.update_interval_ms
            );
        }
        let Some(font_scale) = StatusFontScale::from_f64(self.font_scale) else {
            // `font_scale` is outside 0.3..=1.0 or not a number, so no strip size follows from it.
            bail!("status_line.font_scale must be in 0.3..=1.0, got {}", self.font_scale);
        };
        let [preset_left, preset_center, preset_right] = self.preset.lists();
        let lists = [
            (StatusSide::Left, "left", self.left.clone().unwrap_or(preset_left)),
            (StatusSide::Center, "center", self.center.clone().unwrap_or(preset_center)),
            (StatusSide::Right, "right", self.right.clone().unwrap_or(preset_right)),
        ];
        let mut mem = MemSettings {
            sparkline: self.preset == StatusPreset::Diagnostics,
            ..MemSettings::default()
        };
        let mut sel = SelSettings::default();
        let mut paste = PasteWarning::default();
        let mut ime = ImeSettings::default();
        let mut size = SizeSettings::default();
        let mut alerts = AlertsSettings::default();
        let mut clip = ClipSettings::default();
        let mut input = InputSettings::default();
        let mut glyphs = GlyphsSettings::default();
        let mut cpu = CpuSettings::default();
        let mut out = OutSettings::default();
        let mut path_check = PathCheckSettings::default();
        let mut segments = Vec::new();
        for (side, list_name, ids) in lists {
            for id in ids {
                if segments.iter().any(|segment: &ResolvedSegment| segment.id == id) {
                    // `id` already sits in a list, a second slot would draw it twice.
                    bail!(
                        "status_line.{list_name}: `{}` appears in more than one place",
                        id.as_str()
                    );
                }
                if !id.meta().implemented {
                    // The build has no formatter for `id`, the list could not draw it.
                    bail!(
                        "status_line.{list_name}: `{}` is not available in this build",
                        id.as_str()
                    );
                }
                segments.push(ResolvedSegment {
                    id,
                    side,
                    show: id.meta().default_show,
                    priority: id.meta().default_priority,
                    click: ClickBinding::Default,
                });
            }
        }
        let mut unlisted: Vec<ResolvedSegment> = StatusSegmentId::ALL
            .into_iter()
            .filter(|id| id.meta().implemented)
            .filter(|id| !segments.iter().any(|segment| segment.id == *id))
            .map(|id| ResolvedSegment {
                id,
                side: id.meta().home_side,
                show: id.meta().default_show,
                priority: id.meta().default_priority,
                click: ClickBinding::Default,
            })
            .collect();
        for (key, value) in &self.segment {
            let Some(id) = StatusSegmentId::from_name(key) else {
                // When: `key` is not a catalog id, it is an unknown key of `segment`.
                continue;
            };
            let Some(table) = value.as_table() else {
                // `status_line.segment.<id>` is not a table, nothing in it can apply.
                bail!("status_line.segment.{key} must be a table");
            };
            let meta = id.meta();
            let target =
                segments.iter_mut().chain(unlisted.iter_mut()).find(|segment| segment.id == id);
            let mut scratch = ResolvedSegment {
                id,
                side: meta.home_side,
                show: meta.default_show,
                priority: meta.default_priority,
                click: ClickBinding::Default,
            };
            // A table for an unimplemented segment is still validated, so a typo is caught; an
            // unlisted implemented one keeps its keys for a session enable.
            let resolved = target.unwrap_or(&mut scratch);
            apply_common_keys(resolved, &meta, table)
                .with_context(|| format!("status_line.segment.{key}"))?;
            if id == StatusSegmentId::Mem {
                apply_mem_keys(&mut mem, table).context("status_line.segment.mem")?;
            }
            if id == StatusSegmentId::Sel {
                apply_sel_keys(&mut sel, table).context("status_line.segment.sel")?;
            }
            if id == StatusSegmentId::Paste {
                if let Some(value) = table.get("paste") {
                    paste = value.clone().try_into().context(
                        "status_line.segment.paste: paste must be \"when_off\" or \"never\"",
                    )?;
                }
            }
            if id == StatusSegmentId::Ime {
                if let Some(value) = table.get("hold_secs") {
                    ime.hold_secs = value
                        .as_integer()
                        .and_then(|secs| u64::try_from(secs).ok())
                        .filter(|secs| HOLD_SECS_RANGE.contains(secs))
                        .context(
                            "status_line.segment.ime: hold_secs must be an integer in 1..=60",
                        )?;
                }
            }
            if id == StatusSegmentId::Input {
                apply_input_keys(&mut input, table).context("status_line.segment.input")?;
            }
            if id == StatusSegmentId::Glyphs {
                apply_glyphs_keys(&mut glyphs, table).context("status_line.segment.glyphs")?;
            }
            if id == StatusSegmentId::Cpu {
                apply_cpu_keys(&mut cpu, table).context("status_line.segment.cpu")?;
            }
            if id == StatusSegmentId::Out {
                apply_out_keys(&mut out, table).context("status_line.segment.out")?;
            }
            if id == StatusSegmentId::PathCheck {
                apply_path_check_keys(&mut path_check, table)
                    .context("status_line.segment.path_check")?;
            }
            if id == StatusSegmentId::Clip {
                if let Some(value) = table.get("hold_secs") {
                    clip.hold_secs = value
                        .as_integer()
                        .and_then(|secs| u64::try_from(secs).ok())
                        .filter(|secs| CLIP_HOLD_SECS_RANGE.contains(secs))
                        .context(
                            "status_line.segment.clip: hold_secs must be an integer in 1..=600",
                        )?;
                }
            }
            if id == StatusSegmentId::Alerts {
                apply_alerts_keys(&mut alerts, table).context("status_line.segment.alerts")?;
            }
            if id == StatusSegmentId::Size {
                if let Some(value) = table.get("hold_ms") {
                    size.hold_ms = value
                        .as_integer()
                        .and_then(|millis| u64::try_from(millis).ok())
                        .filter(|millis| HOLD_MS_RANGE.contains(millis))
                        .context(
                            "status_line.segment.size: hold_ms must be an integer in 250..=10000",
                        )?;
                }
            }
        }
        let interval = self.accessibility.announce_min_interval_ms;
        if !ANNOUNCE_MIN_INTERVAL_RANGE_MS.contains(&interval) {
            // The interval is outside 1000..=60000, so announcements would flood or never repeat.
            bail!(
                "status_line.accessibility.announce_min_interval_ms must be in 1000..=60000, got {interval}"
            );
        }
        if mem.critical_mib < mem.warn_mib {
            // `critical_mib < warn_mib`, the red level would sit below the amber one.
            bail!(
                "status_line.segment.mem.critical_mib ({}) must be >= warn_mib ({})",
                mem.critical_mib,
                mem.warn_mib
            );
        }
        Ok(ResolvedStatusLine {
            enabled: self.enabled,
            hide_sensitive: self.hide_sensitive,
            icons: self.icons,
            position: self.position,
            font_scale,
            update_interval_ms: self.update_interval_ms,
            preset: self.preset,
            segments,
            unlisted,
            mem,
            sel,
            paste,
            ime,
            size,
            alerts,
            clip,
            input,
            glyphs,
            cpu,
            out,
            path_check,
            accessibility: ResolvedAccessibility {
                expose: self.accessibility.expose,
                announce: self.accessibility.announce.clone(),
                announce_min_interval_ms: interval,
            },
        })
    }

    /// Dotted paths of every key the status line ignores: unknown keys under
    /// `[status_line]`, unknown ids under `segment`, and unknown keys inside a
    /// segment table.
    pub fn unknown_keys(&self) -> Vec<String> {
        let mut unknown: Vec<String> =
            self.extra.keys().map(|key| format!("status_line.{key}")).collect();
        unknown.extend(
            self.accessibility.extra.keys().map(|key| format!("status_line.accessibility.{key}")),
        );
        for (key, value) in &self.segment {
            let Some(id) = StatusSegmentId::from_name(key) else {
                // When: `key` names no catalog id, the whole table is unknown.
                unknown.push(format!("status_line.segment.{key}"));
                continue;
            };
            let Some(table) = value.as_table() else {
                // When: not a table, `resolve` reports it as an error instead.
                continue;
            };
            for field in table.keys() {
                let known = COMMON_KEYS.contains(&field.as_str())
                    || segment_keys(id).contains(&field.as_str());
                if !known {
                    unknown.push(format!("status_line.segment.{key}.{field}"));
                }
            }
        }
        unknown
    }

    /// One warning naming every ignored key, or `None` when there are none.
    pub fn unknown_keys_warning(&self) -> Option<String> {
        let unknown = self.unknown_keys();
        if unknown.is_empty() {
            // When: `unknown` is empty, every key is known and there is nothing to warn about.
            return None;
        }
        Some(format!("status line config ignores unknown keys: {}", unknown.join(", ")))
    }
}

/// Apply `show`, `priority` and `click` from a segment table.
fn apply_common_keys(
    segment: &mut ResolvedSegment,
    meta: &StatusSegmentMeta,
    table: &toml::Table,
) -> Result<()> {
    if let Some(value) = table.get("show") {
        let show: ShowMode =
            value.clone().try_into().context("show must be \"always\" or \"active\"")?;
        if show == ShowMode::Always && !meta.allows_always {
            // `show = "always"` on an active-only segment, there is no idle text to keep.
            bail!("show = \"always\" is not supported; use \"active\"");
        }
        segment.show = show;
    }
    if let Some(value) = table.get("priority") {
        let priority = value
            .as_integer()
            .filter(|priority| (0..=i64::from(MAX_PRIORITY)).contains(priority))
            .context("priority must be an integer in 0..=1000")?;
        segment.priority = u16::try_from(priority).context("priority out of range")?;
    }
    if let Some(value) = table.get("click") {
        let text = value.as_str().context("click must be a string")?;
        let click = ClickBinding::parse(text).context("invalid click")?;
        if !meta.clickable && matches!(click, ClickBinding::Action(_)) {
            // The segment is not clickable, an action could never run.
            bail!("click actions are not supported on this segment");
        }
        segment.click = click;
    }
    Ok(())
}

/// Apply the memory segment's own keys.
fn apply_mem_keys(mem: &mut MemSettings, table: &toml::Table) -> Result<()> {
    if let Some(value) = table.get("show_trend") {
        mem.show_trend = value.as_bool().context("show_trend must be a boolean")?;
    }
    if let Some(value) = table.get("sparkline") {
        mem.sparkline = value.as_bool().context("sparkline must be a boolean")?;
    }
    for (key, slot) in [("warn_mib", &mut mem.warn_mib), ("critical_mib", &mut mem.critical_mib)] {
        if let Some(value) = table.get(key) {
            let mib = value
                .as_integer()
                .and_then(|mib| u64::try_from(mib).ok())
                .filter(|mib| MEM_THRESHOLD_RANGE_MIB.contains(mib))
                .with_context(|| format!("{key} must be an integer in 1..=1048576"))?;
            *slot = mib;
        }
    }
    Ok(())
}

/// Apply the input segment's own keys.
fn apply_input_keys(input: &mut InputSettings, table: &toml::Table) -> Result<()> {
    if let Some(value) = table.get("hold_secs") {
        input.hold_secs = value
            .as_integer()
            .and_then(|secs| u64::try_from(secs).ok())
            .filter(|secs| CLIP_HOLD_SECS_RANGE.contains(secs))
            .context("hold_secs must be an integer in 1..=600")?;
    }
    if let Some(value) = table.get("held_after_ms") {
        input.held_after_ms = value
            .as_integer()
            .and_then(|millis| u64::try_from(millis).ok())
            .filter(|millis| HELD_AFTER_MS_RANGE.contains(millis))
            .context("held_after_ms must be an integer in 250..=60000")?;
    }
    Ok(())
}

/// Read `key` from `table` as an integer within `range`, if present.
fn integer_key(
    table: &toml::Table,
    key: &str,
    range: std::ops::RangeInclusive<u64>,
) -> Result<Option<u64>> {
    let Some(value) = table.get(key) else {
        // When: table has no `key`, the default stays.
        return Ok(None);
    };
    let integer = value
        .as_integer()
        .and_then(|integer| u64::try_from(integer).ok())
        .filter(|integer| range.contains(integer))
        .with_context(|| {
            format!("{key} must be an integer in {}..={}", range.start(), range.end())
        })?;
    Ok(Some(integer))
}

/// Read `hide_ratio` (0.1..=1.0) from `table` as thousandths, if present.
fn hide_ratio_key(table: &toml::Table) -> Result<Option<u64>> {
    let Some(value) = table.get("hide_ratio") else {
        // When: table has no hide_ratio, the default stays.
        return Ok(None);
    };
    let ratio = value
        .as_float()
        .or_else(|| value.as_integer().map(|integer| integer as f64))
        .filter(|ratio| (0.1..=1.0).contains(ratio))
        .context("hide_ratio must be a number in 0.1..=1.0")?;
    Ok(Some((ratio * 1000.0).round() as u64))
}

/// Apply the CPU segment's own keys.
fn apply_cpu_keys(cpu: &mut CpuSettings, table: &toml::Table) -> Result<()> {
    if let Some(value) = table.get("scale") {
        cpu.scale = match value.as_str() {
            Some("auto") => CpuScale::Auto,
            Some("core") => CpuScale::Core,
            Some("total") => CpuScale::Total,
            _ => anyhow::bail!("scale must be \"auto\", \"core\" or \"total\""),
        };
    }
    if let Some(percent) = integer_key(table, "min_percent", 0..=1000)? {
        cpu.min_percent = percent;
    }
    if let Some(permille) = hide_ratio_key(table)? {
        cpu.hide_permille = permille;
    }
    if let Some(samples) = integer_key(table, "hold_samples", 0..=10)? {
        cpu.hold_samples = samples;
    }
    Ok(())
}

/// Apply the output-rate segment's own keys.
fn apply_out_keys(out: &mut OutSettings, table: &toml::Table) -> Result<()> {
    if let Some(value) = table.get("scope") {
        out.pane_scope = match value.as_str() {
            Some("pane") => true,
            Some("window") => false,
            _ => anyhow::bail!("scope must be \"pane\" or \"window\""),
        };
    }
    if let Some(rate) = integer_key(table, "min_bytes_per_sec", 1024..=1 << 30)? {
        out.min_bytes_per_sec = rate;
    }
    if let Some(permille) = hide_ratio_key(table)? {
        out.hide_permille = permille;
    }
    if let Some(samples) = integer_key(table, "hold_samples", 0..=10)? {
        out.hold_samples = samples;
    }
    Ok(())
}

/// Apply the path-check segment's own keys.
fn apply_path_check_keys(path_check: &mut PathCheckSettings, table: &toml::Table) -> Result<()> {
    if let Some(millis) = integer_key(table, "show_after_ms", 100..=5000)? {
        path_check.show_after_ms = millis;
    }
    if let Some(millis) = integer_key(table, "hold_ms", 0..=10_000)? {
        path_check.hold_ms = millis;
    }
    if let Some(secs) = integer_key(table, "warn_after_secs", 1..=600)? {
        path_check.warn_after_secs = secs;
    }
    Ok(())
}

/// Apply the glyphs segment's own keys.
fn apply_glyphs_keys(glyphs: &mut GlyphsSettings, table: &toml::Table) -> Result<()> {
    if let Some(value) = table.get("min_count") {
        glyphs.min_count = value
            .as_integer()
            .and_then(|count| u64::try_from(count).ok())
            .filter(|count| GLYPHS_MIN_COUNT_RANGE.contains(count))
            .context("min_count must be an integer in 1..=1000")?;
    }
    if let Some(value) = table.get("ignore_private_use") {
        glyphs.ignore_private_use =
            value.as_bool().context("ignore_private_use must be true or false")?;
    }
    Ok(())
}

/// Apply the alerts segment's own keys.
fn apply_alerts_keys(alerts: &mut AlertsSettings, table: &toml::Table) -> Result<()> {
    if let Some(value) = table.get("min_secs") {
        alerts.min_secs = value
            .as_integer()
            .and_then(|secs| u64::try_from(secs).ok())
            .filter(|secs| MIN_SECS_RANGE.contains(secs))
            .context("min_secs must be an integer in 0..=86400")?;
    }
    if let Some(value) = table.get("sources") {
        let sources = value.as_array().context("sources must be a list")?;
        let (mut bell, mut notify, mut finished) = (false, false, false);
        for source in sources {
            match source.as_str() {
                Some("bell") => bell = true,
                Some("notify") => notify = true,
                Some("finished") => finished = true,
                // An unknown source fails the load rather than silently marking nothing.
                _ => bail!("sources entries must be \"bell\", \"notify\" or \"finished\""),
            }
        }
        (alerts.bell, alerts.notify, alerts.finished) = (bell, notify, finished);
    }
    Ok(())
}

/// Apply the selection segment's own keys.
fn apply_sel_keys(sel: &mut SelSettings, table: &toml::Table) -> Result<()> {
    if let Some(value) = table.get("copied_secs") {
        sel.copied_secs = value
            .as_integer()
            .and_then(|secs| u64::try_from(secs).ok())
            .filter(|secs| COPIED_SECS_RANGE.contains(secs))
            .context("copied_secs must be an integer in 1..=10")?;
    }
    Ok(())
}

#[cfg(test)]
#[path = "status_line_resolve_tests.rs"]
mod status_line_resolve_tests;
