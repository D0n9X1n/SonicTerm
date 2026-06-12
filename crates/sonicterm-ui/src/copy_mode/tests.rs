
use super::*;

#[test]
fn read_only_copy_mode_does_not_select() {
    let mut state = CopyModeState::read_only_at((1, 1));
    assert!(state.is_read_only());
    state.start_select();
    assert_eq!(state.selected_range(), None);
}
