use sonic_cfg::theme::Theme;

#[test]
fn import_rejects_malformed_theme() {
    let temp = tempfile::tempdir().expect("tempdir");
    let malformed = temp.path().join("bad.toml");
    std::fs::write(&malformed, "name = [not valid toml").expect("write malformed theme");

    let err = Theme::import_from_file(&malformed, &temp.path().join("themes"))
        .expect_err("malformed theme should fail");
    assert!(err.to_string().contains("parse"), "unexpected error: {err:#}");
}
