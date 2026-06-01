//! Per-Intent reducer arms.
//!
//! **M6a-expand-2b** (THIS PR): leaf-only routing per FINAL spec §9.
//!
//! Leaf intents are those whose translation into Effects is a 1-to-2
//! direct mapping that touches no fan-out state cascade (no pane-tree
//! mutation, no tab/window lifecycle). The reducer arms for these
//! variants emit Effects deterministically from the Intent payload
//! alone — they read `AppState` for context (e.g. clipboard text for
//! Copy), but do not mutate the pane/tab/window topology.
//!
//! Non-leaf intents (NewTab, SplitPane, ClosePane, etc.) still fall
//! through to the empty arm; those land in M6a-expand-2c alongside
//! the full pane-tree migration into `AppState`.
//!
//! Spec §3 mapping table (subset — leaf arms only):
//!
//! | Intent                | Effects                                   |
//! |-----------------------|-------------------------------------------|
//! | PtyWrite              | PtyWrite (1:1 pass-through)               |
//! | PtyBurst              | Render(PtyBurst)                          |
//! | PtyExit               | ChildExitPropagate + PtyClose             |
//! | Key (pressed)         | Render(UserInput) (encoded write happens  |
//! |                       |   at the platform boundary)               |
//! | ImeCommit             | PtyWrite (text bytes) + Render(Ime)       |
//! | ImePreedit            | Render(Ime)                               |
//! | ImeStart / ImeEnd     | Render(Ime)                               |
//! | Paste                 | PtyWrite (bracketed-wrapped at boundary)  |
//! | CopySelection         | ClipboardSet (text fetched by boundary)   |
//! | ClickUrl              | OpenURL                                   |
//! | HoverUrl              | Render(Hover)                             |
//! | ScrollUp/Down/Page*/  | Render(Scroll) (pane scroll mutation      |
//! |   ToTop/ToBottom/     |   happens at boundary in 2b; the          |
//! |   ToCursor            |   Render(Scroll) is the gate that the     |
//! |                       |   boundary observes)                      |
//! | MouseWheel            | Render(Scroll)                            |
//! | FontSizeDelta         | Render(ConfigReload)                      |
//! | ApplyTheme            | Render(ConfigReload)                      |
//! | ConfigChanged         | Render(ConfigReload)                      |
//! | FilesDropped          | (no effect — platform path handles)       |
//! | RedrawRequested       | Render(Vsync)                             |
//! | Tick                  | (no effect — clock only)                  |
//! | Exit                  | Quit                                      |
//!
//! All other variants (window/tab/pane lifecycle, selection, search,
//! palette, broadcast, OS drag) deliberately fall through to the
//! empty arm pending 2c. The contract tests in
//! `tests/intent_stubs.rs` track which arms remain stubs by
//! asserting `out.is_empty()` for the unrouted variants.

use smallvec::SmallVec;

use crate::app_state::AppState;
use crate::effect::AppEffect;
use crate::intent::{AppIntent, RedrawReason};

