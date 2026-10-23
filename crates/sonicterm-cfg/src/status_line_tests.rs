use super::*;

/// Parse a `[status_line]` body into its config table.
fn parse(body: &str) -> StatusLineConfig {
    toml::from_str::<crate::config::Config>(&format!("[status_line]\n{body}"))
        .expect("status line table parses")
        .status_line
}

/// The ids of every resolved segment on `side`, in list order.
fn side_ids(resolved: &ResolvedStatusLine, side: StatusSide) -> Vec<StatusSegmentId> {
    resolved
        .segments
        .iter()
        .filter(|segment| segment.side == side)
        .map(|segment| segment.id)
        .collect()
}

#[test]
fn catalog_rows_match_the_published_priority_table() {
    // The catalog table in the issue is the contract segment issues cite; every id has a row.
    use StatusSegmentId as Seg;
    let expected: [(Seg, &str, SegmentGroup, StatusSide, u16); 21] = [
        (Seg::Alerts, "alerts", SegmentGroup::OtherTabs, StatusSide::Right, 990),
        (Seg::Health, "health", SegmentGroup::Core, StatusSide::Right, 950),
        (Seg::Mem, "mem", SegmentGroup::Core, StatusSide::Right, 900),
        (Seg::Mode, "mode", SegmentGroup::Core, StatusSide::Left, 800),
        (Seg::Input, "input", SegmentGroup::Input, StatusSide::Left, 780),
        (Seg::Clip, "clip", SegmentGroup::ProgramReported, StatusSide::Right, 760),
        (Seg::PathCheck, "path_check", SegmentGroup::Diagnostics, StatusSide::Right, 700),
        (Seg::Link, "link", SegmentGroup::Pointer, StatusSide::Center, 600),
        (Seg::Paste, "paste", SegmentGroup::Input, StatusSide::Left, 560),
        (Seg::Sel, "sel", SegmentGroup::Pointer, StatusSide::Center, 520),
        (Seg::Ime, "ime", SegmentGroup::Input, StatusSide::Left, 500),
        (Seg::Scroll, "scroll", SegmentGroup::Core, StatusSide::Left, 400),
        (Seg::Cpu, "cpu", SegmentGroup::Diagnostics, StatusSide::Right, 380),
        (Seg::Frame, "frame", SegmentGroup::Diagnostics, StatusSide::Right, 360),
        (Seg::Out, "out", SegmentGroup::Diagnostics, StatusSide::Right, 340),
        (Seg::TabMem, "tab_mem", SegmentGroup::Diagnostics, StatusSide::Right, 330),
        (Seg::Glyphs, "glyphs", SegmentGroup::Display, StatusSide::Right, 320),
        (Seg::History, "history", SegmentGroup::Display, StatusSide::Right, 300),
        (Seg::Font, "font", SegmentGroup::Display, StatusSide::Right, 280),
        (Seg::Zoom, "zoom", SegmentGroup::Display, StatusSide::Left, 260),
        (Seg::Size, "size", SegmentGroup::Display, StatusSide::Right, 80),
    ];
    assert_eq!(StatusSegmentId::ALL.len(), expected.len());
    for (index, (id, name, group, side, priority)) in expected.into_iter().enumerate() {
        assert_eq!(StatusSegmentId::ALL[index], id, "catalog order");
        let meta = id.meta();
        assert_eq!(meta.id, id);
        assert_eq!(id.as_str(), name);
        assert_eq!(StatusSegmentId::from_name(name), Some(id));
        assert_eq!(serde_plain::to_string(&id).unwrap(), name, "serde spelling");
        assert_eq!(
            (meta.group, meta.home_side, meta.default_priority),
            (group, side, priority),
            "{name}"
        );
        assert!(meta.spoken_name_key.starts_with("status-spoken-"), "{name}");
    }
}

