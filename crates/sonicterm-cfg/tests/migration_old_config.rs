//! R3 migration: legacy `Sonic/sonic.toml` -> new `SonicTerm/sonicterm.toml`.

use sonicterm_cfg::config::migrate_legacy_config;

#[test]
fn migrates_legacy_when_new_absent() {
    let tmp = tempfile::tempdir().unwrap();
    let legacy_dir = tmp.path().join("Sonic");
    let new_dir = tmp.path().join("SonicTerm");
    std::fs::create_dir_all(&legacy_dir).unwrap();
    let legacy = legacy_dir.join("sonic.toml");
    let new = new_dir.join("sonicterm.toml");
    let body = "# legacy user config\nscrollback = 12345\n";
    std::fs::write(&legacy, body).unwrap();

    let migrated = migrate_legacy_config(Some(&legacy), Some(&new)).unwrap();
    assert!(migrated, "first run with legacy present should migrate");
    assert!(new.exists(), "new config should exist after migration");
    assert_eq!(std::fs::read_to_string(&new).unwrap(), body);
    assert!(legacy.exists(), "legacy file must be left intact for safety");
    assert_eq!(std::fs::read_to_string(&legacy).unwrap(), body);
}

#[test]
fn noop_when_new_already_exists() {
    let tmp = tempfile::tempdir().unwrap();
    let legacy_dir = tmp.path().join("Sonic");
    let new_dir = tmp.path().join("SonicTerm");
    std::fs::create_dir_all(&legacy_dir).unwrap();
    std::fs::create_dir_all(&new_dir).unwrap();
    let legacy = legacy_dir.join("sonic.toml");
    let new = new_dir.join("sonicterm.toml");
    std::fs::write(&legacy, "old\n").unwrap();
    std::fs::write(&new, "new\n").unwrap();

    let migrated = migrate_legacy_config(Some(&legacy), Some(&new)).unwrap();
    assert!(!migrated, "should not overwrite existing new config");
    assert_eq!(std::fs::read_to_string(&new).unwrap(), "new\n");
}

#[test]
fn noop_when_legacy_absent() {
    let tmp = tempfile::tempdir().unwrap();
    let legacy = tmp.path().join("Sonic/sonic.toml");
    let new = tmp.path().join("SonicTerm/sonicterm.toml");

    let migrated = migrate_legacy_config(Some(&legacy), Some(&new)).unwrap();
    assert!(!migrated);
    assert!(!new.exists());
}
