use super::*;

#[test]
fn default_terminal_term_program_is_sonicterm() {
    let cfg = Config::default();
    assert_eq!(cfg.terminal.term_program, "SonicTerm");
}

#[test]
fn font_weight_scale_defaults_to_identity_and_is_documented() {
    let cfg = Config::default();
    assert_eq!(cfg.font.weight_scale, 1.0);
    assert!(default_config_template().contains("weight_scale = 1"));
}

#[test]
fn font_weight_scale_rejects_non_finite_and_out_of_range_values() {
    for value in [f32::NAN, f32::INFINITY, 0.0, 0.49, 5.01] {
        let font = FontConfig { weight_scale: value, ..FontConfig::default() };
        assert_eq!(font.effective_weight_scale(), 1.0);
    }
    let font = FontConfig { weight_scale: 1.1, ..FontConfig::default() };
    assert_eq!(font.effective_weight_scale(), 1.1);
    // Guards the range extension: the renderer and font-stack clamps must
    // accept the same span, otherwise a value valid here is silently reset
    // to 1.0 further down the pipeline.
    for value in [2.5, 5.0] {
        let font = FontConfig { weight_scale: value, ..FontConfig::default() };
        assert_eq!(font.effective_weight_scale(), value);
    }
}

#[test]
fn default_warm_window_pool_keeps_one_spare() {
    let cfg = Config::default();
    assert_eq!(cfg.window.warm_window_pool, 1);
    let template = default_config_template();
    assert!(template.contains("warm_window_pool = 1"));
    assert!(template.contains("0 disables"));
}

#[test]
fn default_cursor_does_not_blink() {
    let cfg = Config::default();
    assert!(!cfg.terminal.cursor_blink);
    assert!(default_config_template().contains("cursor_blink = false"));
}

#[test]
fn parses_terminal_term_program_override() {
    let cfg: Config = toml::from_str(
        r#"
[terminal]
term_program = "WezTerm"
"#,
    )
    .unwrap();
    assert_eq!(cfg.terminal.term_program, "WezTerm");
    assert_eq!(cfg.terminal.scrollback, TerminalConfig::default().scrollback);
}

#[test]
fn default_template_documents_term_program_compatibility_override() {
    let template = default_config_template();
    assert!(template.contains("term_program = \"SonicTerm\""));
    assert!(template.contains("Some tools, such as Copilot"));
    assert!(template.contains("setting term_program = \"WezTerm\""));
    assert!(template.contains("enable their WezTerm/new terminal UI path"));
}

#[test]
fn default_config_paths_live_under_dot_sonicterm() {
    let dir = default_config_dir().expect("home dir should exist in tests");
    assert!(dir.ends_with(".sonicterm"));
    assert_eq!(Config::default_path().unwrap(), dir.join("sonicterm.toml"));
}

#[test]
fn legacy_glyph_fit_values_parse_without_remaining_active_config() {
    // Contract: old v1/v2 files keep loading after the never-wired switch leaves the schema.
    for value in ["v1", "v2"] {
        let cfg: Config = toml::from_str(&format!(
            "[render]\nglyph_fit = \"{value}\"\nalt_screen_bg_fill = \"v2\"\n"
        ))
        .unwrap();

        assert_eq!(cfg.render.alt_screen_bg_fill, RenderImpl::V2);
        assert!(!cfg.to_toml().unwrap().contains("glyph_fit"));
    }
    assert!(!default_config_template().contains("glyph_fit"));
}

