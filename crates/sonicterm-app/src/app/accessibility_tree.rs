//! Accessibility tree nodes, status health and announcement phrases.

use super::*;

/// The About window's row `index`, counted from the version row.
pub(crate) fn about_row_node(index: usize) -> NodeId {
    NodeId(TERMINAL_WINDOW | KIND_ABOUT_ROW | index as u64)
}

/// A fixed code per segment, written out rather than taken from the enum's order, so a node id
/// never changes when the catalog is reordered. Exhaustive: a new segment does not compile
/// without a code.
pub(super) fn segment_code(id: StatusSegmentId) -> u64 {
    match id {
        StatusSegmentId::Alerts => 1,
        StatusSegmentId::Health => 2,
        StatusSegmentId::Mem => 3,
        StatusSegmentId::Mode => 4,
        StatusSegmentId::Input => 5,
        StatusSegmentId::Clip => 6,
        StatusSegmentId::PathCheck => 7,
        StatusSegmentId::Link => 8,
        StatusSegmentId::Paste => 9,
        StatusSegmentId::Sel => 10,
        StatusSegmentId::Ime => 11,
        StatusSegmentId::Scroll => 12,
        StatusSegmentId::Cpu => 13,
        StatusSegmentId::Frame => 14,
        StatusSegmentId::Out => 15,
        StatusSegmentId::TabMem => 16,
        StatusSegmentId::Glyphs => 17,
        StatusSegmentId::History => 18,
        StatusSegmentId::Font => 19,
        StatusSegmentId::Zoom => 20,
        StatusSegmentId::Size => 21,
    }
}

/// The node of segment `id`: the same in every tree, whatever the segment's position.
pub(crate) fn segment_node(id: StatusSegmentId) -> NodeId {
    NodeId(TERMINAL_WINDOW | KIND_SEGMENT | segment_code(id))
}

/// The segment a node id names, if it names one.
pub(super) fn segment_for_node(node: u64) -> Option<StatusSegmentId> {
    StatusSegmentId::ALL.into_iter().find(|id| segment_node(*id).0 == node)
}

/// A rect in raster px.
pub(super) fn rect(left: f32, top: f32, width: f32, height: f32) -> Rect {
    Rect {
        x0: f64::from(left),
        y0: f64::from(top),
        x1: f64::from(left + width),
        y1: f64::from(top + height),
    }
}

/// Build a full tree: the window, the status line group with its segments (all live off), and
/// the announcement node, which is never hidden and never removed.
pub(crate) fn build_tree(inputs: &TreeInputs<'_>) -> TreeUpdate {
    let mut nodes = Vec::new();
    let mut children = Vec::new();
    if let Some((band, strip)) = inputs.strip {
        let mut group = Node::new(Role::Group);
        group.set_label(inputs.strip_label.clone());
        group.set_live(Live::Off);
        group.set_bounds(band);
        let mut segments = Vec::with_capacity(strip.segments.len());
        for segment in &strip.segments {
            let role =
                if segment.role == AccessibleRole::Button { Role::Button } else { Role::Label };
            let mut node = Node::new(role);
            node.set_label(segment.name.clone());
            node.set_live(Live::Off);
            node.set_bounds(rect(
                segment.left_px,
                band.y0 as f32,
                segment.width,
                (band.y1 - band.y0) as f32,
            ));
            node.add_action(AccessAction::Focus);
            if segment.role == AccessibleRole::Button {
                node.add_action(AccessAction::Click);
            }
            segments.push(segment_node(segment.id));
            nodes.push((segment_node(segment.id), node));
        }
        group.set_children(segments);
        nodes.push((STRIP_NODE, group));
        children.push(STRIP_NODE);
    }
    let mut announcement = Node::new(Role::Label);
    announcement.set_label(
        inputs.announcement.as_ref().map(|announced| announced.text.clone()).unwrap_or_default(),
    );
    announcement.set_description(inputs.announcements_label.clone());
    let assertive = inputs.announcement.as_ref().is_some_and(|announced| announced.assertive);
    announcement.set_live(if assertive { Live::Assertive } else { Live::Polite });
    announcement.set_bounds(inputs.announcement_bounds);
    nodes.push((ANNOUNCEMENT_NODE, announcement));
    children.push(ANNOUNCEMENT_NODE);
    let mut root = Node::new(Role::Window);
    root.set_label(inputs.title.clone());
    root.set_children(children);
    nodes.push((ROOT_NODE, root));
    let shown = |id: StatusSegmentId| {
        inputs.strip.is_some_and(|(_, strip)| strip.segments.iter().any(|segment| segment.id == id))
    };
    let focus = inputs.focus.filter(|id| shown(*id)).map_or(ROOT_NODE, segment_node);
    TreeUpdate {
        nodes,
        tree: Some(TreeInfo {
            toolkit_name: Some("SonicTerm".to_owned()),
            ..TreeInfo::new(ROOT_NODE)
        }),
        tree_id: TreeId::ROOT,
        focus,
    }
}