#[test]
fn protection_and_sensitivity_follow_the_catalog() {
    // Alerts, health errors, the broadcast chip and the link host survive narrowing longest.
    use StatusSegmentId as Seg;
    assert_eq!(Seg::Alerts.meta().protection, Protection::Always);
    assert_eq!(Seg::Health.meta().protection, Protection::Errors);
    assert_eq!(Seg::Mode.meta().protection, Protection::BroadcastChip);
    assert_eq!(Seg::Link.meta().protection, Protection::DestinationHost);
    assert_eq!(Seg::Link.meta().sensitivity, Sensitivity::Whole);
    assert!(!Seg::Link.meta().allows_always, "link has no idle text");
    assert_eq!(Seg::Mem.meta().default_show, ShowMode::Always);
    let implemented: Vec<Seg> = Seg::ALL.into_iter().filter(|id| id.meta().implemented).collect();
    assert_eq!(
        implemented,
        [
            Seg::Alerts,
            Seg::Health,
            Seg::Mem,
            Seg::Mode,
            Seg::Input,
            Seg::Clip,
            Seg::Link,
            Seg::Paste,
            Seg::Sel,
            Seg::Ime,
            Seg::Scroll,
            Seg::Glyphs,
            Seg::History,
            Seg::Font,
            Seg::Zoom,
            Seg::Size
        ]
    );
    for id in [Seg::Paste, Seg::Ime] {
        assert!(!id.meta().allows_always && !id.meta().clickable, "{id:?} only explains input");
        assert_eq!(id.meta().sensitivity, Sensitivity::None, "{id:?} shows no typed text");
    }
}

#[test]
fn default_table_is_the_developer_preset_and_off() {
    // An empty `[status_line]` is the developer preset, disabled, with only `mem` always shown.
    let resolved = StatusLineConfig::default().resolve().unwrap();
    assert!(!resolved.enabled);
    assert_eq!(resolved, parse("preset = \"developer\"\n").resolve().unwrap());
    assert_eq!(
        side_ids(&resolved, StatusSide::Left),
        [
            StatusSegmentId::Mode,
            StatusSegmentId::Scroll,
            StatusSegmentId::Input,
            StatusSegmentId::Paste
        ]
    );
    assert_eq!(
        side_ids(&resolved, StatusSide::Center),
        [StatusSegmentId::Link, StatusSegmentId::Sel]
    );
    assert_eq!(
        side_ids(&resolved, StatusSide::Right),
        [
            StatusSegmentId::Alerts,
            StatusSegmentId::Clip,
            StatusSegmentId::Health,
            StatusSegmentId::Glyphs,
            StatusSegmentId::History,
            StatusSegmentId::Mem
        ]
    );
    for segment in &resolved.segments {
        let expected =
            if segment.id == StatusSegmentId::Mem { ShowMode::Always } else { ShowMode::Active };
        assert_eq!(segment.show, expected, "{:?}", segment.id);
        assert_eq!(segment.click, ClickBinding::Default);
    }
    assert_eq!(resolved.mem, MemSettings::default());
    assert_eq!(resolved.sel, SelSettings::default());
    assert_eq!(resolved.update_interval_ms, 2000);
    assert_eq!(resolved.icons, StatusIcons::Unicode);
}

#[test]
fn presets_resolve_to_their_implemented_lists() {
    // Each preset lists only ids this build implements; diagnostics turns the sparkline on.
    let minimal = parse("preset = \"minimal\"\n").resolve().unwrap();
    assert!(side_ids(&minimal, StatusSide::Left).is_empty());
    assert!(side_ids(&minimal, StatusSide::Center).is_empty());
    assert_eq!(
        side_ids(&minimal, StatusSide::Right),
        [
            StatusSegmentId::Alerts,
            StatusSegmentId::Clip,
            StatusSegmentId::Health,
            StatusSegmentId::Mem
        ]
    );
    assert!(!minimal.mem.sparkline);
    let diagnostics = parse("preset = \"diagnostics\"\n").resolve().unwrap();
    let developer = StatusLineConfig::default().resolve().unwrap();
    assert_eq!(diagnostics.segments, developer.segments);
    assert!(diagnostics.mem.sparkline);
    for preset in [StatusPreset::Minimal, StatusPreset::Developer, StatusPreset::Diagnostics] {
        for list in preset.lists() {
            assert!(list.iter().all(|id| id.meta().implemented), "{preset:?}");
        }
    }
}

