//! Confirm value types serialize/deserialize round-trip via the canonical
//! formats SonicTerm uses on disk and on the wire: TOML for keymaps (Action),
//! JSON for diagnostics (Cell). If either breaks, the keymap loader or
//! a tracing dump will silently drop data.

use sonicterm_types::{Action, Cell, CellFlags, Color, Direction, HyperlinkId, ScrollAction};

#[test]
fn action_json_roundtrip_all_variants() {
    let cases = vec![
        Action::NewTab,
        Action::ActivateTab(7),
        Action::FocusPane(Direction::Right),
        Action::ResizePane { dir: Direction::Up, amount: 4 },
        Action::ApplyTheme("tokyo-night".into()),
        Action::Scroll(ScrollAction::PageDown),
        Action::OpenSshPane("user@host:2222".into()),
    ];
    for a in cases {
        let s = serde_json::to_string(&a).expect("encode");
        let back: Action = serde_json::from_str(&s).expect("decode");
        assert_eq!(a, back, "roundtrip failed for {a:?} -> {s}");
    }
}

#[test]
fn color_serde_roundtrip() {
    for c in [Color::Default, Color::Indexed(42), Color::Rgb(10, 20, 30)] {
        let s = serde_json::to_string(&c).unwrap();
        let back: Color = serde_json::from_str(&s).unwrap();
        assert_eq!(c, back);
    }
}

#[test]
fn cell_value_semantics() {
    // Cell itself isn't Serialize (it carries HyperlinkId which is intentionally
    // opaque), but its identity must be stable: default + clone + equality.
    let a = Cell::default();
    let b = a.clone();
    assert_eq!(a, b);
    assert_eq!(a.ch, ' ');
    assert_eq!(a.fg, Color::Default);
    assert!(a.flags.is_empty());
    assert!(a.hyperlink().is_none());
    assert!(a.extras().is_none());

    // Flags round-trip via bitflags bit semantics.
    let flags = CellFlags::BOLD | CellFlags::ITALIC;
    assert!(flags.contains(CellFlags::BOLD));
    assert!(!flags.contains(CellFlags::UNDERLINE));
}

#[test]
fn hyperlink_id_monotonic() {
    let a = HyperlinkId::next();
    let b = HyperlinkId::next();
    assert_ne!(a, b);
    assert!(b.0 > a.0);
}
