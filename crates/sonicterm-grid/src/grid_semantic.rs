//! Per-screen semantic prompt state, driven by the OSC 133 annotations (L, N, P, I and the options
//! on A) and ConEmu's OSC 9;12.
//!
//! This state is separate from the prompt regions in `grid_prompts.rs`: nothing here opens,
//! closes or evicts a region, so MCP readiness and pinned runs never depend on it. Each screen
//! keeps its own state, which moves with the alternate-screen swap as the regions do.

use super::*;

/// Most bytes of decoded command text kept for one screen.
pub const MAX_CMDLINE_BYTES: usize = 1024;

/// The prompt kind an OSC 133 `k=` option names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PromptKind {
    /// `k=i`, or no kind: the first line of a prompt.
    #[default]
    Initial,
    /// `k=r`: a right-aligned prompt.
    Right,
    /// `k=c`: a continuation line the user may edit back into.
    Continuation,
    /// `k=s`: a secondary prompt that cannot be edited back into.
    Secondary,
}

/// Where the screen's shell is, as its annotations last said.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SemanticMode {
    /// Command output, or no prompt yet.
    #[default]
    Output,
    /// A prompt of this kind is being drawn.
    Prompt(PromptKind),
    /// The user's input; `until_eol` input (OSC 133 `I`) ends at the next line feed.
    Input {
        /// The input ends at the next LF, IND or NEL.
        until_eol: bool,
    },
}

/// The OSC 133 `redraw` option: how the shell redraws its prompt after a resize.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PromptRedraw {
    /// `redraw=1`, the default: the shell redraws the whole prompt.
    #[default]
    Full,
    /// `redraw=last`: the shell redraws only the prompt's last line.
    LastLine,
    /// `redraw=0`: the shell does not redraw.
    Off,
}

/// The OSC 133 `click_events` option: which click reports the shell reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClickEvents {
    /// `click_events=1`: clicks at absolute positions.
    Absolute,
    /// `click_events=2`: clicks relative to the prompt.
    Relative,
}

/// The OSC 133 `cl` option: the cursor keys a click may be turned into.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClickLine {
    /// `cl=line`: left and right within one line.
    Line,
    /// `cl=m`: left and right across the lines of one input.
    Multiple,
    /// `cl=v`: up and down too, moving conservatively.
    ConservativeVertical,
    /// `cl=w`: up and down, with the editor doing smart vertical motion.
    SmartVertical,
}

/// The click options an OSC 133 prompt declares; prompt click-to-move reads them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PromptClick {
    /// The `click_events` value, when valid.
    pub events: Option<ClickEvents>,
    /// The `cl` value, when valid.
    pub line_motion: Option<ClickLine>,
    /// `special_key=1`: the shell binds a special key for click motion.
    pub special_key: bool,
}

/// The options the newest A, N or 9;12 declared; each of those resets them first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PromptOptions {
    /// The `redraw` value.
    pub redraw: PromptRedraw,
    /// The click options.
    pub click: PromptClick,
}

/// Decoded command text an authenticated OSC 133 C reported. Program-supplied: never evaluated,
/// and its `Debug` prints only the length, so it cannot reach logs or diagnostics.
#[derive(Clone, PartialEq, Eq)]
pub struct CmdlineText(String);

impl CmdlineText {
    /// `text` as command text, or `None` when it passes [`MAX_CMDLINE_BYTES`].
    pub fn new(text: String) -> Option<Self> {
        // A decoder's buffer can be four times the text; a boxed `str` keeps only the text.
        (text.len() <= MAX_CMDLINE_BYTES).then(|| Self(text.into_boxed_str().into_string()))
    }

    /// The text, for a display that sanitizes it first.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Heap bytes the text holds.
    pub fn capacity(&self) -> usize {
        self.0.capacity()
    }
}

impl std::fmt::Debug for CmdlineText {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "CmdlineText({} bytes)", self.0.len())
    }
}

