//! OSC 22 pointer shapes: kitty's `=`/`>`/`<`/`?` grammar over one bounded stack per screen, the
//! W3C name table with its xterm and kitty aliases, and the effective shape the host shows.

use super::*;

/// Largest OSC 22 payload accepted, in raw bytes; a longer packet is dropped whole.
pub(super) const MAX_POINTER_SHAPE_OSC_BYTES: usize = 4 * 1024;
/// Names read from one packet; later names are ignored.
const MAX_POINTER_SHAPE_NAMES: usize = 64;
/// Longest name looked up; a longer name is unknown.
const MAX_POINTER_SHAPE_NAME_BYTES: usize = 32;
/// Entries one screen's stack holds; a push past it evicts the bottom entry.
const POINTER_SHAPE_STACK_MAX: usize = 16;

/// A pointer shape a program may ask for with OSC 22: the 34 W3C cursor names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PointerShape {
    /// `default`: the platform arrow.
    Default,
    /// `context-menu`: a context menu is available.
    ContextMenu,
    /// `help`: help is available.
    Help,
    /// `pointer`: a link.
    Pointer,
    /// `progress`: busy, but still interactive.
    Progress,
    /// `wait`: busy.
    Wait,
    /// `cell`: a cell or set of cells may be selected.
    Cell,
    /// `crosshair`: a crosshair.
    Crosshair,
    /// `text`: selectable text (the I-beam).
    Text,
    /// `vertical-text`: selectable vertical text.
    VerticalText,
    /// `alias`: a link or shortcut is to be created.
    Alias,
    /// `copy`: something is to be copied.
    Copy,
    /// `move`: something is to be moved.
    Move,
    /// `no-drop`: the drop is not allowed here.
    NoDrop,
    /// `not-allowed`: the action is not allowed.
    NotAllowed,
    /// `grab`: something can be grabbed.
    Grab,
    /// `grabbing`: something is being grabbed.
    Grabbing,
    /// `all-scroll`: something can scroll in any direction.
    AllScroll,
    /// `col-resize`: a column can be resized horizontally.
    ColResize,
    /// `row-resize`: a row can be resized vertically.
    RowResize,
    /// `n-resize`: the north edge moves.
    NResize,
    /// `e-resize`: the east edge moves.
    EResize,
    /// `s-resize`: the south edge moves.
    SResize,
    /// `w-resize`: the west edge moves.
    WResize,
    /// `ne-resize`: the north-east corner moves.
    NeResize,
    /// `nw-resize`: the north-west corner moves.
    NwResize,
    /// `se-resize`: the south-east corner moves.
    SeResize,
    /// `sw-resize`: the south-west corner moves.
    SwResize,
    /// `ew-resize`: resizable east-west.
    EwResize,
    /// `ns-resize`: resizable north-south.
    NsResize,
    /// `nesw-resize`: resizable along the north-east/south-west diagonal.
    NeswResize,
    /// `nwse-resize`: resizable along the north-west/south-east diagonal.
    NwseResize,
    /// `zoom-in`: something can be zoomed in.
    ZoomIn,
    /// `zoom-out`: something can be zoomed out.
    ZoomOut,
}

impl PointerShape {
    /// Every shape, in a fixed order whose position is [`PointerShape::index`].
    pub const ALL: [PointerShape; 34] = [
        Self::Default,
        Self::ContextMenu,
        Self::Help,
        Self::Pointer,
        Self::Progress,
        Self::Wait,
        Self::Cell,
        Self::Crosshair,
        Self::Text,
        Self::VerticalText,
        Self::Alias,
        Self::Copy,
        Self::Move,
        Self::NoDrop,
        Self::NotAllowed,
        Self::Grab,
        Self::Grabbing,
        Self::AllScroll,
        Self::ColResize,
        Self::RowResize,
        Self::NResize,
        Self::EResize,
        Self::SResize,
        Self::WResize,
        Self::NeResize,
        Self::NwResize,
        Self::SeResize,
        Self::SwResize,
        Self::EwResize,
        Self::NsResize,
        Self::NeswResize,
        Self::NwseResize,
        Self::ZoomIn,
        Self::ZoomOut,
    ];