#[test]
fn explicit_list_replaces_only_that_list() {
    // `left = [...]` replaces the preset's left list and keeps its center and right lists.
    let resolved = parse("left = [\"scroll\"]\n").resolve().unwrap();
    assert_eq!(side_ids(&resolved, StatusSide::Left), [StatusSegmentId::Scroll]);
    assert_eq!(
        side_ids(&resolved, StatusSide::Center),
        [StatusSegmentId::Link, StatusSegmentId::Sel]
    );
    assert_eq!(
        side_ids(&resolved, StatusSide::Right),
        [
            StatusSegmentId::Alerts,
            StatusSegmentId::Clip,
            StatusSegmentId::Health,
            StatusSegmentId::Glyphs,
            StatusSegmentId::History,
            StatusSegmentId::Mem
        ]
    );
    let moved = parse("left = [\"mem\"]\nright = [\"health\"]\n").resolve().unwrap();
    assert_eq!(moved.segment(StatusSegmentId::Mem).unwrap().side, StatusSide::Left);
}

#[test]
fn segment_keys_override_preset_values() {
    // Explicit segment keys win over the preset's values, for listed and unlisted segments alike.
    let resolved = parse(
        "[status_line.segment.mem]\nshow = \"active\"\npriority = 10\nclick = \"none\"\n\
         show_trend = false\nsparkline = true\nwarn_mib = 100\ncritical_mib = 200\n",
    )
    .resolve()
    .unwrap();
    let mem = resolved.segment(StatusSegmentId::Mem).unwrap();
    assert_eq!((mem.show, mem.priority, &mem.click), (ShowMode::Active, 10, &ClickBinding::None));
    assert_eq!(
        resolved.mem,
        MemSettings { show_trend: false, sparkline: true, warn_mib: 100, critical_mib: 200 }
    );
    let diagnostics =
        parse("preset = \"diagnostics\"\n[status_line.segment.mem]\nsparkline = false\n");
    assert!(!diagnostics.resolve().unwrap().mem.sparkline);
}

#[test]
fn click_accepts_keywords_and_unit_actions_only() {
    // `click` takes default, none, or a keymap action without arguments; anything else fails.
    assert_eq!(ClickBinding::parse("default").unwrap(), ClickBinding::Default);
    assert_eq!(ClickBinding::parse("none").unwrap(), ClickBinding::None);
    assert_eq!(
        ClickBinding::parse("show_memory_usage").unwrap(),
        ClickBinding::Action(Action::ShowMemoryUsage)
    );
    assert_eq!(
        ClickBinding::parse("edit_config_file").unwrap(),
        ClickBinding::Action(Action::EditConfigFile)
    );
    assert!(ClickBinding::parse("copy_the_path").is_err(), "invented pseudo-action");
    assert!(ClickBinding::parse("activate_tab").is_err(), "action that takes an argument");
    let resolved =
        parse("[status_line.segment.scroll]\nclick = \"toggle_pane_zoom\"\n").resolve().unwrap();
    assert_eq!(
        resolved.segment(StatusSegmentId::Scroll).unwrap().click,
        ClickBinding::Action(Action::TogglePaneZoom)
    );
    assert!(parse("[status_line.segment.scroll]\nclick = \"bogus\"\n").validate().is_err());
}

