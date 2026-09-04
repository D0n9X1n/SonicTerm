use super::*;

#[test]
fn default_keymap_path_lives_under_dot_sonicterm() {
    let path = default_user_keymap_path().expect("home dir should exist in tests");
    assert!(path.starts_with(crate::config::default_config_dir().unwrap()));
    let expected_name = format!("{}.toml", platform_default_keymap_name());
    assert_eq!(path.file_name().and_then(|s| s.to_str()), Some(expected_name.as_str()));
    assert_eq!(path.parent().and_then(|p| p.file_name()).and_then(|s| s.to_str()), Some("keymaps"));
}

/// A dot inside a logical name is not an implicit filesystem extension.
///
/// Only an explicit `.toml` suffix selects direct-path handling; versioned names
/// still search the user and bundled keymap directories.
#[test]
fn dotted_logical_name_resolves_through_keymap_directories() {
    let assets = Path::new("/bundle/assets");

    assert_eq!(
        Keymap::resolve_path_with("sonicterm-v1.2", assets, None).unwrap(),
        assets.join("keymaps/sonicterm-v1.2.toml")
    );
}

/// Explicit relative and Windows-shaped paths stay direct on every build host.
///
/// Testing Windows syntax unconditionally prevents a portable config from being
/// reclassified merely because the current runner uses POSIX path components.
#[test]
fn only_explicit_path_shapes_bypass_named_keymap_lookup() {
    let assets = Path::new("/bundle/assets");
    for direct in [
        "custom.toml",
        "CUSTOM.TOML",
        "./custom",
        "../custom",
        "folder/custom",
        r"folder\custom",
        r"C:\keymaps\custom",
        r"\\server\share\custom",
    ] {
        assert_eq!(
            Keymap::resolve_path_with(direct, assets, None).unwrap(),
            PathBuf::from(direct),
            "{direct:?} must remain an explicit path anchored to process CWD when relative"
        );
    }
    let absolute = std::env::current_dir().unwrap().join("custom-keymap");
    assert_eq!(
        Keymap::resolve_path_with(absolute.to_str().unwrap(), assets, None).unwrap(),
        absolute
    );
}

