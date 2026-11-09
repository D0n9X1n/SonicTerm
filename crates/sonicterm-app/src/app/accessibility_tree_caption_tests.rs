//! Self-drawn caption buttons in the accessibility tree.

use super::*;
use sonicterm_ui::window_band::CaptionButton;

/// Tree inputs with no strip and the three caption buttons.
fn caption_inputs() -> TreeInputs<'static> {
    TreeInputs {
        title: "#1 Work".to_owned(),
        strip_label: "Status line".to_owned(),
        announcements_label: "Announcements".to_owned(),
        strip: None,
        announcement_bounds: rect(0.0, 0.0, 10.0, 1.0),
        announcement: None,
        focus: None,
        caption: vec![
            (CaptionButton::Minimize, "Minimize".to_owned(), rect(862.0, 0.0, 46.0, 32.0)),
            (CaptionButton::Maximize, "Restore".to_owned(), rect(908.0, 0.0, 46.0, 32.0)),
            (CaptionButton::Close, "Close".to_owned(), rect(954.0, 0.0, 46.0, 32.0)),
        ],
        name_band: Some(("Window: #1 Work".to_owned(), rect(400.0, 0.0, 120.0, 32.0), true)),
    }
}

/// The caption buttons replace native controls, so each is a named, pressable button with its
/// bounds even with no status line, under ids that never change and map back to the button.
#[test]
fn caption_buttons_are_named_pressable_buttons_without_a_strip() {
    let tree = build_tree(&caption_inputs());
    let root = tree.nodes.iter().find(|(id, _)| *id == ROOT_NODE).unwrap();
    for (button, name) in [
        (CaptionButton::Minimize, "Minimize"),
        (CaptionButton::Maximize, "Restore"),
        (CaptionButton::Close, "Close"),
    ] {
        let id = caption_node(button);
        assert!(root.1.children().contains(&id), "{button:?} is a child of the window");
        let node = &tree.nodes.iter().find(|(node_id, _)| *node_id == id).unwrap().1;
        assert_eq!((node.role(), node.label()), (Role::Button, Some(name)));
        assert!(node.supports_action(AccessAction::Click));
        assert!(node.bounds().is_some());
        assert_eq!(caption_for_node(id.0), Some(button));
    }
    assert!(!root.1.children().contains(&STRIP_NODE), "no strip, no status line group");
    assert_eq!(caption_for_node(segment_node(StatusSegmentId::Mem).0), None);
}

/// A separate name band is a named button a reader can press to rename the window; without one the
/// tree has no such node.
#[test]
fn the_name_band_is_a_pressable_button() {
    let tree = build_tree(&caption_inputs());
    let node = &tree.nodes.iter().find(|(id, _)| *id == NAME_BAND_NODE).unwrap().1;
    assert_eq!((node.role(), node.label()), (Role::Button, Some("Window: #1 Work")));
    assert!(node.supports_action(AccessAction::Click));
    let without = TreeInputs { name_band: None, ..caption_inputs() };
    assert!(build_tree(&without).nodes.iter().all(|(id, _)| *id != NAME_BAND_NODE));
    // With `click = "none"` the name is a label a reader cannot press.
    let inert = TreeInputs {
        name_band: Some(("Window: #1 Work".to_owned(), rect(400.0, 0.0, 120.0, 32.0), false)),
        ..caption_inputs()
    };
    let tree = build_tree(&inert);
    let node = &tree.nodes.iter().find(|(id, _)| *id == NAME_BAND_NODE).unwrap().1;
    assert_eq!(node.role(), Role::Label);
    assert!(!node.supports_action(AccessAction::Click));
}