#[test]
fn non_clickable_segment_accepts_keywords_but_rejects_actions() {
    // A non-clickable segment treats default and none as inert so a copied table never breaks the load.
    let meta = StatusSegmentMeta { clickable: false, ..StatusSegmentId::Mode.meta() };
    let mut segment = ResolvedSegment {
        id: StatusSegmentId::Mode,
        side: StatusSide::Left,
        show: ShowMode::Active,
        priority: 800,
        click: ClickBinding::Default,
    };
    for keyword in ["default", "none"] {
        let table: toml::Table = toml::from_str(&format!("click = \"{keyword}\"")).unwrap();
        assert!(apply_common_keys(&mut segment, &meta, &table).is_ok(), "{keyword}");
    }
    let table: toml::Table = toml::from_str("click = \"show_memory_usage\"").unwrap();
    assert!(apply_common_keys(&mut segment, &meta, &table).is_err());
}

#[test]
fn invalid_values_fail_the_load() {
    // Each out-of-range or ill-typed value is rejected rather than silently clamped.
    for body in [
        "update_interval_ms = 499\n",
        "update_interval_ms = 60001\n",
        "[status_line.segment.mem]\npriority = 1001\n",
        "[status_line.segment.mem]\npriority = -1\n",
        "[status_line.segment.mem]\nshow = \"sometimes\"\n",
        "[status_line.segment.link]\nshow = \"always\"\n",
        "[status_line.segment.mem]\nwarn_mib = 0\n",
        "[status_line.segment.mem]\nwarn_mib = 3000\n",
        "[status_line.segment.mem]\nsparkline = \"yes\"\n",
        "[status_line.segment.sel]\ncopied_secs = 0\n",
        "[status_line.segment.paste]\npaste = \"when_on\"\n",
        "[status_line.segment.paste]\nshow = \"always\"\n",
        "[status_line.segment.paste]\nclick = \"show_memory_usage\"\n",
        "[status_line.segment.ime]\nhold_secs = 0\n",
        "[status_line.segment.ime]\nhold_secs = 61\n",
        "[status_line.segment.size]\nhold_ms = 249\n",
        "[status_line.segment.size]\nhold_ms = 10001\n",
        "[status_line.segment.size]\nshow = \"always\"\n",
        "[status_line.segment.font]\nshow = \"always\"\n",
        "[status_line.segment.history]\nshow = \"always\"\n",
        "[status_line.segment.zoom]\nshow = \"always\"\n",
        "[status_line.segment.alerts]\nshow = \"always\"\n",
        "[status_line.segment.alerts]\nmin_secs = 86401\n",
        "[status_line.segment.alerts]\nsources = [\"activity\"]\n",
        "[status_line.segment.alerts]\nsources = \"bell\"\n",
        "[status_line.segment.clip]\nhold_secs = 0\n",
        "[status_line.segment.input]\nhold_secs = 601\n",
        "[status_line.segment.input]\nheld_after_ms = 249\n",
        "[status_line.segment.glyphs]\nmin_count = 0\n",
        "[status_line.segment.glyphs]\nmin_count = 1001\n",
        "[status_line.segment.glyphs]\nignore_private_use = 1\n",
        "[status_line.segment.glyphs]\nshow = \"always\"\n",
        "[status_line.segment.input]\nheld_after_ms = 60001\n",
        "[status_line.segment.input]\nshow = \"always\"\n",
        "[status_line.segment.input]\nclick = \"show_memory_usage\"\n",
        "[status_line.segment.clip]\nhold_secs = 601\n",
        "[status_line.segment.clip]\nshow = \"always\"\n",
        "[status_line.segment.size]\nclick = \"show_memory_usage\"\n",
        "[status_line.segment.sel]\ncopied_secs = 11\n",
        "[status_line.segment.sel]\ncopied_secs = \"2\"\n",
        "[status_line.segment]\nmem = 3\n",
        "left = [\"mem\"]\n",
        "left = [\"scroll\"]\nright = [\"scroll\"]\n",
        "right = [\"cpu\"]\n",
    ] {
        assert!(parse(body).validate().is_err(), "{body}");
    }
}