/// Named keymaps prefer the user directory and otherwise use bundled assets.
#[test]
fn named_keymap_resolution_prefers_user_over_bundled_assets() {
    let root =
        std::env::temp_dir().join(format!("sonicterm-keymap-resolution-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let user = root.join("user");
    let assets = root.join("assets");
    let user_keymap = user.join("keymaps/custom.toml");
    std::fs::create_dir_all(user_keymap.parent().unwrap()).unwrap();
    std::fs::create_dir_all(assets.join("keymaps")).unwrap();
    std::fs::write(&user_keymap, "user").unwrap();

    assert_eq!(Keymap::resolve_path_with("custom", &assets, Some(&user)).unwrap(), user_keymap);
    assert_eq!(
        Keymap::resolve_path_with("missing", &assets, Some(&user)).unwrap(),
        assets.join("keymaps/missing.toml")
    );

    std::fs::remove_dir_all(root).unwrap();
}

/// The portable `user` alias creates and selects the host platform keymap.
#[test]
fn user_alias_seeds_and_resolves_the_platform_default_keymap() {
    let root =
        std::env::temp_dir().join(format!("sonicterm-keymap-user-alias-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let assets = root.join("assets");
    let user = root.join("user");

    let resolved = Keymap::resolve_path_with("user", &assets, Some(&user)).unwrap();
    assert_eq!(
        resolved,
        user.join("keymaps").join(format!("{}.toml", platform_default_keymap_name()))
    );
    assert_eq!(std::fs::read_to_string(&resolved).unwrap(), Keymap::bundled_default_text());
    std::fs::write(&resolved, "user edit").unwrap();
    assert_eq!(Keymap::resolve_path_with("user", &assets, Some(&user)).unwrap(), resolved);
    assert_eq!(std::fs::read_to_string(&resolved).unwrap(), "user edit");

    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn windows_default_leaves_alt_v_for_terminal_apps() {
    if !cfg!(target_os = "windows") {
        return;
    }
    let keymap = Keymap::bundled_default();

    assert_eq!(keymap.lookup("alt+v"), None);
    assert_eq!(keymap.lookup("ctrl+shift+v"), Some(&Action::PasteFromClipboard));
}

/// Every bundled platform keymap must parse cleanly with the strict loader.
///
/// Regression guard: when an action is removed/renamed in the
/// `Action` enum (as `show_keymap_cheatsheet` was), the bundled
/// keymap that still references it would make `toml::from_str::<Keymap>`
/// fail on the unknown variant. At runtime that whole-document failure
/// drops *every* binding and silently falls back to defaults. Parsing each
/// shipped keymap here turns that into a compile-time-embedded CI failure
/// instead of a broken keymap reaching users.
#[test]
fn every_bundled_keymap_parses_with_no_dead_actions() {
    let bundles: [(&str, &str); 3] = [
        ("macos", include_str!("../../../assets/keymaps/sonicterm-macos.toml")),
        ("windows", include_str!("../../../assets/keymaps/sonicterm-windows.toml")),
        ("linux", include_str!("../../../assets/keymaps/sonicterm-linux.toml")),
    ];
    for (os, text) in bundles {
        let km: Keymap = toml::from_str(text)
            .unwrap_or_else(|e| panic!("bundled {os} keymap must parse (dead action?): {e}"));
        assert!(!km.bindings.is_empty(), "bundled {os} keymap should have bindings");
        assert!(
            km.bindings.iter().all(|binding| binding.action.0 != Action::MoveTabToNewWindow),
            "bundled {os} keymap must leave Move Tab to New Window unbound"
        );
        assert!(
            km.bindings.iter().all(|binding| binding.action.0 != Action::SaveCurrentSettings),
            "bundled {os} keymap must leave Save Current Settings unbound"
        );
    }
}

#[test]
fn move_tab_to_new_window_action_parses_for_user_bindings() {
    let source = r#"
[meta]
name = "test"
version = "1"

[[binding]]
keys = "alt+shift+n"
action = "move_tab_to_new_window"
"#;
    let keymap: Keymap = toml::from_str(source).expect("action should deserialize");
    assert_eq!(keymap.lookup("alt+shift+n"), Some(&Action::MoveTabToNewWindow));
}

#[test]
fn save_current_settings_action_parses_for_user_bindings() {
    // Contract: users can bind the save action even though bundled keymaps leave it unbound.
    let source = r#"
[meta]
name = "test"
version = "1"

[[binding]]
keys = "alt+shift+s"
action = "save_current_settings"
"#;
    let keymap: Keymap = toml::from_str(source).expect("action should deserialize");
    assert_eq!(keymap.lookup("alt+shift+s"), Some(&Action::SaveCurrentSettings));
}

/// `bundled_default()` (the runtime fallback) parses for the host platform.
#[test]
fn bundled_default_parses() {
    let km = Keymap::bundled_default();
    assert!(!km.bindings.is_empty(), "bundled default must have bindings");
}

/// Documents the raw-serde failure mode: a single unknown
/// action makes a *whole-document* `toml::from_str::<Keymap>` fail. This is
/// the low-level behavior the resilient loader (`parse_resilient`) now wraps
/// so a stale action no longer drops the user's whole keymap.
#[test]
fn unknown_action_fails_whole_document_serde_parse() {
    let toml_src = r#"
[meta]
name = "test"
version = "1"

[[binding]]
keys = "super+t"
action = "new_tab"

[[binding]]
keys = "super+shift+?"
action = "show_keymap_cheatsheet"
"#;
    let parsed: Result<Keymap, _> = toml::from_str(toml_src);
    assert!(parsed.is_err(), "raw serde must fail the whole parse on one unknown action");
    let msg = format!("{}", parsed.unwrap_err());
    assert!(
        msg.contains("show_keymap_cheatsheet") || msg.contains("unknown variant"),
        "error should name the offending action; got: {msg}"
    );
}

/// fix: the resilient loader keeps valid bindings and drops only the
/// one referencing a removed action — instead of discarding the whole
/// keymap and falling back to defaults.
#[test]
fn resilient_parse_drops_only_the_unknown_action_binding() {
    let toml_src = r#"
[meta]
name = "test"
version = "1"

[[binding]]
keys = "super+t"
action = "new_tab"

[[binding]]
keys = "super+shift+?"
action = "show_keymap_cheatsheet"

[[binding]]
keys = "super+w"
action = "close_active_pane_or_tab"
"#;
    let km = Keymap::parse_resilient(toml_src, "test").expect("structurally valid -> Ok");
    assert_eq!(km.bindings.len(), 2, "only the unknown-action binding should be dropped");
    assert_eq!(km.lookup("super+t"), Some(&Action::NewTab));
    assert_eq!(km.lookup("super+w"), Some(&Action::CloseActivePaneOrTab));
    assert!(km.lookup("super+shift+?").is_none(), "dead binding must not resolve");
}

/// Parameterized actions in table form (`{ activate_tab = 0 }`,
/// `{ scroll = "line_up" }`) must still resolve through the resilient path.
#[test]
fn resilient_parse_keeps_parameterized_table_actions() {
    let toml_src = r#"
[meta]
name = "test"
version = "1"

[[binding]]
keys = "super+1"
action = { activate_tab = 0 }

[[binding]]
keys = "super+k"
action = { scroll = "line_up" }
"#;
    let km = Keymap::parse_resilient(toml_src, "test").expect("valid");
    assert_eq!(km.bindings.len(), 2);
    assert_eq!(km.lookup("super+1"), Some(&Action::ActivateTab(0)));
    assert_eq!(km.lookup("super+k"), Some(&Action::Scroll(ScrollAction::LineUp)));
}

/// Structural damage (invalid TOML / missing `[meta]`) still returns `Err`,
/// so `load_or_default` keeps its bundled-default fallback for genuinely
/// broken files — only *unknown actions* are tolerated, not garbage.
#[test]
fn resilient_parse_still_errors_on_structural_damage() {
    let no_meta = r#"
[[binding]]
keys = "super+t"
action = "new_tab"
"#;
    assert!(Keymap::parse_resilient(no_meta, "test").is_err(), "missing [meta] must error");

    let garbage = "this is = = not toml [[[";
    assert!(Keymap::parse_resilient(garbage, "test").is_err(), "invalid TOML must error");
}

/// End-to-end: a keymap where *every* binding is a dead action parses to an
/// empty binding set (structurally valid). `load_or_default` then still has
/// the bundled default to fall back on if desired, but the resilient parse
/// itself does not error.
#[test]
fn resilient_parse_all_unknown_yields_empty_not_error() {
    let toml_src = r#"
[meta]
name = "test"
version = "1"

[[binding]]
keys = "super+shift+?"
action = "show_keymap_cheatsheet"
"#;
    let km = Keymap::parse_resilient(toml_src, "test").expect("structurally valid");
    assert!(km.bindings.is_empty(), "all-dead-action file yields no bindings (no panic, no error)");
}
