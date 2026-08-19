use super::{LINUX_DESKTOP_ID, LINUX_INSTANCE_NAME, NATIVE_WINDOW_TITLE};

#[test]
fn native_window_title_is_static_app_name() {
    assert_eq!(NATIVE_WINDOW_TITLE, "SonicTerm");
}

#[test]
fn linux_window_identity_matches_packaged_desktop_metadata() {
    // Protect X11 WM_CLASS and Wayland app ID from drifting away from package metadata.
    assert_eq!(LINUX_DESKTOP_ID, "com.d0n9x1n.SonicTerm");
    assert_eq!(LINUX_INSTANCE_NAME, "sonicterm");
}