/// Build the About window's tree: the window titled `title`, one label node per report row in
/// order, read as `label: value`, then the Copy button labelled from `copy`, which can be pressed.
pub(crate) fn build_about_tree(title: &str, rows: &[(String, String)], copy: &str) -> TreeUpdate {
    let mut nodes = Vec::with_capacity(rows.len() + 2);
    let mut children = Vec::with_capacity(rows.len() + 1);
    for (index, (label, value)) in rows.iter().enumerate() {
        let mut node = Node::new(Role::Label);
        node.set_label(format!("{label}: {value}"));
        node.set_value(value.clone());
        node.add_action(AccessAction::Focus);
        children.push(about_row_node(index));
        nodes.push((about_row_node(index), node));
    }
    let mut button = Node::new(Role::Button);
    // The drawn label is bracketed, `[ Copy ]`; a screen reader says only the word.
    button.set_label(copy.trim().trim_start_matches('[').trim_end_matches(']').trim().to_owned());
    button.add_action(AccessAction::Focus);
    button.add_action(AccessAction::Click);
    children.push(ABOUT_COPY_NODE);
    nodes.push((ABOUT_COPY_NODE, button));
    let mut root = Node::new(Role::Window);
    root.set_label(title.to_owned());
    root.set_children(children);
    nodes.push((ROOT_NODE, root));
    TreeUpdate {
        nodes,
        tree: Some(TreeInfo {
            toolkit_name: Some("SonicTerm".to_owned()),
            ..TreeInfo::new(ROOT_NODE)
        }),
        tree_id: TreeId::ROOT,
        focus: ROOT_NODE,
    }
}

/// The health segment's severity in `view`, from its tones.
pub(crate) fn health_severity(view: &StatusLineView) -> Severity {
    let Some(health) = view.segments.iter().find(|segment| segment.id == StatusSegmentId::Health)
    else {
        // When: the view has no `Health` segment, nothing is wrong that it shows.
        return Severity::None;
    };
    let tones = || health.parts.iter().map(|part| part.tone);
    if tones().any(|tone| tone == StatusTone::Error) {
        Severity::Error
    } else if tones().any(|tone| matches!(tone, StatusTone::Warn | StatusTone::ChipWarn)) {
        // When: `tones` has a warning part but no error part, the segment warns.
        Severity::Warn
    } else {
        // When: `tones` has no warning or error part, the segment reports nothing wrong.
        Severity::None
    }
}

/// Health's severity in `view` and its spoken name with any warning cue; `None` while nothing is
/// wrong.
pub(super) fn health_state(
    view: &StatusLineView,
    i18n: &sonicterm_ui::i18n::I18n,
) -> (Severity, Option<String>) {
    let severity = health_severity(view);
    if severity == Severity::None {
        // When: `severity` is None, nothing is wrong and there is nothing to say.
        return (Severity::None, None);
    }
    let health = [CommittedSegment {
        id: StatusSegmentId::Health,
        left_px: 0.0,
        width: 1.0,
        clickable: false,
    }];
    let text = accessible_strip(&health, view, false, i18n)
        .segments
        .into_iter()
        .next()
        .map(|segment| segment.name);
    (severity, text)
}

impl App {
    /// The words for `kind` newly marked on tab `tab` of `window_id`; `None` once the tab closed.
    pub(super) fn mark_phrase(
        &self,
        window_id: WindowId,
        tab: TabId,
        kind: MarkKind,
    ) -> Option<String> {
        let tabs = &self.windows.get(&window_id)?.tabs;
        let index = tabs.index_of(tab)?;
        Some(self.alert_phrase(index + 1, kind))
    }

    /// The words for a new `kind` mark on tab number `tab`; a finish says the duration its mark
    /// recorded, which a visit to the tab since does not clear.
    pub(super) fn alert_phrase(&self, tab: usize, kind: MarkKind) -> String {
        let i18n = &self.i18n;
        let source = match kind {
            MarkKind::Bell => i18n.translate("status-spoken-alert-bell"),
            MarkKind::Notify => i18n.translate("status-spoken-alert-notify"),
            MarkKind::Finished(secs) => {
                // Say how long the command ran.
                let duration = format_duration(secs);
                i18n.t_args(
                    "status-spoken-alert-finished",
                    Some(&[("duration", duration.as_str())]),
                )
            }
        };
        let tab_text = tab.to_string();
        let phrase = i18n.t_args(
            "status-spoken-alert-tab",
            Some(&[("tab", tab_text.as_str()), ("sources", source.as_str())]),
        );
        format!("{} {phrase}", i18n.translate("status-spoken-alerts"))
    }
}

#[cfg(test)]
#[path = "accessibility_tree_tests.rs"]
mod accessibility_tree_tests;
