use sonicterm_ui::tabbar_view::{Point, Rect, TabAction, TabHover, TabWidget};

fn widget() -> TabWidget {
    TabWidget {
        idx: 2,
        bg_rect: Rect { x: 10.0, y: 4.0, w: 120.0, h: 32.0 },
        close_x_rect: Rect { x: 110.0, y: 13.0, w: 14.0, h: 14.0 },
        title_rect: Rect { x: 20.0, y: 4.0, w: 82.0, h: 32.0 },
        title: "tab".to_string(),
        active: false,
        hover: TabHover::None,
        index: 2,
        bg: Rect { x: 10.0, y: 4.0, w: 120.0, h: 32.0 },
        close: Rect { x: 110.0, y: 13.0, w: 14.0, h: 14.0 },
    }
}

#[test]
fn center_of_bg_outside_close_activates_tab() {
    assert_eq!(widget().hit(Point { x: 60.0, y: 20.0 }), Some(TabAction::Activate(2)));
}

#[test]
fn close_sub_rect_closes_tab() {
    assert_eq!(widget().hit(Point { x: 117.0, y: 20.0 }), Some(TabAction::Close(2)));
}

#[test]
fn outside_bg_misses() {
    assert_eq!(widget().hit(Point { x: 9.0, y: 20.0 }), None);
}
