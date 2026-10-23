//! The status line checklist palette page: choose segments for this session, from the palette
//! command or a right press on the strip. Every change applies at once to every terminal window.

use sonicterm_cfg::status_line::{SegmentGroup, Sensitivity, StatusSegmentId, StatusSide};
use sonicterm_cfg::status_overrides::MoveDirection;
use sonicterm_ui::command_palette::{StatusSegmentRow, StatusSegmentRowKind};
use winit::window::WindowId;

use crate::app::{App, FrontmostKind};

/// Checklist groups in catalog order, with their heading keys. A group with no implemented
/// segment is not shown.
pub(super) const GROUPS: [(SegmentGroup, &str); 7] = [
    (SegmentGroup::Core, "status-group-core"),
    (SegmentGroup::Pointer, "status-group-pointer"),
    (SegmentGroup::Input, "status-group-input"),
    (SegmentGroup::Display, "status-group-display"),
    (SegmentGroup::OtherTabs, "status-group-other-tabs"),
    (SegmentGroup::Diagnostics, "status-group-diagnostics"),
    (SegmentGroup::ProgramReported, "status-group-program-reported"),
];

/// The localized-name key of segment `id`: `status-name-` and its config spelling with `-`.
fn name_key(id: StatusSegmentId) -> String {
    format!("status-name-{}", id.as_str().replace('_', "-"))
}

/// The key of list `side`'s name.
fn side_key(side: StatusSide) -> &'static str {
    match side {
        StatusSide::Left => "status-checklist-left",
        StatusSide::Center => "status-checklist-center",
        StatusSide::Right => "status-checklist-right",
    }
}

/// Every message key the checklist reads, for the locale coverage test.
#[cfg(test)]
fn label_keys() -> Vec<String> {
    let mut keys: Vec<String> = [
        "command-choose-status-line-segments",
        "status-checklist-title",
        "status-checklist-footer",
        "status-checklist-show",
        "status-checklist-hide-sensitive",
        "status-checklist-reset",
        "status-checklist-sensitive",
        "status-checklist-hidden",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    keys.extend(
        [StatusSide::Left, StatusSide::Center, StatusSide::Right]
            .map(|side| side_key(side).to_owned()),
    );
    keys.extend(GROUPS.map(|(_, key)| key.to_owned()));
    keys.extend(StatusSegmentId::ALL.into_iter().filter(|id| id.meta().implemented).map(name_key));
    keys
}

impl App {
    /// Open the checklist attached to `window_id`, or to the frontmost window when `None`,
    /// highlighting `highlighted`'s row or else the first row.
    pub(in crate::app) fn open_status_segments(
        &mut self,
        window_id: Option<WindowId>,
        highlighted: Option<StatusSegmentId>,
    ) {
        self.palette_pointer_capture = None;
        let attached = window_id.or_else(|| match self.frontmost_kind() {
            FrontmostKind::Child(id) => Some(id),
            _ => None,
        });
        // The palette names its main-window attachment `None`, as every other page does.
        self.palette_attached_window = attached.filter(|id| Some(*id) != self.main_window_id);
        let rows = self.status_segment_rows();
        let title = self.i18n.translate("status-checklist-title");
        let footer = self.i18n.translate("status-checklist-footer");
        self.command_palette.start_status_segments(title, footer, rows, highlighted);
        self.request_redraw_for_overlay(self.palette_attached_window);
    }

    /// The checklist rows for the strip the session shows now.
    fn status_segment_rows(&mut self) -> Vec<StatusSegmentRow> {
        let shown = self.current_status_line();
        let check = |on: bool| if on { "✓ " } else { "  " };
        let row = |kind, label: String, name: String| StatusSegmentRow { kind, label, name };
        let mut rows = vec![
            row(
                StatusSegmentRowKind::ShowStrip,
                format!("{}{}", check(shown.enabled), self.i18n.translate("status-checklist-show")),
                String::new(),
            ),
            row(
                StatusSegmentRowKind::HideSensitive,
                format!(
                    "{}{}",
                    check(shown.hide_sensitive),
                    self.i18n.translate("status-checklist-hide-sensitive")
                ),
                String::new(),
            ),
        ];
        for (group, heading) in GROUPS {
            let ids: Vec<StatusSegmentId> = StatusSegmentId::ALL
                .into_iter()
                .filter(|id| id.meta().implemented && id.meta().group == group)
                .collect();
            if ids.is_empty() {
                // When: ids is empty, the group has no implemented segment and no heading.
                continue;
            }
            rows.push(row(
                StatusSegmentRowKind::Heading,
                self.i18n.translate(heading),
                String::new(),
            ));
            for id in ids {
                let name = self.i18n.translate(&name_key(id));
                let side = shown.segment(id).map(|segment| segment.side);
                let mut label = format!("{}{name}", check(side.is_some()));
                if let Some(side) = side {
                    label.push_str(" — ");
                    label.push_str(&self.i18n.translate(side_key(side)));
                }
                if id.meta().sensitivity != Sensitivity::None {
                    label.push_str(" · ");
                    label.push_str(&self.i18n.translate("status-checklist-sensitive"));
                    if shown.hide_sensitive {
                        label.push_str(" · ");
                        label.push_str(&self.i18n.translate("status-checklist-hidden"));
                    }
                }
                rows.push(row(StatusSegmentRowKind::Segment(id), label, name));
            }
        }
        rows.push(row(
            StatusSegmentRowKind::Reset,
            self.i18n.translate("status-checklist-reset"),
            String::new(),
        ));
        rows
    }

    /// Apply the highlighted checklist row; the page stays open and shows the change.
    pub(in crate::app) fn activate_status_segment_row(&mut self) {
        let Some(kind) = self.command_palette.selected_status_row() else {
            // When: selected_status_row is None, no row is highlighted and Enter changes nothing.
            return;
        };
        match kind {
            StatusSegmentRowKind::ShowStrip => self.toggle_status_line(),
            StatusSegmentRowKind::HideSensitive => self.toggle_status_line_sensitive(),
            StatusSegmentRowKind::Segment(id) => self.toggle_status_segment(id),
            StatusSegmentRowKind::Reset => self.reset_status_line_choices(),
            StatusSegmentRowKind::Heading => {
                // When: kind is a heading, which is never highlighted, nothing applies.
            }
        }
        self.refresh_status_segment_rows();
    }

    /// Move the highlighted segment to the neighbouring list when it is shown. Returns whether
    /// the key was a move; on any other row the key edits the query as usual.
    pub(in crate::app) fn move_status_segment_row(&mut self, direction: MoveDirection) -> bool {
        let Some(StatusSegmentRowKind::Segment(id)) = self.command_palette.selected_status_row()
        else {
            // When: selected_status_row is not a Segment, Left and Right move the caret.
            return false;
        };
        if self.current_status_line().segment(id).is_none() {
            // When: the segment is not shown, there is no list to move it from.
            return false;
        }
        self.move_status_segment(id, direction);
        self.refresh_status_segment_rows();
        true
    }

    /// Rebuild the open checklist's rows after a change and redraw its window once.
    fn refresh_status_segment_rows(&mut self) {
        let rows = self.status_segment_rows();
        self.command_palette.update_status_segments(rows);
        self.request_checklist_redraw();
    }
}

#[cfg(test)]
#[path = "status_segments_tests.rs"]
mod status_segments_tests;