#[test]
fn seeding_user_examples_writes_theme_and_platform_keymaps() {
    let nonce =
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "sonicterm-config-seed-{}-{}",
        std::process::id(),
        nonce
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    seed_user_examples(&dir).unwrap();
    assert!(dir.join("themes/wezterm.toml").exists());
    assert!(dir.join("keymaps/sonicterm-macos.toml").exists());
    assert!(dir.join("keymaps/sonicterm-windows.toml").exists());
    assert!(dir.join("keymaps/sonicterm-linux.toml").exists());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn persist_font_runtime_values_changes_only_decorated_font_scalars() {
    // Contract: persistence changes only font scalars while preserving formatting and unknown keys.
    let nonce =
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "sonicterm-config-font-persist-{}-{}",
        std::process::id(),
        nonce
    ));
    let path = dir.join("sonicterm.toml");
    std::fs::create_dir_all(&dir).unwrap();
    let before = concat!(
        "# keep this header\n",
        "theme = \"night-owl\" # no theme rewrite\n",
        "future_top = \"preserved\"\n",
        "\n",
        "[font] # keep table comment\n",
        "family = \"User Mono\"\n",
        "size   = 13 # keep size comment\n",
        "line_height = 1.7\n",
        "weight_scale=1.0# keep weight comment\n",
        "future_font = \"preserved too\"\n",
        "\n",
        "[font.future]\n",
        "nested = true\n",
        "\n",
        "[window]\n",
        "cols = 177\n",
    );
    std::fs::write(&path, before).unwrap();

    Config::persist_font_runtime_values(&path, 14.5, 1.25).unwrap();

    let after = std::fs::read_to_string(&path).unwrap();
    assert_eq!(
        after,
        before
            .replace("size   = 13", "size   = 14.5")
            .replace("weight_scale=1.0", "weight_scale=1.25",)
    );
    let _ = std::fs::remove_dir_all(dir);
}

