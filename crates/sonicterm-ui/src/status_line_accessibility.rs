//! Accessible status-strip structure derived from the committed visual layout.

use super::*;

/// One segment as the last presented frame committed it: what pointer routing hit-tests.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CommittedSegment {
    /// Which segment.
    pub id: StatusSegmentId,
    /// Left edge along the strip, raster px.
    pub left_px: f32,
    /// Width, raster px.
    pub width: f32,
    /// Whether a click on it does something.
    pub clickable: bool,
}

/// How assistive technology presents a segment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AccessibleRole {
    /// A clickable segment, which takes `Click`.
    Button,
    /// An inert segment.
    Label,
}

/// One segment as assistive technology sees it.
#[derive(Debug, Clone, PartialEq)]
pub struct AccessibleSegment {
    /// The segment's own id, never its position, so it is stable across reorder and fitting.
    pub id: StatusSegmentId,
    /// Button when clickable, otherwise label.
    pub role: AccessibleRole,
    /// Plain words: a warning cue when the segment warns, its spoken name and its spoken value.
    pub name: String,
    /// Left edge along the strip, raster px.
    pub left_px: f32,
    /// Width, raster px.
    pub width: f32,
}

/// The strip as assistive technology sees it, with a hash of everything a screen reader reads.
#[derive(Debug, Clone, PartialEq)]
pub struct AccessibleStrip {
    /// Segments in visual order.
    pub segments: Vec<AccessibleSegment>,
    /// Hash of ids, roles, names and bounds; colors and styles are not part of it.
    pub hash: u64,
}

/// Build the accessible strip: structure (which segments, their order, bounds and actions) from
/// the `committed` layout of the last presented frame, and each segment's name from the current
/// `view`, so a value can change without a new frame while nothing undrawn is exposed. A
/// committed segment the view does not format keeps its node, named by its spoken name alone, and
/// while `hide_sensitive` is on every sensitive segment is absent.
pub fn accessible_strip(
    committed: &[CommittedSegment],
    view: &StatusLineView,
    hide_sensitive: bool,
    i18n: &I18n,
) -> AccessibleStrip {
    let mut ordered: Vec<&CommittedSegment> = committed.iter().collect();
    ordered.sort_by(|left, right| left.left_px.total_cmp(&right.left_px));
    let mut segments = Vec::with_capacity(ordered.len());
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    for placed in ordered {
        if hide_sensitive && placed.id.meta().sensitivity != Sensitivity::None {
            // When: `hide_sensitive` is on and the segment holds anything sensitive, it is absent.
            continue;
        }
        let current = view.segments.iter().find(|segment| segment.id == placed.id);
        let tones =
            || current.into_iter().flat_map(|segment| segment.parts.iter()).map(|part| part.tone);
        let cue = if tones().any(|tone| tone == StatusTone::Error) {
            Some(i18n.translate("status-accessible-error"))
        } else if tones().any(|tone| matches!(tone, StatusTone::Warn | StatusTone::ChipWarn)) {
            // When: `tones` has a warning part but no error, the name says so in words, not color.
            Some(i18n.translate("status-accessible-warning"))
        } else {
            // When: `tones` has no warning or error part, the name carries no cue.
            None
        };
        // A segment the current inputs do not format keeps its node with its spoken name only.
        let (spoken_name, spoken_value) = current.map_or_else(
            || (i18n.translate(placed.id.meta().spoken_name_key), String::new()),
            |segment| (segment.spoken.name.clone(), segment.spoken.value.clone()),
        );
        let name = [cue, Some(spoken_name), Some(spoken_value)]
            .into_iter()
            .flatten()
            .filter(|word| !word.trim().is_empty())
            .collect::<Vec<_>>()
            .join(" ");
        let role = if placed.clickable { AccessibleRole::Button } else { AccessibleRole::Label };
        placed.id.hash(&mut hasher);
        role.hash(&mut hasher);
        name.hash(&mut hasher);
        placed.left_px.to_bits().hash(&mut hasher);
        placed.width.to_bits().hash(&mut hasher);
        segments.push(AccessibleSegment {
            id: placed.id,
            role,
            name,
            left_px: placed.left_px,
            width: placed.width,
        });
    }
    AccessibleStrip { segments, hash: hasher.finish() }
}

#[cfg(test)]
#[path = "status_line_accessibility_tests.rs"]
mod status_line_accessibility_tests;
