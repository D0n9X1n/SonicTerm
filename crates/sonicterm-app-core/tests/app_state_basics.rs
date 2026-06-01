use sonicterm_app_core::{AppIntent, AppState, RedrawReason};
use sonicterm_types::Action;

#[test]
fn builder_sets_initial_grid() {
    let s = AppState::builder().with_grid(80, 24).build();
    assert_eq!(s.cols, 80);
    assert_eq!(s.rows, 24);
}

#[test]
fn queue_and_drain_intents_fifo() {
    let mut s = AppState::default();
    s.queue(AppIntent::Redraw(RedrawReason::PtyBytes));
    s.queue(AppIntent::DispatchAction(Action::CloseTab));
    s.queue(AppIntent::Quit);
    let drained = s.drain_intents();
    assert_eq!(drained.len(), 3);
    assert!(matches!(drained[0], AppIntent::Redraw(RedrawReason::PtyBytes)));
    assert!(matches!(drained[2], AppIntent::Quit));
    assert!(s.drain_intents().is_empty());
}

#[test]
fn redraw_reason_is_copy() {
    fn assert_copy<T: Copy>() {}
    assert_copy::<RedrawReason>();
}
