//! Per-Intent reducer arms.
//!
//! **M6a-expand-2b** (prior): leaf-only routing per FINAL spec §9.
//!
//! **M6a-expand-2c-window** (THIS PR): adds the six window-lifecycle
//! arms (NewWindow / WindowCloseRequested / WindowFocused /
//! WindowBlurred / WindowResized / WindowMoved). These mutate
//! `AppState::{focused_window, last_window_pos, cols, rows,
//! live_window_count}` and emit the corresponding window-class
//! Effects per spec §3.
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
use crate::supporting::LogicalSize;

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

        // ── Window lifecycle (M6a-expand-2c-window) ─────────────────
        //
        // Per FINAL spec §3:
        //   NewWindow           → WindowOpen + (deferred MenubarUpdate)
        //   WindowCloseRequested→ WindowClose [+ Quit if last]
        //   WindowFocused       → Render(Focus) (only on transition)
        //   WindowBlurred       → Render(Focus) (only on transition)
        //   WindowResized       → Render(Resize) + grid-size mutation
        //   WindowMoved         → record only (no Effects; OS already
        //                         repositioned the surface)
        AppIntent::NewWindow { role } => {
            _state.live_window_count = _state.live_window_count.saturating_add(1);
            out.push(AppEffect::WindowOpen { role, initial_size: None });
        }
        AppIntent::WindowCloseRequested { window } => {
            // Decrement (saturating: the boundary may double-fire on
            // some platforms; never wrap below zero).
            _state.live_window_count = _state.live_window_count.saturating_sub(1);
            if _state.focused_window == Some(window) {
                _state.focused_window = None;
            }
            out.push(AppEffect::WindowClose { window });
            if _state.live_window_count == 0 {
                // Last window — cascade a Quit. The boundary's
                // `quit_on_last_window_close = false` policy is
                // honoured at dispatch time (it suppresses the
                // platform exit and re-opens a fresh main window
                // instead); the reducer always emits the intent so
                // the contract is observable.
                out.push(AppEffect::Quit);
            }
        }
        AppIntent::WindowFocused { window } => {
            if _state.focused_window != Some(window) {
                _state.focused_window = Some(window);
                out.push(AppEffect::Render { window, reason: RedrawReason::Focus });
            }
        }
        AppIntent::WindowBlurred { window } => {
            if _state.focused_window == Some(window) {
                _state.focused_window = None;
                out.push(AppEffect::Render { window, reason: RedrawReason::Focus });
            }
        }
        AppIntent::WindowResized { window, cols, rows } => {
            _state.cols = u32::from(cols);
            _state.rows = u32::from(rows);
            out.push(AppEffect::Render { window, reason: RedrawReason::Resize });
            // Echo a programmatic resize Effect so the boundary can
            // re-publish the canonical size to its renderer / tab
            // strip. The boundary already resized the wgpu surface in
            // response to the underlying winit `Resized` event; the
            // Effect here is the observable contract surface.
            out.push(AppEffect::WindowResize {
                window,
                size: LogicalSize { width: f64::from(cols), height: f64::from(rows) },
            });
        }
        AppIntent::WindowMoved { window: _, pos } => {
            _state.last_window_pos = Some(pos);
            // No Effects: the OS already moved the window. Recording
            // the position is enough for future reducer arms (e.g.
            // session-restore) to read it.
        }

        // ── Tab lifecycle (M6a-expand-2c-tab) ───────────────────────
        //
        // Per FINAL spec §3:
        //   NewTab        → Render(TabAdded)   + tab_count++ + active_tab_idx = new_idx
        //   CloseTab      → Render(TabRemoved) + tab_count-- + active_tab_idx reset if matched
        //   NextTab       → Render(TabSwitch)  + active_tab_idx = (cur+1) % tab_count
        //   PrevTab       → Render(TabSwitch)  + active_tab_idx = (cur-1) % tab_count
        //   GoToTab       → Render(TabSwitch)  iff idx differs from current (and in-range)
        //   TearOutTab    → Render(TabRemoved) + tab_count-- in source window
        //                   (the destination NewWindow + NewTab cascade lands separately;
        //                   the boundary's `os_drag` path drives the new-window creation
        //                   in its own dispatch_intent call)
        //
        // Multi-window tab state lifts in 2c-pane (`AppState` will own
        // a per-WindowKey tab vector). Until then `tab_count` /
        // `active_tab_idx` track the focused window only — the
        // boundary in `sonicterm-app::app::WindowState.tabs` remains
        // source-of-truth for actual tab content + the visible strip.
        AppIntent::NewTab { window, cwd: _ } => {
            _state.tab_count = _state.tab_count.saturating_add(1);
            // New tab becomes the active one (matches the boundary
            // behaviour in `App::new_tab` / `spawn_tab_in_child`).
            let new_idx = _state.tab_count.saturating_sub(1) as usize;
            _state.active_tab_idx = Some(new_idx);
            out.push(AppEffect::Render { window, reason: RedrawReason::TabAdded });
        }
        AppIntent::CloseTab { window, idx } => {
            _state.tab_count = _state.tab_count.saturating_sub(1);
            // If we closed the active tab, the boundary picks a new
            // active index; we conservatively clamp/clear our tracker
            // so the next switch/activate is observable as a real
            // transition (not a no-op).
            match _state.active_tab_idx {
                Some(cur) if cur == idx => {
                    _state.active_tab_idx =
                        if _state.tab_count == 0 { None } else { Some(cur.saturating_sub(1)) };
                }
                Some(cur) if cur > idx => {
                    // Indices above the removed one shift down by one.
                    _state.active_tab_idx = Some(cur - 1);
                }
                _ => {}
            }
            out.push(AppEffect::Render { window, reason: RedrawReason::TabRemoved });
        }
        AppIntent::NextTab { window } => {
            if _state.tab_count > 1 {
                let cur = _state.active_tab_idx.unwrap_or(0);
                let next = (cur + 1) % (_state.tab_count as usize);
                _state.active_tab_idx = Some(next);
                out.push(AppEffect::Render { window, reason: RedrawReason::TabSwitch });
            } else if _state.tab_count == 1 && _state.active_tab_idx.is_none() {
                _state.active_tab_idx = Some(0);
            }
        }
        AppIntent::PrevTab { window } => {
            if _state.tab_count > 1 {
                let n = _state.tab_count as usize;
                let cur = _state.active_tab_idx.unwrap_or(0);
                let prev = (cur + n - 1) % n;
                _state.active_tab_idx = Some(prev);
                out.push(AppEffect::Render { window, reason: RedrawReason::TabSwitch });
            } else if _state.tab_count == 1 && _state.active_tab_idx.is_none() {
                _state.active_tab_idx = Some(0);
            }
        }
        AppIntent::GoToTab { window, idx } => {
            // Out-of-range: drop silently (matches boundary's
            // saturating `tabs.activate(i)` — clamps to last valid).
            let n = _state.tab_count as usize;
            if n == 0 {
                return;
            }
            let clamped = idx.min(n - 1);
            if _state.active_tab_idx != Some(clamped) {
                _state.active_tab_idx = Some(clamped);
                out.push(AppEffect::Render { window, reason: RedrawReason::TabSwitch });
            }
        }
        AppIntent::TearOutTab { src_window, src_tab } => {
            // Source window loses one tab. The destination NewWindow
            // + NewTab cascade lands as separate dispatch_intent
            // calls from the os_drag boundary.
            _state.tab_count = _state.tab_count.saturating_sub(1);
            // Adjust active_tab_idx the same way CloseTab does — the
            // tab effectively leaves the strip.
            match _state.active_tab_idx {
                Some(cur) if cur == src_tab => {
                    _state.active_tab_idx =
                        if _state.tab_count == 0 { None } else { Some(cur.saturating_sub(1)) };
                }
                Some(cur) if cur > src_tab => {
                    _state.active_tab_idx = Some(cur - 1);
                }
                _ => {}
            }
            out.push(AppEffect::Render { window: src_window, reason: RedrawReason::TabRemoved });
        }

        // ── Pane lifecycle / navigation (M6a-expand-2c-pane) ────────
        //
        // Per FINAL spec §3:
        //   SplitPane         → Render(Layout)  + pane_count++ + focus = new
        //   ClosePane         → Render(Layout)  + pane_count-- + focus clamp
        //   ResizePane        → Render(Layout)  (no count mutation)
        //   FocusPaneLeft     → Render(Focus)   (only on transition; we
        //                       conservatively emit since the boundary
        //                       owns the geometry — see note below)
        //   FocusPaneRight    → Render(Focus)
        //   FocusPaneUp       → Render(Focus)
        //   FocusPaneDown     → Render(Focus)
        //
        // The reducer tracks a flat `pane_count` + `focused_pane_idx`
        // pair — *not* a pane tree. The boundary's
        // `WindowState.tab_states[..].tree` remains source-of-truth for
        // the actual geometry and the focused-leaf id. Directional
        // focus Intents therefore can't resolve the *target* leaf in
        // pure reducer land; we emit `Render(Focus)` unconditionally
        // when `pane_count >= 2` so the boundary can re-paint, and
        // leave `focused_pane_idx` untouched (the boundary's
        // `focus_pane_dir` mutates the canonical tree and the reducer
        // catches up via the next SplitPane/ClosePane Intent). With a
        // single pane, directional focus is a no-op.
        AppIntent::SplitPane { window, dir: _ } => {
            _state.pane_count = _state.pane_count.saturating_add(1);
            // The split makes the *new* leaf the focused pane. Index
            // is the new last leaf (count - 1 after increment), but
            // pre-split count was 0 means this is also the first pane
            // — boundary's `spawn_pane`/`split_active` both end up
            // focusing the new leaf.
            let new_idx = _state.pane_count.saturating_sub(1) as usize;
            _state.focused_pane_idx = Some(new_idx);
            out.push(AppEffect::Render { window, reason: RedrawReason::Layout });
        }
        AppIntent::ClosePane { window } => {
            _state.pane_count = _state.pane_count.saturating_sub(1);
            // If the active was the last leaf, drop to previous; if
            // none remain, clear the focus tracker.
            _state.focused_pane_idx = if _state.pane_count == 0 {
                None
            } else {
                let cur = _state.focused_pane_idx.unwrap_or(0);
                let max = (_state.pane_count as usize).saturating_sub(1);
                Some(cur.min(max))
            };
            out.push(AppEffect::Render { window, reason: RedrawReason::Layout });
        }
        AppIntent::ResizePane { window, dir: _, cells: _ } => {
            // Resize doesn't change topology — pane_count and
            // focused_pane_idx are stable. Emit Render(Layout) so the
            // boundary re-paints with the new split fraction.
            if _state.pane_count >= 2 {
                out.push(AppEffect::Render { window, reason: RedrawReason::Layout });
            }
        }
        AppIntent::FocusPaneLeft { window }
        | AppIntent::FocusPaneRight { window }
        | AppIntent::FocusPaneUp { window }
        | AppIntent::FocusPaneDown { window } => {
            if _state.pane_count >= 2 {
                out.push(AppEffect::Render { window, reason: RedrawReason::Focus });
            }
        }

        // ── Mouse (M6a-expand-2c-mouse) ─────────────────────────────
        //
        // Per FINAL spec §3:
        //   MouseButton(pressed,Left)  → Render(Selection) (transition;
        //                                 boundary owns selection geom)
        //                              + tracks `mouse_left_down`
        //   MouseButton(released,Left) → Render(Selection) (transition)
        //                              + clears `mouse_left_down`
        //   MouseButton(non-Left)      → Render(UserInput) (right/middle
        //                                 click — boundary translates to
        //                                 paste / context menu)
        //   MouseMove                  → Render(Hover) IFF the position
        //                                 differs from the last reported
        //                                 one (implicit coalescer — same
        //                                 shape as WindowFocused's
        //                                 transition-guard pattern).
        //                                 Tracks `last_mouse_pos`.
        //
        // The boundary's `WindowState.{mouse_down, cursor_pos, selection,
        // drag_session}` remain source-of-truth for the actual hit-tests
        // (tab drag, selection extend, scrollbar drag, OSC8 hover); the
        // reducer's job here is the observability + dedupe surface.
        // MouseWheel + HoverUrl were routed in 2b and stay there.
        AppIntent::MouseButton { window, pressed, button, mods: _, pos } => {
            _state.last_mouse_pos = Some(pos);
            let is_left = matches!(button, crate::supporting::MouseButton::Left);
            if is_left {
                // Only emit on transition — same shape as WindowFocused.
                if _state.mouse_left_down != pressed {
                    _state.mouse_left_down = pressed;
                    out.push(AppEffect::Render { window, reason: RedrawReason::Selection });
                }
            } else {
                // Right / middle / extra: emit UserInput so the boundary
                // can repaint a freshly-pasted region or a context menu
                // affordance immediately.
                out.push(AppEffect::Render { window, reason: RedrawReason::UserInput });
            }
        }
        AppIntent::MouseMove { window, pos } => {
            // Implicit coalescer: only emit when the cursor actually
            // moved. winit fires CursorMoved on every device tick even
            // if the integer pixel position is unchanged (sub-pixel
            // jitter on Retina), so the LogicalPos equality check
            // collapses the burst into a single Render per frame in
            // the common case. Drag-extend repaints still flow through
            // the boundary's selection-extend path; the reducer's
            // Render(Hover) is the URL/scrollbar/tab-close affordance
            // gate.
            if _state.last_mouse_pos != Some(pos) {
                _state.last_mouse_pos = Some(pos);
                out.push(AppEffect::Render { window, reason: RedrawReason::Hover });
            }
        }

        // ── Non-leaf — stubs (full reducer arms land in 2c-misc) ────
        AppIntent::ForegroundProcChanged { .. }
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