fn font_persist_test_dir(label: &str) -> std::path::PathBuf {
    let nonce =
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    let dir = std::env::temp_dir()
        .join(format!("sonicterm-config-font-persist-{label}-{}-{nonce}", std::process::id(),));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn persist_font_runtime_values_handles_integer_missing_and_inline_fields() {
    // Contract: persistence supports every valid TOML shape used for the two font scalars.
    let dir = font_persist_test_dir("shapes");
    let integer_path = dir.join("integer.toml");
    let missing_fields_path = dir.join("missing-fields.toml");
    let missing_table_path = dir.join("missing-table.toml");
    let inline_path = dir.join("inline.toml");
    std::fs::write(&integer_path, "[font]\nsize = 13\nweight_scale = 1\n").unwrap();
    std::fs::write(&missing_fields_path, "[font]\nfamily = \"Keep\"\n").unwrap();
    std::fs::write(&missing_table_path, "theme = \"keep\"\n").unwrap();
    std::fs::write(
        &inline_path,
        "font = { family = \"Keep\", size = 13, weight_scale = 1, future = true }\n",
    )
    .unwrap();

    for path in [&integer_path, &missing_fields_path, &missing_table_path, &inline_path] {
        Config::persist_font_runtime_values(path, 15.25, 2.5).unwrap();
        let cfg = Config::load_strict(path).unwrap();
        assert_eq!(cfg.font.size, 15.25);
        assert_eq!(cfg.font.weight_scale, 2.5);
    }
    assert_eq!(
        std::fs::read_to_string(&inline_path).unwrap(),
        "font = { family = \"Keep\", size = 15.25, weight_scale = 2.5, future = true }\n"
    );
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn persist_font_runtime_values_creates_starter_then_patches_it() {
    // Contract: a missing destination is seeded with user examples before live values are applied.
    let dir = font_persist_test_dir("missing-destination");
    let path = dir.join("config").join("sonicterm.toml");

    Config::persist_font_runtime_values(&path, 11.75, 0.75).unwrap();

    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.starts_with("# SonicTerm configuration."));
    let cfg = Config::load_strict(&path).unwrap();
    assert_eq!(cfg.font.size, 11.75);
    assert_eq!(cfg.font.weight_scale, 0.75);
    assert!(dir.join("config/themes/wezterm.toml").exists());
    assert!(dir.join("config/keymaps/sonicterm-windows.toml").exists());
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn persist_font_runtime_values_rejects_bad_input_and_documents_without_changes() {
    // Contract: invalid values or documents fail without changing the destination bytes.
    let dir = font_persist_test_dir("rejections");
    let valid_path = dir.join("valid.toml");
    let malformed_path = dir.join("malformed.toml");
    let non_table_path = dir.join("non-table.toml");
    let wrong_size_path = dir.join("wrong-size.toml");
    let wrong_weight_path = dir.join("wrong-weight.toml");
    std::fs::write(&valid_path, "[font]\nsize = 13\nweight_scale = 1\n").unwrap();
    std::fs::write(&malformed_path, "[font\nsize = 13\n").unwrap();
    std::fs::write(&non_table_path, "font = 7\n").unwrap();
    std::fs::write(&wrong_size_path, "[font]\nsize = \"large\"\nweight_scale = 1\n").unwrap();
    std::fs::write(&wrong_weight_path, "[font]\nsize = 13\nweight_scale = \"heavy\"\n").unwrap();

    for (size, weight) in [
        (f32::NAN, 1.0),
        (f32::INFINITY, 1.0),
        (0.0, 1.0),
        (-1.0, 1.0),
        (13.0, f32::NAN),
        (13.0, f32::INFINITY),
        (13.0, 0.49),
        (13.0, 5.01),
    ] {
        let before = std::fs::read(&valid_path).unwrap();
        assert!(Config::persist_font_runtime_values(&valid_path, size, weight).is_err());
        assert_eq!(std::fs::read(&valid_path).unwrap(), before);
    }
    for path in [&malformed_path, &non_table_path, &wrong_size_path, &wrong_weight_path] {
        let before = std::fs::read(path).unwrap();
        assert!(Config::persist_font_runtime_values(path, 14.0, 1.2).is_err());
        assert_eq!(std::fs::read(path).unwrap(), before);
    }
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn persist_font_runtime_values_rejects_busy_process_and_cross_process_locks() {
    // Contract: both in-process and filesystem locks prevent concurrent config replacement.
    let dir = font_persist_test_dir("busy-lock");
    let path = dir.join("sonicterm.toml");
    let original = b"[font]\nsize = 13\nweight_scale = 1\n";
    std::fs::write(&path, original).unwrap();

    let process_guard = ProcessSavePathGuard::acquire(&path).unwrap();
    assert!(Config::persist_font_runtime_values(&path, 14.0, 2.0).is_err());
    drop(process_guard);
    assert_eq!(std::fs::read(&path).unwrap(), original);

    let mut lock_name = path.file_name().unwrap().to_os_string();
    lock_name.push(".save.lock");
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(dir.join(lock_name))
        .unwrap();
    lock.lock().unwrap();
    assert!(Config::persist_font_runtime_values(&path, 14.0, 2.0).is_err());
    lock.unlock().unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), original);

    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn persist_font_runtime_values_rejects_a_concurrent_disk_change() {
    // Contract: compare-before-replace preserves a concurrent writer and removes staged files.
    let dir = font_persist_test_dir("concurrent-change");
    let path = dir.join("sonicterm.toml");
    let original = b"theme = \"before\"\n[font]\nsize = 13\nweight_scale = 1\n";
    let concurrent = b"theme = \"after\"\n[font]\nsize = 13\nweight_scale = 1\n";
    std::fs::write(&path, original).unwrap();

    let result =
        Config::persist_font_runtime_values_before_commit(&path, 14.0, 2.0, |destination| {
            std::fs::write(destination, concurrent)
                .with_context(|| format!("write concurrent config at {destination:?}"))
        });

    assert!(result.is_err());
    assert_eq!(std::fs::read(&path).unwrap(), concurrent);
    let staged = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_name().to_string_lossy().ends_with(".tmp"))
        .collect::<Vec<_>>();
    assert!(staged.is_empty(), "conflicted save left staged temp files: {staged:?}");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn persist_font_runtime_values_is_idempotent_and_replaces_existing_destination() {
    // Contract: repeated saves are byte-idempotent while replacing an existing destination.
    let dir = font_persist_test_dir("repeat");
    let path = dir.join("sonicterm.toml");
    std::fs::write(&path, "theme = \"keep\"\n[font]\nsize = 13.0\nweight_scale = 1.0\n").unwrap();

    Config::persist_font_runtime_values(&path, 13.5, 1.5).unwrap();
    let once = std::fs::read(&path).unwrap();
    Config::persist_font_runtime_values(&path, 13.5, 1.5).unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), once);
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "theme = \"keep\"\n[font]\nsize = 13.5\nweight_scale = 1.5\n"
    );
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn persist_font_runtime_values_retains_crlf_and_rejects_mixed_endings() {
    // Contract: homogeneous line endings survive, while ambiguous mixed documents stay untouched.
    let dir = font_persist_test_dir("line-endings");
    let crlf_path = dir.join("crlf.toml");
    let mixed_path = dir.join("mixed.toml");
    let multiline_mixed_path = dir.join("multiline-mixed.toml");
    let escaped_crlf_path = dir.join("escaped-crlf.toml");
    std::fs::write(&crlf_path, b"# keep\r\n[font]\r\nsize = 13\r\nweight_scale = 1\r\n").unwrap();
    std::fs::write(&mixed_path, b"[font]\r\nsize = 13\nweight_scale = 1\r\n").unwrap();
    std::fs::write(
        &multiline_mixed_path,
        b"future = \"\"\"a\nb\"\"\"\r\n[font]\r\nsize = 13\r\nweight_scale = 1\r\n",
    )
    .unwrap();
    std::fs::write(
        &escaped_crlf_path,
        b"theme = \"line\\r\\nvalue\"\n[font]\nsize = 13\nweight_scale = 1\n",
    )
    .unwrap();

    Config::persist_font_runtime_values(&crlf_path, 14.0, 2.0).unwrap();
    let crlf = std::fs::read(&crlf_path).unwrap();
    assert_eq!(crlf, b"# keep\r\n[font]\r\nsize = 14\r\nweight_scale = 2\r\n");
    Config::persist_font_runtime_values(&escaped_crlf_path, 14.0, 2.0).unwrap();
    assert_eq!(
        std::fs::read(&escaped_crlf_path).unwrap(),
        b"theme = \"line\\r\\nvalue\"\n[font]\nsize = 14\nweight_scale = 2\n"
    );
    for path in [&mixed_path, &multiline_mixed_path] {
        let before = std::fs::read(path).unwrap();
        assert!(Config::persist_font_runtime_values(path, 14.0, 2.0).is_err());
        assert_eq!(std::fs::read(path).unwrap(), before);
    }
    let _ = std::fs::remove_dir_all(dir);
}

