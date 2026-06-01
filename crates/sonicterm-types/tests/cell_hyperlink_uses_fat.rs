//! Epic #300 P1: hyperlink writes materialize FatAttributes; clearing
//! collapses back to `None`.

use sonicterm_types::hyperlink_id::HyperlinkId;
use sonicterm_types::{Cell, CellFlags, Color};

#[test]
fn no_hyperlink_means_no_fat() {
    let c = Cell::plain('a', Color::Default, Color::Default, CellFlags::empty());
    assert!(c.hyperlink().is_none());
    assert!(!c.has_fat(), "absence of rare attrs must not materialize FatAttributes");
}

#[test]
fn setting_hyperlink_materializes_fat() {
    let mut c = Cell::default();
    assert!(!c.has_fat());
    c.set_hyperlink(Some(HyperlinkId(42)));
    assert!(c.has_fat(), "hyperlink write must allocate FatAttributes");
    assert_eq!(c.hyperlink(), Some(HyperlinkId(42)));
}

#[test]
fn clearing_only_hyperlink_collapses_fat() {
    let mut c = Cell::default();
    c.set_hyperlink(Some(HyperlinkId(7)));
    assert!(c.has_fat());
    c.set_hyperlink(None);
    assert!(!c.has_fat(), "clearing only rare attr must collapse FatAttributes back to None");
    assert_eq!(c.hyperlink(), None);
}

#[test]
fn extras_and_hyperlink_share_one_box() {
    let mut c = Cell::default();
    c.set_extras(Some("zw".to_string().into_boxed_str()));
    c.set_hyperlink(Some(HyperlinkId(1)));
    assert!(c.has_fat());
    // Clearing one does NOT collapse — the other still lives.
    c.set_hyperlink(None);
    assert!(c.has_fat(), "extras still present; FatAttributes must stay");
    // Clearing the last one collapses.
    let _ = c.take_extras();
    assert!(!c.has_fat());
}