#[test]
fn unknown_list_id_and_unknown_preset_fail_parsing() {
    // A list is what the strip draws, so an id outside the catalog fails the parse itself.
    let config =
        |body: &str| toml::from_str::<crate::config::Config>(&format!("[status_line]\n{body}"));
    assert!(config("left = [\"cwd\"]\n").is_err());
    assert!(config("preset = \"everything\"\n").is_err());
    assert!(config("icons = \"none\"\n").is_err(), "icons = none was removed");
}

#[test]
fn unknown_keys_are_ignored_and_reported_once() {
    // Unknown keys never fail the load; one warning names every ignored key.
    let config = parse(
        "separator = \" | \"\n[status_line.segment.mem]\ncolour = \"red\"\nwarn_mib = 900\n\
         [status_line.segment.scroll]\nsparkline = true\n[status_line.segment.cwd]\nshow = \"active\"\n",
    );
    let resolved = config.resolve().unwrap();
    assert_eq!(resolved.mem.warn_mib, 900);
    let mut unknown = config.unknown_keys();
    unknown.sort();
    assert_eq!(
        unknown,
        [
            "status_line.segment.cwd",
            "status_line.segment.mem.colour",
            "status_line.segment.scroll.sparkline",
            "status_line.separator",
        ]
    );
    let warning = config.unknown_keys_warning().unwrap();
    assert_eq!(warning.matches("status_line.").count(), 4);
    assert!(StatusLineConfig::default().unknown_keys_warning().is_none());
}

#[test]
fn table_round_trips_through_toml() {
    // Serializing a parsed table and parsing it back keeps every key, including unknown ones.
    let config = parse(
        "enabled = true\npreset = \"minimal\"\nicons = \"ascii\"\nhide_sensitive = true\n\
         update_interval_ms = 5000\nleft = [\"scroll\"]\nlegacy = 1\n\
         [status_line.segment.mem]\nwarn_mib = 512\n",
    );
    let text = toml::to_string(&config).unwrap();
    let again: StatusLineConfig = toml::from_str(&text).unwrap();
    assert_eq!(again, config);
    assert_eq!(again.resolve().unwrap(), config.resolve().unwrap());
}

#[test]
fn sel_copied_secs_overrides_and_is_a_known_key() {
    // `copied_secs` sets how long a copy result shows; it is known only on the `sel` table.
    let config = parse(
        "[status_line.segment.sel]\ncopied_secs = 5\n[status_line.segment.mem]\ncopied_secs = 5\n",
    );
    assert_eq!(config.resolve().unwrap().sel, SelSettings { copied_secs: 5 });
    assert_eq!(config.unknown_keys(), ["status_line.segment.mem.copied_secs"]);
}

#[test]
fn paste_and_ime_keys_resolve() {
    // `paste` keeps only `when_off` (default) and `never`; `hold_secs` sets the discarded hold.
    let defaults = StatusLineConfig::default().resolve().unwrap();
    assert_eq!(
        (defaults.paste, defaults.ime),
        (PasteWarning::WhenOff, ImeSettings { hold_secs: 8 })
    );
    let config = parse(
        "[status_line.segment.paste]\npaste = \"never\"\n[status_line.segment.ime]\nhold_secs = 3\n",
    );
    let resolved = config.resolve().unwrap();
    assert_eq!((resolved.paste, resolved.ime), (PasteWarning::Never, ImeSettings { hold_secs: 3 }));
    assert!(config.unknown_keys().is_empty());
}

#[test]
fn size_hold_and_display_segments_resolve() {
    // `size` holds 1500 ms by default; `font` and `size` are opt-in, `history` is a developer default.
    let defaults = StatusLineConfig::default().resolve().unwrap();
    assert_eq!(defaults.size, SizeSettings { hold_ms: 1500 });
    assert!(defaults.segment(StatusSegmentId::History).is_some());
    assert!(defaults.segment(StatusSegmentId::Font).is_none());
    assert!(defaults.segment(StatusSegmentId::Size).is_none());
    let config =
        parse("right = [\"font\", \"size\"]\n[status_line.segment.size]\nhold_ms = 3000\n");
    let resolved = config.resolve().unwrap();
    assert_eq!(resolved.size, SizeSettings { hold_ms: 3000 });
    assert_eq!(
        side_ids(&resolved, StatusSide::Right),
        [StatusSegmentId::Font, StatusSegmentId::Size]
    );
    assert!(config.unknown_keys().is_empty());
}

