use super::*;

/// The DMG script that writes the bundle's Info.plist.
const MACOS_DMG_SCRIPT: &str = include_str!("../../../scripts/make-macos-dmg.sh");

/// The Info.plist heredoc from `make-macos-dmg.sh`, with a version filled in.
fn shipped_info_plist() -> String {
    let normalized = MACOS_DMG_SCRIPT.replace("\r\n", "\n");
    let marker = "cat > \"$APP/Contents/Info.plist\" <<PLIST\n";
    let start = normalized.find(marker).expect("Info.plist heredoc") + marker.len();
    let remainder = &normalized[start..];
    let end = remainder.find("\nPLIST\n").expect("Info.plist heredoc terminator");
    remainder[..end].replace("${VERSION}", "1.0.0")
}

/// Facts for the installed, registered bundle; tests change one field at a time.
fn installed_facts() -> BundleFacts {
    BundleFacts {
        is_app_bundle: true,
        identifier: Some(BUNDLE_IDENTIFIER.to_string()),
        read_only_volume: false,
        translocated: false,
        registered: true,
    }
}

/// Contract: the confirmation list is exactly what the shipped Info.plist claims: three
/// content types, then the two terminal-link schemes.
#[test]
fn the_plan_lists_exactly_the_shipped_claims() {
    let claims = claims_from_info_plist(&shipped_info_plist()).unwrap();
    assert_eq!(claims.identifier.as_deref(), Some(BUNDLE_IDENTIFIER));
    assert_eq!(
        claims.content_types,
        vec!["public.shell-script", "com.apple.terminal.shell-script", "public.unix-executable"]
    );
    assert_eq!(claims.schemes, vec!["ssh", "x-man-page"]);
    let plan = plan_for_claims(&claims).unwrap();
    assert_eq!(plan.mode, PlanMode::Change);
    assert_eq!(
        plan.items.iter().map(HandlerItem::describe).collect::<Vec<_>>(),
        vec![
            "Content type public.shell-script",
            "Content type com.apple.terminal.shell-script",
            "Content type public.unix-executable",
            "URL scheme ssh:",
            "URL scheme x-man-page:",
        ]
    );
}

/// Contract: repeated claims are listed once, and a plist with no claims, no dictionary or
/// broken XML refuses rather than producing an empty or wrong plan.
#[test]
fn claims_deduplicate_and_bad_plists_refuse() {
    let repeated = "<plist><dict><key>CFBundleDocumentTypes</key><array>\
        <dict><key>LSItemContentTypes</key><array><string>a</string><string>a</string></array></dict>\
        <dict><key>LSItemContentTypes</key><array><string>a</string><string>b</string></array></dict>\
        </array></dict></plist>";
    assert_eq!(claims_from_info_plist(repeated).unwrap().content_types, vec!["a", "b"]);
    let empty = claims_from_info_plist("<plist><dict></dict></plist>").unwrap();
    assert_eq!(empty, BundleClaims::default());
    assert!(plan_for_claims(&empty).unwrap_err().contains("claims no"));
    assert!(claims_from_info_plist("<plist></plist>").unwrap_err().contains("no top-level"));
    assert!(claims_from_info_plist("<plist").unwrap_err().contains("not readable"));
}

/// Contract: only the installed, registered com.d0n9x1n.sonicterm bundle may change defaults;
/// a non-bundle build, a wrong identifier, a translocated or disk-image copy, and an
/// unregistered copy each refuse with their own reason.
#[test]
fn only_the_installed_registered_bundle_may_change_defaults() {
    assert_eq!(bundle_refusal(&installed_facts()), None);
    let cargo_run = BundleFacts { is_app_bundle: false, ..installed_facts() };
    assert!(bundle_refusal(&cargo_run).unwrap().starts_with("Missing bundle"));
    let other = BundleFacts { identifier: Some("com.example".into()), ..installed_facts() };
    assert!(bundle_refusal(&other).unwrap().starts_with("Missing bundle"));
    for facts in [
        BundleFacts { translocated: true, ..installed_facts() },
        BundleFacts { read_only_volume: true, ..installed_facts() },
    ] {
        assert!(bundle_refusal(&facts).unwrap().contains("Move SonicTerm.app to /Applications"));
    }
    let unregistered = BundleFacts { registered: false, ..installed_facts() };
    assert!(bundle_refusal(&unregistered).unwrap().contains("not registered"));
}

