use super::*;

#[test]
fn file_drop_without_event_loop_is_not_retained_or_acknowledged() {
    // A refused native drop must not become delayed input after a later event-loop installation.
    assert!(!push_files(WindowId::from(41), vec![PathBuf::from("example.txt")]));
    assert!(drain_file_drops().is_empty());
    assert!(!push_files(WindowId::from(42), Vec::new()));
    assert!(drain_file_drops().is_empty());
}
