//! Session choices on the status line, kept as overrides on the file's resolved lists.
//!
//! An override names a segment, never a position, so a file that only reorders its lists keeps
//! every override. File order governs every segment the user did not touch; a segment the user
//! placed follows the file's segments on its side, in the order the user placed it.

use std::collections::{BTreeMap, HashMap};

use crate::status_line::{ResolvedSegment, ResolvedStatusLine, StatusSegmentId, StatusSide};

/// One segment's session choice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SegmentOverride {
    /// Hidden although the file lists it.
    Absent,
    /// Shown on `side`, after that side's file segments, in `order` among the placed ones: a
    /// segment the file does not list, or one the user moved to another side.
    Placed {
        /// The list it is drawn in.
        side: StatusSide,
        /// Its place among the placed segments, `1..=n`, dense.
        order: u16,
    },
}

/// Which neighbouring list a move goes to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MoveDirection {
    /// Toward the left list: right → center → left.
    Left,
    /// Toward the right list: left → center → right.
    Right,
}

/// The session's overrides on the file's `[status_line]`. Bounded by the catalog: at most one
/// entry per segment id.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StatusLineOverrides {
    enabled: Option<bool>,
    hide_sensitive: Option<bool>,
    segments: BTreeMap<StatusSegmentId, SegmentOverride>,
}

/// Where the file puts a segment: its side, or `None` when no list names it.
type FileState = HashMap<StatusSegmentId, StatusSide>;

fn file_state(file: &ResolvedStatusLine) -> FileState {
    file.segments.iter().map(|segment| (segment.id, segment.side)).collect()
}

/// The list next to `side` in `direction`, if there is one.
fn neighbour(side: StatusSide, direction: MoveDirection) -> Option<StatusSide> {
    match (side, direction) {
        (StatusSide::Right, MoveDirection::Left) => Some(StatusSide::Center),
        (StatusSide::Center, MoveDirection::Left) => Some(StatusSide::Left),
        (StatusSide::Left, MoveDirection::Right) => Some(StatusSide::Center),
        (StatusSide::Center, MoveDirection::Right) => Some(StatusSide::Right),
        (StatusSide::Left, MoveDirection::Left) | (StatusSide::Right, MoveDirection::Right) => None,
    }
}

impl StatusLineOverrides {
    /// The `enabled` override, if the session changed it from the file.
    pub fn enabled(&self) -> Option<bool> {
        self.enabled
    }

    /// The `hide_sensitive` override, if the session changed it from the file.
    pub fn hide_sensitive(&self) -> Option<bool> {
        self.hide_sensitive
    }

    /// The override on segment `id`, if any.
    pub fn segment(&self, id: StatusSegmentId) -> Option<SegmentOverride> {
        self.segments.get(&id).copied()
    }

