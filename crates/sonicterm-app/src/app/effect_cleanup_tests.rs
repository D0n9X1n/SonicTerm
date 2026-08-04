use super::*;
use enum_map::enum_map;

fn test_governor() -> ResourceGovernor {
    ResourceGovernor::new(
        ProcessKind::Gui,
        GovernorLimits {
            process_bytes: usize::MAX,
            class_bytes: enum_map! { _ => usize::MAX },
            class_items: enum_map! { _ => None },
        },
    )
    .unwrap()
}

fn owner_limits() -> OwnerLimits {
    OwnerLimits {
        owner_bytes: usize::MAX,
        class_bytes: enum_map! { _ => usize::MAX },
        class_items: enum_map! { _ => None },
    }
}

#[test]
fn open_url_effect_delegates_validation_to_the_real_opener() {
    let error = open_url_effect("invalid:anything").expect_err("unsupported scheme must fail");
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
    assert_eq!(error.to_string(), "scheme not allowed");
}

#[test]
fn open_url_effect_logs_real_errors_without_revalidating() {
    const SOURCE: &str = include_str!("mod.rs");
    assert!(SOURCE.contains("if let Err(error) = open_url_effect(&url)"));
    assert!(SOURCE.contains("tracing::warn!(%error, \"failed to open URL effect\")"));
    assert!(!SOURCE.contains("tracing::warn!(%error, %url"));
    assert!(!SOURCE.contains("url_open::validate(&url)"));
    assert!(!SOURCE.contains("let _ = sonicterm_cfg::url_open::open"));
}

#[test]
fn synthetic_window_ids_use_winit_safe_conversion() {
    const SOURCE: &str = include_str!("mod.rs");

    assert!(!SOURCE.contains("transmute::<u64, WindowId>"));
    assert!(SOURCE.contains("WindowId::from(u64::MAX - tag)"));
    assert!(SOURCE.contains("WindowId::from(u64::MAX)"));
}

#[test]
fn windows_native_background_parser_rejects_non_ascii_byte_slices() {
    const SOURCE: &str = include_str!("mod.rs");

    assert!(SOURCE.contains("if h.len() != 6 || !h.is_ascii()"));
    assert!(SOURCE.contains("not exactly six ASCII bytes"));
}

#[test]
fn owner_cleanup_returns_success_and_governor_refusals() {
    let governor = test_governor();
    let clean =
        governor.create_child(governor.root_owner(), OwnerKind::Window, owner_limits()).unwrap();
    close_owner(&governor, clean).unwrap();
    assert!(governor.snapshot(clean).is_err());

    let charged =
        governor.create_child(governor.root_owner(), OwnerKind::Window, owner_limits()).unwrap();
    let reservation = governor
        .try_reserve(
            charged,
            ResourceClass::GridVisible,
            sonicterm_types::ResourceAmount { bytes: 1, items: 1 },
        )
        .unwrap();

    let error = close_owner(&governor, charged).expect_err("live charges must remain observable");
    assert!(
        matches!(error, sonicterm_types::BudgetError::OwnerHasLiveCharges { owner, .. } if owner == charged)
    );
    drop(reservation);
}