#[test]
fn alerts_settings_resolve() {
    // `alerts` is in the developer and minimal presets; `sources` replaces the default set.
    let defaults = StatusLineConfig::default().resolve().unwrap();
    assert_eq!(defaults.alerts, AlertsSettings::default());
    assert!(defaults.alerts.bell && defaults.alerts.notify && defaults.alerts.finished);
    assert_eq!(defaults.alerts.min_secs, 10);
    let config = parse("[status_line.segment.alerts]\nmin_secs = 0\nsources = [\"bell\"]\n");
    let resolved = config.resolve().unwrap();
    assert_eq!(
        resolved.alerts,
        AlertsSettings { min_secs: 0, bell: true, notify: false, finished: false }
    );
    assert!(config.unknown_keys().is_empty());
    assert!(!StatusSegmentId::Alerts.meta().allows_always);
    assert!(StatusSegmentId::Alerts.meta().clickable);
}

#[test]
fn clip_hold_resolves_and_clip_is_in_both_presets() {
    // `clip` holds 10 s by default, is active-only and clickable, and joins developer and minimal.
    let defaults = StatusLineConfig::default().resolve().unwrap();
    assert_eq!(defaults.clip, ClipSettings { hold_secs: 10 });
    assert!(defaults.segment(StatusSegmentId::Clip).is_some());
    let minimal = parse("preset = \"minimal\"\n").resolve().unwrap();
    assert!(minimal.segment(StatusSegmentId::Clip).is_some());
    let config = parse("[status_line.segment.clip]\nhold_secs = 30\n");
    assert_eq!(config.resolve().unwrap().clip, ClipSettings { hold_secs: 30 });
    assert!(config.unknown_keys().is_empty());
    assert!(!StatusSegmentId::Clip.meta().allows_always);
    assert!(StatusSegmentId::Clip.meta().clickable);
}

#[test]
fn input_settings_resolve_and_input_is_a_developer_default() {
    // `input` holds a refusal 8 s and shows `held` after 1,500 ms; it is active-only and inert.
    let defaults = StatusLineConfig::default().resolve().unwrap();
    assert_eq!(defaults.input, InputSettings { hold_secs: 8, held_after_ms: 1500 });
    assert_eq!(defaults.segment(StatusSegmentId::Input).unwrap().side, StatusSide::Left);
    let config = parse("[status_line.segment.input]\nhold_secs = 3\nheld_after_ms = 250\n");
    assert_eq!(config.resolve().unwrap().input, InputSettings { hold_secs: 3, held_after_ms: 250 });
    assert!(config.unknown_keys().is_empty());
    assert!(!StatusSegmentId::Input.meta().allows_always);
    assert!(!StatusSegmentId::Input.meta().clickable);
}

#[test]
fn glyphs_settings_resolve_and_glyphs_is_a_developer_default() {
    // `glyphs` counts from 1 and keeps private use by default; it is active-only and clickable.
    let defaults = StatusLineConfig::default().resolve().unwrap();
    assert_eq!(defaults.glyphs, GlyphsSettings { min_count: 1, ignore_private_use: false });
    assert_eq!(defaults.segment(StatusSegmentId::Glyphs).unwrap().side, StatusSide::Right);
    let config =
        parse("[status_line.segment.glyphs]\nmin_count = 1000\nignore_private_use = true\n");
    assert_eq!(
        config.resolve().unwrap().glyphs,
        GlyphsSettings { min_count: 1000, ignore_private_use: true }
    );
    assert!(config.unknown_keys().is_empty());
    assert!(!StatusSegmentId::Glyphs.meta().allows_always);
    assert!(StatusSegmentId::Glyphs.meta().clickable);
}
