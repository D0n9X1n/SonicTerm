use super::*;

#[test]
fn token_elevation_maps_zero_and_nonzero_values() {
    // Protect the Win32 DWORD contract without assuming elevation is encoded as exactly one.
    assert_eq!(process_privilege_from_token_elevation(0), ProcessPrivilege::Unprivileged);
    assert_eq!(process_privilege_from_token_elevation(1), ProcessPrivilege::Privileged);
    assert_eq!(process_privilege_from_token_elevation(u32::MAX), ProcessPrivilege::Privileged);
}

#[test]
fn current_process_token_probe_returns_an_observed_classification() {
    // Exercise the real TOKEN_QUERY boundary while remaining valid on elevated and ordinary CI runners.
    assert!(matches!(
        detect_process_privilege().expect("current process token must be queryable"),
        ProcessPrivilege::Unprivileged | ProcessPrivilege::Privileged
    ));
}
