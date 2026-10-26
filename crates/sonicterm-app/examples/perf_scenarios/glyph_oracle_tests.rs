use super::*;

// Artifact rows cannot silently substitute another fixture, strike, font, schema, or empty set.
#[test]
fn row_binding_rejects_wrong_inputs_and_incomplete_tiles() {
    let row = Row {
        schema: 1,
        fixture: "S9".into(),
        dpi: 72,
        family: crate::workload::FONT_FAMILY.into(),
        size: crate::workload::FONT_SIZE.into(),
        tiles: BTreeMap::from([("face".into(), [24, 16])]),
    };
    let bytes = serde_json::to_vec(&row).unwrap();
    assert_eq!(decode(&bytes, "S9", 72).unwrap()["face"], [24, 16]);
    assert!(decode(&bytes, "S12", 72).is_err());
    assert!(decode(&bytes, "S9", 144).is_err());
    for (field, replacement) in [
        ("schema", serde_json::json!(2)),
        ("family", serde_json::json!("other")),
        ("size", serde_json::json!("14.0")),
        ("tiles", serde_json::json!({})),
        ("tiles", serde_json::json!({"face": [0, 16]})),
    ] {
        let mut value = serde_json::to_value(&row).unwrap();
        value[field] = replacement;
        assert!(decode(&serde_json::to_vec(&value).unwrap(), "S9", 72).is_err());
    }
}

// Consumers verify both layers again at use time, so a file changed after workflow verification fails.
#[test]
fn artifact_reader_rejects_changed_manifest_payload_and_missing_rows() {
    let directory = tempfile::tempdir().unwrap();
    let row = Row {
        schema: 1,
        fixture: "S9".into(),
        dpi: 72,
        family: crate::workload::FONT_FAMILY.into(),
        size: crate::workload::FONT_SIZE.into(),
        tiles: BTreeMap::from([("face".into(), [24, 16])]),
    };
    let bytes = serde_json::to_vec(&row).unwrap();
    let manifest =
        serde_json::to_vec(&serde_json::json!({"files": {"S9-72.json": sha256_hex(&bytes)}}))
            .unwrap();
    let digest = sha256_hex(&manifest);
    let manifest_path = directory.path().join("manifest.json");
    let payload = directory.path().join("S9-72.json");
    std::fs::write(&manifest_path, &manifest).unwrap();
    std::fs::write(&payload, &bytes).unwrap();
    assert_eq!(load_verified(directory.path(), &digest, "S9", 72).unwrap()["face"], [24, 16]);
    assert!(load_verified(directory.path(), &digest, "S12", 72).is_err());
    std::fs::write(&payload, b"{}").unwrap();
    assert!(load_verified(directory.path(), &digest, "S9", 72).is_err());
    std::fs::write(&payload, &bytes).unwrap();
    std::fs::write(&manifest_path, b"{}").unwrap();
    assert!(load_verified(directory.path(), &digest, "S9", 72).is_err());
}

// Equivalent font files survive runner-local paths; changed bytes cannot retain an oracle identity.
#[test]
fn portable_font_sources_hash_files_and_preserve_bound_gdi_names() {
    let directory = tempfile::tempdir().unwrap();
    let first = directory.path().join("first.ttf");
    let second = directory.path().join("second.ttf");
    std::fs::write(&first, b"font bytes").unwrap();
    std::fs::write(&second, b"font bytes").unwrap();
    let original = portable_source(first.to_str().unwrap()).unwrap();
    assert_eq!(original, portable_source(second.to_str().unwrap()).unwrap());
    std::fs::write(&second, b"different font").unwrap();
    assert_ne!(original, portable_source(second.to_str().unwrap()).unwrap());
    assert_eq!(portable_source("memory:Segoe UI Emoji").unwrap(), "memory:Segoe UI Emoji");
    assert_eq!(portable_source("builtin:Symbols").unwrap(), "builtin:Symbols");
}