/// Contract: translocation is read from the path alone, and registration matches the exact
/// bundle path, ignoring a trailing slash.
#[test]
fn translocation_and_registration_are_matched_by_path() {
    assert!(is_translocated_path(
        "/private/var/folders/xy/T/AppTranslocation/0A1B/d/SonicTerm.app"
    ));
    assert!(!is_translocated_path("/Applications/SonicTerm.app"));
    let registered = vec!["/Applications/SonicTerm.app/".to_string()];
    assert!(is_registered("/Applications/SonicTerm.app", &registered));
    assert!(!is_registered("/Volumes/SonicTerm/SonicTerm.app", &registered));
}

/// Contract: success is changed, a cancellation code is cancelled, a permission code is
/// denied, and any other error is failed with its domain and code.
#[test]
fn completion_errors_map_to_outcomes() {
    assert_eq!(outcome_for_error(None), HandlerOutcome::Changed);
    assert_eq!(outcome_for_error(Some(("NSCocoaErrorDomain", 3072))), HandlerOutcome::Cancelled);
    assert_eq!(outcome_for_error(Some(("NSOSStatusErrorDomain", -128))), HandlerOutcome::Cancelled);
    assert_eq!(outcome_for_error(Some(("NSOSStatusErrorDomain", -54))), HandlerOutcome::Denied);
    assert_eq!(
        outcome_for_error(Some(("NSOSStatusErrorDomain", -10814))),
        HandlerOutcome::Failed("NSOSStatusErrorDomain -10814".into())
    );
}

/// Contract: a writable temporary directory is not reported as a read-only volume, and the
/// sealed system volume is.
#[cfg(target_os = "macos")]
#[test]
fn read_only_volumes_are_detected() {
    let scratch = tempfile::tempdir().unwrap();
    assert!(!is_read_only_volume(scratch.path()));
    assert!(is_read_only_volume(std::path::Path::new("/System")));
    assert!(!is_read_only_volume(std::path::Path::new("/no/such/path")));
}

/// Contract: run from the test binary (not SonicTerm.app), the backend refuses as a missing
/// bundle and changes nothing; this is the `cargo run` case.
#[cfg(target_os = "macos")]
#[test]
fn a_non_bundle_build_refuses_as_missing_bundle() {
    use sonicterm_app::default_handlers::DefaultHandlerBackend;
    let mut backend = MacDefaultHandlers::default();
    assert_eq!(backend.platform(), "macos");
    assert!(backend.plan().unwrap_err().starts_with("Missing bundle"));
}

/// Contract: the UTType class the content-type setter needs resolves at runtime, and it knows
/// every claimed content type.
#[cfg(target_os = "macos")]
#[test]
fn claimed_content_types_resolve_to_ut_types() {
    assert!(native::ut_type_class_present());
    for content_type in claims_from_info_plist(&shipped_info_plist()).unwrap().content_types {
        assert!(native::content_type(&content_type).is_some(), "{content_type}");
    }
}

/// Manual check, read only: after Apply on an installed build, the Shell role for
/// `public.unix-executable` should resolve to SonicTerm. Run with `--ignored --nocapture`.
#[cfg(target_os = "macos")]
#[test]
#[ignore = "reads this Mac's Launch Services state; run by hand after Apply"]
fn shell_role_handler_for_unix_executables() {
    let handler = native::shell_role_handler("public.unix-executable");
    println!("Shell role handler for public.unix-executable: {handler:?}");
}
