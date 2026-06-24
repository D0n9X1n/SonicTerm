use super::NATIVE_WINDOW_TITLE;

#[test]
fn native_window_title_is_static_app_name() {
    assert_eq!(NATIVE_WINDOW_TITLE, "SonicTerm");
}

