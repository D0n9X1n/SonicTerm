//! Measured status-strip placement, shortening, and priority-based omission.

use super::*;

/// Strip geometry for [`fit`], in the renderer's pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FitMetrics {
    /// Strip width.
    pub width: f32,
    /// Space at each strip edge.
    pub edge_pad: f32,
    /// Space between segments and between lists.
    pub segment_gap: f32,
    /// Width of the `|` drawn between two adjacent segments of one list; it widens only those gaps.
    pub separator_width: f32,
}

impl FitMetrics {
    /// The gap between two adjacent segments of one list: the segment gap with the separator in it.
    fn in_group_gap(&self) -> f32 {
        self.segment_gap + self.separator_width
    }
}

/// The text of the separator drawn between adjacent segments of one list.
pub const SEGMENT_SEPARATOR: &str = "|";

/// The separators between adjacent presented segments of each list, left list first: a `|` in the
/// name tone centered in each gap. None goes at a list's edge or between lists, and none is a hit
/// box, so a press on one finds no segment.
pub fn separators(
    view: &StatusLineView,
    placed: &[PlacedSegment],
    metrics: &FitMetrics,
) -> Vec<PlacedPart> {
    let mut found = Vec::new();
    for side in [StatusSide::Left, StatusSide::Center, StatusSide::Right] {
        let mut group: Vec<&PlacedSegment> = placed
            .iter()
            .filter(|segment| {
                view.segments.get(segment.index).is_some_and(|view| view.side == side)
            })
            .collect();
        group.sort_by(|first, second| first.left.total_cmp(&second.left));
        for pair in group.windows(2) {
            let (gap_start, gap_end) = (pair[0].left + pair[0].width, pair[1].left);
            found.push(PlacedPart {
                text: SEGMENT_SEPARATOR.to_string(),
                tone: StatusTone::Name,
                left: (gap_start + gap_end) / 2.0 - metrics.separator_width / 2.0,
                width: metrics.separator_width,
            });
        }
    }
    found
}

/// One placed part; its text starts at `left` and spans `width`, chips included.
#[derive(Debug, Clone, PartialEq)]
pub struct PlacedPart {
    /// Text to draw.
    pub text: String,
    /// Color role.
    pub tone: StatusTone,
    /// Left edge.
    pub left: f32,
    /// Width of its text.
    pub width: f32,
}

/// One presented segment and its bounds.
#[derive(Debug, Clone, PartialEq)]
pub struct PlacedSegment {
    /// Index of the segment in [`StatusLineView::segments`].
    pub index: usize,
    /// Left edge.
    pub left: f32,
    /// Width.
    pub width: f32,
    /// Its parts, as fitted.
    pub parts: Vec<PlacedPart>,
}

/// A segment's fitting state.
struct FitSlot {
    index: usize,
    side: StatusSide,
    /// Drop order: lower goes first.
    rank: (bool, u16, usize),
    parts: Vec<StatusPart>,
    part_widths: Vec<f32>,
    compact: Option<Vec<StatusPart>>,
    active: bool,
}

impl FitSlot {
    /// Width of the slot's parts: the sum of their text widths.
    fn width(&self) -> f32 {
        self.part_widths.iter().sum()
    }

    /// Whether fitting left the slot nothing to draw, so it is not shown, takes no gap and gets
    /// no separator.
    fn is_blank(&self) -> bool {
        self.parts.iter().all(|part| part.text.trim().is_empty())
    }
}