/// One screen's semantic prompt state.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SemanticState {
    /// What the shell is drawing now.
    pub mode: SemanticMode,
    /// The absolute row the newest prompt started on; `None` before one or once it left history.
    pub anchor: Option<u64>,
    /// Advances with every new prompt, every anchor eviction and RIS, and never repeats on this
    /// screen.
    pub generation: u64,
    /// The newest prompt's options.
    pub options: PromptOptions,
    /// The absolute row of the newest continuation or secondary prompt of this generation.
    pub last_secondary_row: Option<u64>,
    /// The newest region's command text, from an authenticated C.
    pub cmdline: Option<CmdlineText>,
    /// This generation's shell marked input (B or I), so its typed cells carry `PROMPT_INPUT`.
    pub input_seen: bool,
}

impl SemanticState {
    /// Start a new generation anchored at `row`.
    fn start_generation(&mut self, row: u64, kind: PromptKind) {
        self.generation = self.generation.saturating_add(1);
        self.anchor = Some(row);
        self.last_secondary_row = None;
        self.cmdline = None;
        self.input_seen = false;
        self.mode = SemanticMode::Prompt(kind);
    }
}

impl Grid {
    /// The active screen's semantic prompt state.
    pub fn semantic(&self) -> &SemanticState {
        &self.semantic
    }

    /// Record a prompt of `kind` at the cursor row. With `options` (A, N, 9;12) the options are
    /// replaced and a new generation starts; without (P), a continuation or secondary kind under
    /// an anchor only notes its row, and any other kind starts a generation keeping the options.
    pub fn mark_semantic_prompt(&mut self, kind: PromptKind, options: Option<PromptOptions>) {
        let row = self.cursor_absolute_row();
        let secondary = matches!(kind, PromptKind::Continuation | PromptKind::Secondary);
        if options.is_none() && secondary && self.semantic.anchor.is_some() {
            // When: a P continuation or secondary prompt sits under an anchor, it joins that
            // generation instead of starting one.
            self.semantic.last_secondary_row = Some(row);
            self.semantic.mode = SemanticMode::Prompt(kind);
            return;
        }
        // Without an anchor, a continuation or secondary P acts as `k=i`; A, N and 9;12 keep theirs.
        let kind = if secondary && options.is_none() { PromptKind::Initial } else { kind };
        self.semantic.start_generation(row, kind);
        if let Some(options) = options {
            self.semantic.options = options;
        }
    }

    /// Record input starting (B, or I when `until_eol`).
    pub fn mark_semantic_input(&mut self, until_eol: bool) {
        self.semantic.mode = SemanticMode::Input { until_eol };
        self.semantic.input_seen = true;
    }

    /// Record output starting (C or D); the anchor and generation stay until the next prompt.
    pub fn mark_semantic_output(&mut self) {
        self.semantic.mode = SemanticMode::Output;
    }

    /// A line feed, IND or NEL ends input that an I said runs to the end of the line.
    #[inline]
    pub fn end_semantic_line(&mut self) {
        if self.semantic.mode == (SemanticMode::Input { until_eol: true }) {
            self.semantic.mode = SemanticMode::Output;
        }
    }

    /// Replace the newest region's command text.
    pub fn set_semantic_cmdline(&mut self, cmdline: Option<CmdlineText>) {
        self.semantic.cmdline = cmdline;
    }

    /// RIS: back to output with no anchor, options or command text, in a new generation.
    pub fn reset_semantic(&mut self) {
        let generation = self.semantic.generation.saturating_add(1);
        self.semantic = SemanticState { generation, ..SemanticState::default() };
    }

    /// Heap bytes the semantic state holds on this screen.
    pub(super) fn semantic_bytes(&self) -> usize {
        self.semantic.cmdline.as_ref().map_or(0, CmdlineText::capacity)
    }

    /// Rebase the anchor and secondary row after `removed` rows left the top of this screen; an
    /// evicted anchor starts a new generation so a reader can tell its prompt is gone.
    pub(super) fn rebase_semantic(&mut self, removed: u64) {
        let state = &mut self.semantic;
        state.last_secondary_row =
            state.last_secondary_row.and_then(|row| row.checked_sub(removed));
        if let Some(anchor) = state.anchor {
            state.anchor = anchor.checked_sub(removed);
            if state.anchor.is_none() {
                // The anchor's row was removed, so the prompt it named is gone.
                state.generation = state.generation.saturating_add(1);
            }
        }
    }
}

#[cfg(test)]
#[path = "grid_semantic_tests.rs"]
mod grid_semantic_tests;
