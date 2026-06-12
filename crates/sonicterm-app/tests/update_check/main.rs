use sonicterm_app::app::update_check::{
    latest_release_from_json, version_is_newer, UpdateCheckResult,
};

#[test]
fn version_compare_accepts_v_prefixed_semver_tags() {
    assert!(version_is_newer("0.10.2", "v0.10.3"));
    assert!(!version_is_newer("0.10.3", "v0.10.3"));
    assert!(!version_is_newer("0.10.4", "v0.10.3"));
}

#[test]
fn latest_release_ignores_drafts_and_prereleases() {
    let json = r#"
    [
      {"tag_name":"v0.11.0-beta.1","html_url":"https://example.invalid/beta","draft":false,"prerelease":true},
      {"tag_name":"v0.10.4","html_url":"https://example.invalid/draft","draft":true,"prerelease":false},
      {"tag_name":"v0.10.3","html_url":"https://example.invalid/stable","draft":false,"prerelease":false}
    ]
    "#;

    assert_eq!(
        latest_release_from_json("0.10.2", json),
        Some(UpdateCheckResult::Newer {
            tag: "v0.10.3".into(),
            url: "https://example.invalid/stable".into(),
        })
    );
}

#[test]
fn latest_release_reports_up_to_date_for_current_version() {
    let json = r#"[{"tag_name":"v0.10.3","html_url":"https://example.invalid/stable"}]"#;

    assert_eq!(latest_release_from_json("0.10.3", json), Some(UpdateCheckResult::UpToDate));
}