/// Fit `view` into `metrics.width`, never overflowing.
///
/// Compact forms are tried first, lowest rank first. Then, lowest rank first,
/// a segment's text value is middle-ellipsized, its droppable details go, and
/// finally the whole segment goes. Rank orders unprotected before protected,
/// then ascending priority, then the segment nearest the center first.
pub fn fit(
    view: &StatusLineView,
    metrics: &FitMetrics,
    mut measure: impl FnMut(&str) -> f32,
) -> Vec<PlacedSegment> {
    let side_len =
        |side: StatusSide| view.segments.iter().filter(|segment| segment.side == side).count();
    let left_len = side_len(StatusSide::Left);
    let mut seen = [0usize; 3];
    let mut slots: Vec<FitSlot> = view
        .segments
        .iter()
        .enumerate()
        .map(|(index, segment)| {
            let side_index = match segment.side {
                StatusSide::Left => 0,
                StatusSide::Center => 1,
                StatusSide::Right => 2,
            };
            let position = seen[side_index];
            seen[side_index] += 1;
            // Distance from the center: 0 for the center list, then outwards.
            let distance = match segment.side {
                StatusSide::Left => left_len - position,
                StatusSide::Center => 0,
                StatusSide::Right => position + 1,
            };
            FitSlot {
                index,
                side: segment.side,
                rank: (segment.protected, segment.priority, distance),
                part_widths: segment.parts.iter().map(|part| measure(&part.text)).collect(),
                parts: segment.parts.clone(),
                compact: segment.compact.clone(),
                active: true,
            }
        })
        .collect();
    let group_width = |slots: &[FitSlot], side: StatusSide| -> Option<f32> {
        let widths: Vec<f32> = slots
            .iter()
            .filter(|slot| slot.active && slot.side == side)
            .map(|slot| slot.width())
            .collect();
        // Segments of one list are a separator gap apart, so a dropped segment takes its `|` along.
        (!widths.is_empty()).then(|| {
            widths.iter().sum::<f32>() + metrics.in_group_gap() * (widths.len() - 1) as f32
        })
    };
    loop {
        let groups: Vec<f32> = [StatusSide::Left, StatusSide::Center, StatusSide::Right]
            .into_iter()
            .filter_map(|side| group_width(&slots, side))
            .collect();
        let needed = 2.0 * metrics.edge_pad
            + groups.iter().sum::<f32>()
            + metrics.segment_gap * groups.len().saturating_sub(1) as f32;
        let overflow = needed - metrics.width;
        if overflow <= 0.0 {
            // When: `overflow` is not positive, everything active fits.
            break;
        }
        let compactable = slots
            .iter_mut()
            .filter(|slot| slot.active && slot.compact.is_some())
            .min_by_key(|slot| slot.rank);
        if let Some(slot) = compactable {
            // When: `compactable` found a compact form, use the lowest-ranked one before dropping.
            let compact = slot.compact.take().unwrap_or_default();
            slot.part_widths = compact.iter().map(|part| measure(&part.text)).collect();
            slot.parts = compact;
            slot.active = !slot.is_blank();
            continue;
        }
        let Some(slot) = slots.iter_mut().filter(|slot| slot.active).min_by_key(|slot| slot.rank)
        else {
            // When: no active entry is left in `slots`, the strip is empty and cannot overflow.
            break;
        };
        if shrink_slot(slot, overflow, &mut measure) {
            // When: `shrink_slot` narrowed a text value, recheck the fit before dropping.
            continue;
        }
        if let Some(position) = slot.parts.iter().rposition(|part| part.droppable) {
            // When: a droppable detail is left, drop it before the whole segment.
            slot.parts.remove(position);
            slot.part_widths.remove(position);
            slot.active = !slot.is_blank();
            continue;
        }
        slot.active = false;
    }
    place(&slots, metrics)
}

/// Middle-ellipsize the widest text value of `slot` by `overflow`. Returns
/// whether it got narrower; a value at its minimum is no longer ellipsizable.
fn shrink_slot(slot: &mut FitSlot, overflow: f32, measure: &mut impl FnMut(&str) -> f32) -> bool {
    let widest = (0..slot.parts.len())
        .filter(|&position| slot.parts[position].ellipsize)
        .max_by(|&first, &second| slot.part_widths[first].total_cmp(&slot.part_widths[second]));
    let Some(position) = widest else {
        // When: `widest` is None, no ellipsizable value is left and the caller drops instead.
        return false;
    };
    let old_width = slot.part_widths[position];
    let target = old_width - overflow;
    let shortened = ellipsize_middle_to_width(&slot.parts[position].text, target, measure);
    let new_width = measure(&shortened);
    let part = &mut slot.parts[position];
    if new_width > target || new_width >= old_width {
        // The minimum form was reached, stop ellipsizing this value.
        part.ellipsize = false;
    }
    if new_width >= old_width {
        // When: `new_width` is not below `old_width`, shortening gained nothing.
        return false;
    }
    part.text = shortened;
    slot.part_widths[position] = new_width;
    true
}

