use sonic_shared::tabs::*;

#[test]
fn push_and_activate() {
    let mut bar = TabBar::new();
    let a = bar.push(Tab::new("A"));
    let _b = bar.push(Tab::new("B"));
    assert_eq!(bar.len(), 2);
    assert_eq!(bar.active().unwrap().title, "B");
    bar.activate(0);
    assert_eq!(bar.active().unwrap().id, a);
}

#[test]
fn close_shifts_active() {
    let mut bar = TabBar::new();
    bar.push(Tab::new("A"));
    let b = bar.push(Tab::new("B"));
    bar.close(b);
    assert_eq!(bar.active().unwrap().title, "A");
}

#[test]
fn reorder_moves_tabs() {
    let mut bar = TabBar::new();
    let a = bar.push(Tab::new("A"));
    let _b = bar.push(Tab::new("B"));
    let _c = bar.push(Tab::new("C"));
    bar.reorder(0, 2);
    assert_eq!(bar.tabs()[2].id, a);
}

#[test]
fn detach_removes_and_returns_tab() {
    let mut bar = TabBar::new();
    let a = bar.push(Tab::new("A"));
    let b = bar.push(Tab::new("B"));
    let taken = bar.detach(a).expect("detached");
    assert_eq!(taken.id, a);
    assert_eq!(bar.len(), 1);
    assert_eq!(bar.active().unwrap().id, b);
}

#[test]
fn next_and_prev_wrap_around() {
    let mut bar = TabBar::new();
    bar.push(Tab::new("A"));
    bar.push(Tab::new("B"));
    bar.push(Tab::new("C"));
    bar.activate(2);
    bar.next(); // wraps to 0
    assert_eq!(bar.active_index(), 0);
    bar.prev(); // wraps back to 2
    assert_eq!(bar.active_index(), 2);
}

#[test]
fn close_last_tab_leaves_empty_bar() {
    let mut bar = TabBar::new();
    let a = bar.push(Tab::new("A"));
    bar.close(a);
    assert!(bar.is_empty());
    assert!(bar.active().is_none());
}

#[test]
fn tab_ids_are_unique() {
    let a = Tab::new("x");
    let b = Tab::new("y");
    assert_ne!(a.id, b.id);
}