    /// The canonical CSS name, which a `__current__` query answers.
    pub fn css_name(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::ContextMenu => "context-menu",
            Self::Help => "help",
            Self::Pointer => "pointer",
            Self::Progress => "progress",
            Self::Wait => "wait",
            Self::Cell => "cell",
            Self::Crosshair => "crosshair",
            Self::Text => "text",
            Self::VerticalText => "vertical-text",
            Self::Alias => "alias",
            Self::Copy => "copy",
            Self::Move => "move",
            Self::NoDrop => "no-drop",
            Self::NotAllowed => "not-allowed",
            Self::Grab => "grab",
            Self::Grabbing => "grabbing",
            Self::AllScroll => "all-scroll",
            Self::ColResize => "col-resize",
            Self::RowResize => "row-resize",
            Self::NResize => "n-resize",
            Self::EResize => "e-resize",
            Self::SResize => "s-resize",
            Self::WResize => "w-resize",
            Self::NeResize => "ne-resize",
            Self::NwResize => "nw-resize",
            Self::SeResize => "se-resize",
            Self::SwResize => "sw-resize",
            Self::EwResize => "ew-resize",
            Self::NsResize => "ns-resize",
            Self::NeswResize => "nesw-resize",
            Self::NwseResize => "nwse-resize",
            Self::ZoomIn => "zoom-in",
            Self::ZoomOut => "zoom-out",
        }
    }

    /// The shape a CSS name or an accepted alias names; `None` for any other name, and for any
    /// name longer than 32 bytes.
    pub fn from_name(name: &[u8]) -> Option<Self> {
        if name.len() > MAX_POINTER_SHAPE_NAME_BYTES {
            // When: name is longer than any accepted spelling, it is unknown without a lookup.
            return None;
        }
        Self::ALL.into_iter().find(|shape| shape.css_name().as_bytes() == name).or_else(|| {
            POINTER_SHAPE_ALIASES
                .iter()
                .find(|(alias, _)| alias.as_bytes() == name)
                .map(|(_, shape)| *shape)
        })
    }

    /// This shape's position in [`PointerShape::ALL`].
    pub fn index(self) -> u8 {
        Self::ALL.iter().position(|shape| *shape == self).unwrap_or(0) as u8
    }
}

/// The xterm cursor-font and kitty names accepted beside the CSS names: the union of kitty's and
/// Ghostty's alias lists. They agree on every name but two, resolved by the X11 glyph each names:
/// `cross` is a crosshair (as Ghostty reads it) and `fleur` the four-way move arrow (as kitty does).
const POINTER_SHAPE_ALIASES: [(&str, PointerShape); 47] = [
    ("left_ptr", PointerShape::Default),
    ("arrow", PointerShape::Default),
    ("question_arrow", PointerShape::Help),
    ("whats_this", PointerShape::Help),
    ("hand", PointerShape::Pointer),
    ("hand2", PointerShape::Pointer),
    ("pointing_hand", PointerShape::Pointer),
    ("left_ptr_watch", PointerShape::Progress),
    ("half-busy", PointerShape::Progress),
    ("watch", PointerShape::Wait),
    ("clock", PointerShape::Wait),
    ("plus", PointerShape::Cell),
    ("cross", PointerShape::Crosshair),
    ("tcross", PointerShape::Crosshair),
    ("xterm", PointerShape::Text),
    ("ibeam", PointerShape::Text),
    ("beam", PointerShape::Text),
    ("dnd-link", PointerShape::Alias),
    ("dnd-copy", PointerShape::Copy),
    ("fleur", PointerShape::Move),
    ("dnd-move", PointerShape::Move),
    ("pointer-move", PointerShape::Move),
    ("dnd-no-drop", PointerShape::NoDrop),
    ("crossed_circle", PointerShape::NotAllowed),
    ("forbidden", PointerShape::NotAllowed),
    ("hand1", PointerShape::Grab),
    ("openhand", PointerShape::Grab),
    ("closedhand", PointerShape::Grabbing),
    ("dnd-none", PointerShape::Grabbing),
    ("top_side", PointerShape::NResize),
    ("right_side", PointerShape::EResize),
    ("bottom_side", PointerShape::SResize),
    ("left_side", PointerShape::WResize),
    ("top_right_corner", PointerShape::NeResize),
    ("top_left_corner", PointerShape::NwResize),
    ("bottom_right_corner", PointerShape::SeResize),
    ("bottom_left_corner", PointerShape::SwResize),
    ("sb_h_double_arrow", PointerShape::EwResize),
    ("split_h", PointerShape::EwResize),
    ("sb_v_double_arrow", PointerShape::NsResize),
    ("split_v", PointerShape::NsResize),
    ("size_bdiag", PointerShape::NeswResize),
    ("size-bdiag", PointerShape::NeswResize),
    ("size_fdiag", PointerShape::NwseResize),
    ("size-fdiag", PointerShape::NwseResize),
    ("zoom_in", PointerShape::ZoomIn),
    ("zoom_out", PointerShape::ZoomOut),
];