    /// Every order number held, for bounds checks.
    pub fn orders(&self) -> impl Iterator<Item = u16> + '_ {
        self.segments.values().filter_map(|choice| match choice {
            SegmentOverride::Placed { order, .. } => Some(*order),
            SegmentOverride::Absent => None,
        })
    }

    /// Whether any override is held.
    pub fn is_empty(&self) -> bool {
        self.enabled.is_none() && self.hide_sensitive.is_none() && self.segments.is_empty()
    }

    /// Drop every override.
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// Show or hide the strip; an override equal to `file` is not kept.
    pub fn set_enabled(&mut self, file: &ResolvedStatusLine, enabled: bool) {
        self.enabled = (enabled != file.enabled).then_some(enabled);
    }

    /// Hide or show sensitive segments; an override equal to `file` is not kept.
    pub fn set_hide_sensitive(&mut self, file: &ResolvedStatusLine, hide: bool) {
        self.hide_sensitive = (hide != file.hide_sensitive).then_some(hide);
    }

    /// The lists and toggles the strip shows: `file` with every override applied.
    pub fn effective(&self, file: &ResolvedStatusLine) -> ResolvedStatusLine {
        let mut shown = file.clone();
        shown.enabled = self.enabled.unwrap_or(file.enabled);
        shown.hide_sensitive = self.hide_sensitive.unwrap_or(file.hide_sensitive);
        let configured = |id: StatusSegmentId| -> Option<ResolvedSegment> {
            file.segments
                .iter()
                .chain(file.unlisted.iter())
                .find(|segment| segment.id == id)
                .cloned()
        };
        let mut placed: Vec<(u16, ResolvedSegment)> = self
            .segments
            .iter()
            .filter_map(|(id, choice)| match choice {
                SegmentOverride::Placed { side, order } => {
                    let segment = configured(*id)?;
                    Some((*order, ResolvedSegment { side: *side, ..segment }))
                }
                SegmentOverride::Absent => None,
            })
            .collect();
        placed.sort_by_key(|(order, _)| *order);
        let mut segments = Vec::with_capacity(file.segments.len() + placed.len());
        for side in [StatusSide::Left, StatusSide::Center, StatusSide::Right] {
            // The file's untouched segments keep their order; any override takes a segment out.
            segments.extend(
                file.segments
                    .iter()
                    .filter(|segment| {
                        segment.side == side && !self.segments.contains_key(&segment.id)
                    })
                    .cloned(),
            );
            segments.extend(
                placed
                    .iter()
                    .filter(|(_, segment)| segment.side == side)
                    .map(|(_, segment)| segment.clone()),
            );
        }
        shown.unlisted.retain(|segment| !segments.iter().any(|shown| shown.id == segment.id));
        shown.segments = segments;
        shown
    }

    /// Show `id` if it is hidden, hide it if it is shown. A segment enabled anew goes to the list
    /// its preset puts it in, or its home list, after that list's segments.
    pub fn toggle_segment(&mut self, file: &ResolvedStatusLine, id: StatusSegmentId) {
        let listed = file_state(file).get(&id).copied();
        let shown = self.effective(file).segment(id).is_some();
        if shown {
            if listed.is_some() {
                // The file shows it, so hiding it needs an `absent` override.
                self.segments.insert(id, SegmentOverride::Absent);
            } else {
                // When: listed is None, only the session showed it; dropping that hides it.
                self.segments.remove(&id);
            }
        } else if listed.is_some() {
            // When: listed is Some but it is hidden, removing the `absent` override cancels the
            // disable and returns it to its file position; appending applies only to unlisted ids.
            self.segments.remove(&id);
        } else {
            // When: listed is None, the segment is placed on its preset or home list.
            let [left, center, _] = file.preset.lists();
            let side = if left.contains(&id) {
                StatusSide::Left
            } else if center.contains(&id) {
                // When: left lacks id and center holds it, the preset places it in the center.
                StatusSide::Center
            } else if file.preset.lists()[2].contains(&id) {
                // When: neither left nor center holds id, but the preset's right list does.
                StatusSide::Right
            } else {
                // When: no preset list holds id, it goes to its home list.
                id.meta().home_side
            };
            self.place(id, side);
        }
        self.renumber();
    }

    /// Move a shown segment to the neighbouring list in `direction`, at the end of that list,
    /// even when it is the side the file gives it.
    pub fn move_segment(
        &mut self,
        file: &ResolvedStatusLine,
        id: StatusSegmentId,
        direction: MoveDirection,
    ) {
        let Some(current) = self.effective(file).segment(id).map(|segment| segment.side) else {
            // When: effective shows no segment id, there is nothing to move.
            return;
        };
        let Some(target) = neighbour(current, direction) else {
            // When: neighbour finds no list beyond current, the segment stays.
            return;
        };
        self.place(id, target);
        self.renumber();
        self.drop_satisfied(file);
    }

    /// Reconcile with a reload from `previous` to `new`: drop an `absent` override only when the
    /// file changed that segment's presence, a placed one when the file changed that segment's
    /// presence or side, an `enabled` or `hide_sensitive` override whose own value changed, and
    /// every override the new file already satisfies. Reordering alone changes none.
    pub fn reconcile(&mut self, previous: &ResolvedStatusLine, new: &ResolvedStatusLine) {
        if previous.enabled != new.enabled {
            self.enabled = None;
        }
        if previous.hide_sensitive != new.hide_sensitive {
            self.hide_sensitive = None;
        }
        if self.enabled == Some(new.enabled) {
            self.enabled = None;
        }
        if self.hide_sensitive == Some(new.hide_sensitive) {
            self.hide_sensitive = None;
        }
        let (before, after) = (file_state(previous), file_state(new));
        self.segments.retain(|id, choice| match choice {
            SegmentOverride::Absent => before.contains_key(id) == after.contains_key(id),
            SegmentOverride::Placed { .. } => before.get(id) == after.get(id),
        });
        self.renumber();
        self.drop_satisfied(new);
    }

    /// Drop every segment override whose removal leaves what `file` shows unchanged, releasing its
    /// order number. Bounded by the catalog, so at most one effective list per segment id.
    fn drop_satisfied(&mut self, file: &ResolvedStatusLine) {
        let shown = self.effective(file);
        let ids: Vec<StatusSegmentId> = self.segments.keys().copied().collect();
        for id in ids {
            let Some(choice) = self.segments.remove(&id) else {
                // When: segments no longer holds id, an earlier pass already dropped it.
                continue;
            };
            if self.effective(file) != shown {
                // Without the override the strip differs, so the file does not satisfy it.
                self.segments.insert(id, choice);
            }
        }
        self.renumber();
    }

    /// Place `id` on `side`, last among the placed segments.
    fn place(&mut self, id: StatusSegmentId, side: StatusSide) {
        let order = self.orders().max().unwrap_or(0).saturating_add(1);
        self.segments.insert(id, SegmentOverride::Placed { side, order });
    }

    /// Renumber the placed segments `1..=n` in their current order, so no number exceeds the
    /// catalog size however many changes the session makes.
    fn renumber(&mut self) {
        let mut placed: Vec<(u16, StatusSegmentId)> = self
            .segments
            .iter()
            .filter_map(|(id, choice)| match choice {
                SegmentOverride::Placed { order, .. } => Some((*order, *id)),
                SegmentOverride::Absent => None,
            })
            .collect();
        placed.sort_unstable();
        for (index, (_, id)) in placed.into_iter().enumerate() {
            if let Some(SegmentOverride::Placed { order, .. }) = self.segments.get_mut(&id) {
                *order = index as u16 + 1;
            }
        }
    }
}