#[cfg(windows)]
#[test]
#[allow(clippy::permissions_set_readonly_false)]
fn failed_read_only_save_removes_the_staged_temp_file() {
    // Contract: a Windows read-only replacement failure leaves no staged temporary file.
    let dir = font_persist_test_dir("readonly-cleanup");
    let path = dir.join("sonicterm.toml");
    let original = b"[font]\nsize = 13\nweight_scale = 1\n";
    std::fs::write(&path, original).unwrap();
    let mut permissions = std::fs::metadata(&path).unwrap().permissions();
    permissions.set_readonly(true);
    std::fs::set_permissions(&path, permissions).unwrap();

    assert!(Config::persist_font_runtime_values(&path, 14.0, 2.0).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), original);
    let staged = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_name().to_string_lossy().ends_with(".tmp"))
        .collect::<Vec<_>>();
    assert!(staged.is_empty(), "failed save left staged temp files: {staged:?}");

    let mut permissions = std::fs::metadata(&path).unwrap().permissions();
    permissions.set_readonly(false);
    std::fs::set_permissions(&path, permissions).unwrap();
    let _ = std::fs::remove_dir_all(dir);
}

#[cfg(unix)]
#[test]
fn persist_font_runtime_values_follows_symlink_and_preserves_permissions() {
    // Contract: Unix saves update the symlink target without replacing the link or its mode.
    use std::os::unix::fs::{symlink, PermissionsExt};

    let dir = font_persist_test_dir("symlink");
    let target = dir.join("target.toml");
    let link = dir.join("link.toml");
    std::fs::write(&target, "[font]\nsize = 13\nweight_scale = 1\n").unwrap();
    std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o640)).unwrap();
    symlink(&target, &link).unwrap();

    Config::persist_font_runtime_values(&link, 16.0, 3.0).unwrap();

    assert!(std::fs::symlink_metadata(&link).unwrap().file_type().is_symlink());
    assert_eq!(std::fs::metadata(&target).unwrap().permissions().mode() & 0o777, 0o640);
    let cfg = Config::load_strict(&target).unwrap();
    assert_eq!(cfg.font.size, 16.0);
    assert_eq!(cfg.font.weight_scale, 3.0);
    let _ = std::fs::remove_dir_all(dir);
}