/// The OSC 22 state of one pane: a stack per screen of shapes or `None` (an unset entry), and the
/// effective shape last published with its change count.
#[derive(Debug)]
pub(super) struct PointerShapeState {
    /// Main screen at 0, alternate at 1; each at most [`POINTER_SHAPE_STACK_MAX`] entries.
    stacks: [Vec<Option<PointerShape>>; 2],
    /// The effective shape at the last [`Performer::note_pointer_shape`].
    published: PointerShape,
    /// Changes of `published`, wrapping; a reader sees a change as a new value.
    generation: u32,
}

impl Default for PointerShapeState {
    fn default() -> Self {
        Self { stacks: [Vec::new(), Vec::new()], published: PointerShape::Text, generation: 0 }
    }
}

impl Performer {
    fn pointer_shape_stack(&self) -> &Vec<Option<PointerShape>> {
        &self.pointer_shape.stacks[usize::from(self.grid.is_alt())]
    }

    fn pointer_shape_stack_mut(&mut self) -> &mut Vec<Option<PointerShape>> {
        let index = usize::from(self.grid.is_alt());
        &mut self.pointer_shape.stacks[index]
    }

    /// The active screen's top shape, or the tracking fallback for an empty stack or an unset top:
    /// `default` while mouse tracking (1000, 1002 or 1003) is on, else `text`.
    fn effective_pointer_shape(&self) -> PointerShape {
        // A tracking mode means the program reads the pointer, so the arrow shows.
        self.pointer_shape_stack().last().copied().flatten().unwrap_or(
            if self.mouse_tracking == MouseTracking::Off {
                PointerShape::Text
            } else {
                PointerShape::Default
            },
        )
    }

    /// Publish the effective shape after anything that may change it (OSC 22, a tracking mode, a
    /// screen switch, RIS); the generation moves only when the shape really changed.
    pub(super) fn note_pointer_shape(&mut self) {
        let effective = self.effective_pointer_shape();
        if effective != self.pointer_shape.published {
            self.pointer_shape.published = effective;
            self.pointer_shape.generation = self.pointer_shape.generation.wrapping_add(1);
        }
    }

    /// Empty both screens' stacks, as RIS does.
    pub(super) fn clear_pointer_shapes(&mut self) {
        for stack in &mut self.pointer_shape.stacks {
            stack.clear();
        }
    }