/// The `[status_line]` values a session shows, which Save Current Settings writes where they
/// differ from the file on disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusLineSave {
    /// Whether the strip is shown.
    pub enabled: bool,
    /// Whether sensitive segments are hidden.
    pub hide_sensitive: bool,
    /// The window edge the strip sits on.
    pub position: crate::status_line::StatusPosition,
    /// The `left`, `center` and `right` lists, in order.
    pub lists: [Vec<StatusSegmentId>; 3],
}

impl StatusLineSave {
    /// The values `resolved` shows.
    pub fn from_resolved(resolved: &ResolvedStatusLine) -> Self {
        let side = |side: StatusSide| {
            resolved
                .segments
                .iter()
                .filter(|segment| segment.side == side)
                .map(|segment| segment.id)
                .collect()
        };
        Self {
            enabled: resolved.enabled,
            hide_sensitive: resolved.hide_sensitive,
            position: resolved.position,
            lists: [side(StatusSide::Left), side(StatusSide::Center), side(StatusSide::Right)],
        }
    }
}

/// The `[status_line]` a save wrote: the table as the rendered file holds it and its resolved
/// lists, which become the session's applied table and reconciliation baseline.
#[derive(Debug, Clone, PartialEq)]
pub struct WrittenStatusLine {
    /// The `[status_line]` table of the written document.
    pub table: crate::status_line::StatusLineConfig,
    /// That table, resolved.
    pub resolved: ResolvedStatusLine,
}

#[cfg(test)]
#[path = "status_overrides_tests.rs"]
mod status_overrides_tests;
