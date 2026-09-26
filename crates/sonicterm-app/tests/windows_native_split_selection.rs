#![cfg(target_os = "windows")]

mod native_split_selection;

#[test]
fn native_split_drag_copies_only_the_press_pane() {
    // Real Windows surfaces exercise shared App pointer routing and copy without touching the system clipboard.
    let nonce =
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    let scratch = std::env::temp_dir()
        .join(format!("sonicterm-native-selection-{}-{nonce}", std::process::id()));
    native_split_selection::run(&scratch).unwrap();
}