/// Route a single Intent through the leaf reducer, appending zero or
/// more Effects to `out`. Does not sort — `AppStateMachine::handle`
/// applies the spec §6 class ordering after this returns.
pub(crate) fn reduce_leaf(
    _state: &mut AppState,
    intent: AppIntent,
    out: &mut SmallVec<[AppEffect; 4]>,
) {
    match intent {
        // ── PTY leaf ────────────────────────────────────────────────
        AppIntent::PtyWrite { pane, bytes } => {
            out.push(AppEffect::PtyWrite { pane, data: bytes });
        }
        AppIntent::PtyBurst { pane: _, generation: _ } => {
            // Render the affected window. The platform boundary owns
            // the pane→window map (it lives in `App.windows` / the
            // pane tree); for 2b we emit a "best-known" Render with
            // a sentinel window id of 0 — the boundary's
            // `dispatch_effects` ignores the window field on Render
            // and uses its frontmost-window discriminator. 2c will
            // route this through `AppState` once panes migrate.
            out.push(AppEffect::Render {
                window: sonicterm_types::WindowKey::new(0),
                reason: RedrawReason::PtyBurst,
            });
        }
        AppIntent::PtyExit { pane, status } => {
            out.push(AppEffect::ChildExitPropagate { pane, status });
            out.push(AppEffect::PtyClose { pane });
        }

        // ── Keyboard / IME leaf ─────────────────────────────────────
        AppIntent::Key { window, code: _, mods: _, pressed } => {
            // The actual byte encoding stays at the platform boundary
            // until 2c (keymap.rs is winit-flavoured). Emit a Render
            // so the cursor blink resets immediately on key down.
            if pressed {
                out.push(AppEffect::Render { window, reason: RedrawReason::UserInput });
            }
        }
        AppIntent::ImeCommit { window, text } => {
            // Per spec §3: commit goes to the focused pane's PTY.
            // Pane is implicit (focused at write time); 2b boundary
            // resolves it. We carry the bytes verbatim.
            // Use pane sentinel 0 — boundary translates to focused pane.
            out.push(AppEffect::PtyWrite {
                pane: crate::supporting::PaneId(0),
                data: text.into_bytes().into(),
            });
            out.push(AppEffect::Render { window, reason: RedrawReason::Ime });
        }
        AppIntent::ImePreedit { window, .. }
        | AppIntent::ImeStart { window }
        | AppIntent::ImeEnd { window } => {
            out.push(AppEffect::Render { window, reason: RedrawReason::Ime });
        }

        // ── Clipboard leaf ──────────────────────────────────────────
        AppIntent::CopySelection { window: _ } => {
            // 2b: the actual selection text resolution happens at the
            // boundary (selection lives on WindowState). We emit a
            // ClipboardSet sentinel with an empty payload; the
            // boundary's `dispatch_effects` substitutes the real
            // selected text it just resolved. This keeps the Effect
            // surface stable even though AppState doesn't carry the
            // selection yet.
            out.push(AppEffect::ClipboardSet { text: String::new() });
        }
        AppIntent::Paste { window: _, text, bracketed: _ } => {
            out.push(AppEffect::PtyWrite {
                pane: crate::supporting::PaneId(0),
                data: text.into_bytes().into(),
            });
        }

        // ── Scroll leaf — emit Render(Scroll); scroll mutation
        // happens at the boundary in 2b (scroll lives on the
        // grid/pane, not AppState). 2c lifts it into AppState. ─────
        AppIntent::ScrollUp { window, .. }
        | AppIntent::ScrollDown { window, .. }
        | AppIntent::ScrollPageUp { window }
        | AppIntent::ScrollPageDown { window }
        | AppIntent::ScrollToTop { window }
        | AppIntent::ScrollToBottom { window }
        | AppIntent::ScrollToCursor { window } => {
            out.push(AppEffect::Render { window, reason: RedrawReason::Scroll });
        }

        // ── Mouse wheel — leaf in 2b (scroll dispatch at boundary). ─
        AppIntent::MouseWheel { window, .. } => {
            out.push(AppEffect::Render { window, reason: RedrawReason::Scroll });
        }

        // ── Hyperlinks leaf ─────────────────────────────────────────
        AppIntent::ClickUrl { window: _, url } => {
            out.push(AppEffect::OpenURL { url });
        }
        AppIntent::HoverUrl { window, .. } => {
            out.push(AppEffect::Render { window, reason: RedrawReason::Hover });
        }

        // ── Config / theming leaf ───────────────────────────────────
        AppIntent::FontSizeDelta { .. } | AppIntent::ApplyTheme { .. } => {
            out.push(AppEffect::Render {
                window: sonicterm_types::WindowKey::new(0),
                reason: RedrawReason::ConfigReload,
            });
        }
        AppIntent::ConfigChanged { .. } => {
            out.push(AppEffect::Render {
                window: sonicterm_types::WindowKey::new(0),
                reason: RedrawReason::ConfigReload,
            });
        }

        // ── Frame timing leaf ───────────────────────────────────────
        AppIntent::RedrawRequested { window } => {
            out.push(AppEffect::Render { window, reason: RedrawReason::Vsync });
        }
        AppIntent::Exit => {
            out.push(AppEffect::Quit);
        }

        // ── Non-leaf — stubs (full reducer arms land in 2c) ─────────
        AppIntent::NewWindow { .. }
        | AppIntent::WindowCloseRequested { .. }
        | AppIntent::WindowFocused { .. }
        | AppIntent::WindowBlurred { .. }
        | AppIntent::WindowResized { .. }
        | AppIntent::WindowMoved { .. }
        | AppIntent::NewTab { .. }
        | AppIntent::CloseTab { .. }
        | AppIntent::NextTab { .. }
        | AppIntent::PrevTab { .. }
        | AppIntent::GoToTab { .. }
        | AppIntent::TearOutTab { .. }
        | AppIntent::SplitPane { .. }
        | AppIntent::ClosePane { .. }
        | AppIntent::ResizePane { .. }
        | AppIntent::FocusPaneLeft { .. }
        | AppIntent::FocusPaneRight { .. }
        | AppIntent::FocusPaneUp { .. }
        | AppIntent::FocusPaneDown { .. }
        | AppIntent::ForegroundProcChanged { .. }
        | AppIntent::MouseButton { .. }
        | AppIntent::MouseMove { .. }
        | AppIntent::SelectionStart { .. }
        | AppIntent::SelectionExtend { .. }
        | AppIntent::SelectionEnd { .. }
        | AppIntent::ClearSelection { .. }
        | AppIntent::OpenSearch { .. }
        | AppIntent::SearchQuery { .. }
        | AppIntent::SearchStep { .. }
        | AppIntent::CloseSearch { .. }
        | AppIntent::ToggleCommandPalette { .. }
        | AppIntent::PaletteFilter { .. }
        | AppIntent::PaletteStep { .. }
        | AppIntent::PaletteSubmit { .. }
        | AppIntent::OsDragOutcome(_)
        | AppIntent::FilesDropped { .. }
        | AppIntent::SetBroadcastScope { .. }
        | AppIntent::Tick { .. } => {
            // Intentionally empty. M6a-expand-2c.
        }
    }
}