/// `text` middle-ellipsized to at most `max_width`, keeping at least
/// four graphemes; the minimum form may exceed `max_width`.
pub fn ellipsize_middle_to_width(
    text: &str,
    max_width: f32,
    measure: &mut impl FnMut(&str) -> f32,
) -> String {
    if measure(text) <= max_width {
        // When: `measure(text)` is within `max_width`, keep it whole.
        return text.to_string();
    }
    let graphemes: Vec<&str> = text.graphemes(true).collect();
    if graphemes.len() <= MIN_KEPT_GRAPHEMES + 1 {
        // When: `graphemes` is too short to shorten past `MIN_KEPT_GRAPHEMES`, keep it whole.
        return text.to_string();
    }
    let compose = |kept: usize| {
        let head = kept.div_ceil(2);
        let tail = kept / 2;
        let mut out: String = graphemes[..head].concat();
        out.push_str(ELLIPSIS);
        out.push_str(&graphemes[graphemes.len() - tail..].concat());
        out
    };
    let (mut low, mut high) = (MIN_KEPT_GRAPHEMES, graphemes.len() - 1);
    while low < high {
        let middle = (low + high).div_ceil(2);
        if measure(&compose(middle)) <= max_width {
            low = middle;
        } else {
            // When: `measure` at `middle` exceeds `max_width`, search shorter forms.
            high = middle - 1;
        }
    }
    compose(low)
}

/// Place active slots: left from the left edge, right from the right edge,
/// center centered and kept between them.
fn place(slots: &[FitSlot], metrics: &FitMetrics) -> Vec<PlacedSegment> {
    let mut placed = Vec::new();
    let active =
        |side: StatusSide| slots.iter().filter(move |slot| slot.active && slot.side == side);
    let mut pen = metrics.edge_pad;
    let mut left_end = None;
    for slot in active(StatusSide::Left) {
        placed.push(place_slot(slot, pen));
        pen += slot.width();
        left_end = Some(pen);
        pen += metrics.in_group_gap();
    }
    let mut pen = metrics.width - metrics.edge_pad;
    let mut right_start = None;
    for slot in active(StatusSide::Right).collect::<Vec<_>>().into_iter().rev() {
        pen -= slot.width();
        placed.push(place_slot(slot, pen));
        right_start = Some(pen);
        pen -= metrics.in_group_gap();
    }
    let center: Vec<&FitSlot> = active(StatusSide::Center).collect();
    if !center.is_empty() {
        let total = center.iter().map(|slot| slot.width()).sum::<f32>()
            + metrics.in_group_gap() * (center.len() - 1) as f32;
        let low = left_end.map_or(metrics.edge_pad, |end| end + metrics.segment_gap);
        let high = right_start
            .map_or(metrics.width - metrics.edge_pad, |start| start - metrics.segment_gap)
            - total;
        let mut pen = ((metrics.width - total) / 2.0).clamp(low, high.max(low));
        for slot in center {
            placed.push(place_slot(slot, pen));
            pen += slot.width() + metrics.in_group_gap();
        }
    }
    placed.sort_by_key(|segment| segment.index);
    placed
}

/// Place one slot's parts from `origin`, each directly after the one before it.
fn place_slot(slot: &FitSlot, origin: f32) -> PlacedSegment {
    let mut pen = origin;
    let mut parts = Vec::with_capacity(slot.parts.len());
    for (part, width) in slot.parts.iter().zip(&slot.part_widths) {
        parts.push(PlacedPart {
            text: part.text.clone(),
            tone: part.tone,
            left: pen,
            width: *width,
        });
        pen += width;
    }
    PlacedSegment { index: slot.index, left: origin, width: pen - origin, parts }
}

#[cfg(test)]
#[path = "status_line_fit_tests.rs"]
mod status_line_fit_tests;