    /// Apply one complete OSC 22 payload: an optional operator (`=`, `>`, `<` or `?`; none means
    /// `=`) then comma-separated names, of which the first 64 are read. A query answers with the
    /// request's terminator, BEL when `bell_terminated`, and changes nothing.
    pub(super) fn apply_pointer_shape_osc(&mut self, payload: &[u8], bell_terminated: bool) {
        let (operator, names) = match payload.first() {
            Some(operator @ (b'=' | b'>' | b'<' | b'?')) => (*operator, &payload[1..]),
            _ => (b'=', payload),
        };
        let names = names.split(|byte| *byte == b',').take(MAX_POINTER_SHAPE_NAMES);
        match operator {
            b'?' => {
                // When: operator is `?`, every name is answered in one reply and nothing changes.
                let answers: Vec<&str> =
                    names.map(|name| self.pointer_shape_answer(name)).collect();
                let terminator = if bell_terminated { "\x07" } else { "\x1b\\" };
                self.reply(format!("\x1b]22;{}{terminator}", answers.join(",")).as_bytes());
                return;
            }
            b'<' => {
                self.pointer_shape_stack_mut().pop();
            }
            b'>' => {
                // When: operator is `>`, each known name is pushed in order.
                for name in names {
                    let Some(shape) = PointerShape::from_name(name) else {
                        // When: name is empty or unknown, a push skips it; only its length is logged.
                        log_unknown_pointer_shape(name);
                        continue;
                    };
                    let stack = self.pointer_shape_stack_mut();
                    // A full stack drops its bottom entry to make room for the push.
                    if stack.len() == POINTER_SHAPE_STACK_MAX {
                        stack.remove(0);
                    }
                    stack.push(Some(shape));
                }
            }
            _ => {
                // When: operator is `=` or absent, each name in turn replaces the top entry.
                for name in names {
                    let entry = if name.is_empty() {
                        None
                    } else if let Some(shape) = PointerShape::from_name(name) {
                        // When: name is known, it replaces the top entry.
                        Some(shape)
                    } else {
                        // When: name is unknown, the set ignores it; only its length is logged.
                        log_unknown_pointer_shape(name);
                        continue;
                    };
                    let stack = self.pointer_shape_stack_mut();
                    match stack.last_mut() {
                        Some(top) => *top = entry,
                        None => stack.push(entry),
                    }
                }
            }
        }
        self.note_pointer_shape();
    }

    /// One query answer: `1` for an accepted name, the top shape's CSS name (or `0` when unset or
    /// empty) for `__current__`, the fixed `text` and `default` for `__default__` and
    /// `__grabbed__`, and `0` for anything else.
    fn pointer_shape_answer(&self, name: &[u8]) -> &'static str {
        if PointerShape::from_name(name).is_some() {
            // When: from_name accepts name, it is a shape SonicTerm can show.
            return "1";
        }
        match name {
            b"__current__" => self
                .pointer_shape_stack()
                .last()
                .copied()
                .flatten()
                .map_or("0", PointerShape::css_name),
            b"__default__" => PointerShape::Text.css_name(),
            b"__grabbed__" => PointerShape::Default.css_name(),
            _ => "0",
        }
    }
}

/// Log an ignored name by its length only: program text is never logged.
fn log_unknown_pointer_shape(name: &[u8]) {
    if !name.is_empty() {
        tracing::debug!(name_bytes = name.len(), "OSC 22 ignored an unknown pointer shape name");
    }
}

impl Parser {
    /// The pointer shape the pane shows: the active screen's top OSC 22 shape, else `default`
    /// while mouse tracking is on and `text` otherwise.
    pub fn pointer_shape(&self) -> PointerShape {
        self.performer.pointer_shape.published
    }

    /// The shape and its change count in one word for lock-free publication: the generation in
    /// bits 8..32 (wrapping) and [`PointerShape::index`] in bits 0..8. The word changes only when
    /// the shape does.
    pub fn pointer_shape_snapshot(&self) -> u32 {
        let state = &self.performer.pointer_shape;
        (state.generation << 8) | u32::from(state.published.index())
    }
}

/// The shape a word from [`Parser::pointer_shape_snapshot`] carries.
pub fn pointer_shape_of_snapshot(snapshot: u32) -> PointerShape {
    PointerShape::ALL.get((snapshot & 0xff) as usize).copied().unwrap_or(PointerShape::Text)
}

#[cfg(test)]
#[path = "vt_pointer_shape_tests.rs"]
mod vt_pointer_shape_tests;
