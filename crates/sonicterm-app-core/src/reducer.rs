//! Reducer arms for all application intent families.
//!
//! Direct intents translate to effects, while lifecycle and interaction
//! intents also update the pure-data `AppState` mirror. Authoritative live
//! window/tab/pane resources remain in `sonicterm-app`; the reducer records
//! deterministic transitions and emits effects for the boundary to execute.
//!
//! Multi-effect operations append their complete batch here. The public state
//! machine then applies stable effect-class ordering before returning it.

use smallvec::SmallVec;

use crate::app_state::AppState;
use crate::effect::AppEffect;
use crate::intent::{AppIntent, RedrawReason};
use crate::supporting::LogicalSize;

/// Reduce one intent, appending zero or more effects to `out`. This function
/// does not sort; `AppStateMachine::handle` applies canonical class ordering
/// after it returns.
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
            // pane tree), so this emits a "best-known" Render with
            // a sentinel window id of 0 — the boundary's
            // `dispatch_effects` ignores the window field on Render
            // and uses its frontmost-window discriminator.
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
            // The actual byte encoding stays at the platform boundary,
            // since `keymap.rs` is winit-flavoured. Emit a Render
            // so the cursor blink resets immediately on key down.
            if pressed {
                out.push(AppEffect::Render { window, reason: RedrawReason::UserInput });
            }
        }
        AppIntent::ImeCommit { window, text } => {
            // Commit goes to the focused pane's PTY. The pane is
            // implicit (focused at write time) and the boundary
            // resolves it, so the bytes are carried verbatim against
            // pane sentinel 0.
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
            // The actual selection text resolution happens at the
            // boundary (selection lives on WindowState). We emit a
            // ClipboardSet sentinel with an empty payload; the
            // boundary's `dispatch_effects` substitutes the real
            // selected text it just resolved. This keeps the Effect
            // surface stable even though AppState does not carry the
            // selection.
            out.push(AppEffect::ClipboardSet { text: String::new() });
        }
        AppIntent::Paste { window: _, text, bracketed: _ } => {
            out.push(AppEffect::PtyWrite {
                pane: crate::supporting::PaneId(0),
                data: text.into_bytes().into(),
            });
        }

        // ── Scroll leaf — emit Render(Scroll); scroll mutation
        // happens at the boundary, since scroll lives on the
        // grid/pane rather than AppState. ─────────────────────────
        AppIntent::ScrollUp { window, .. }
        | AppIntent::ScrollDown { window, .. }
        | AppIntent::ScrollPageUp { window }
        | AppIntent::ScrollPageDown { window }
        | AppIntent::ScrollToTop { window }
        | AppIntent::ScrollToBottom { window }
        | AppIntent::ScrollToCursor { window } => {
            out.push(AppEffect::Render { window, reason: RedrawReason::Scroll });
        }

        // ── Mouse wheel — scroll dispatch happens at the boundary. ──
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

        // ── Window lifecycle ────────────────────────────────────────
        //
        // Intent → effect mapping:
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

        // ── Tab lifecycle ───────────────────────────────────────────
        //
        // Intent → effect mapping:
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
        // `tab_count` / `active_tab_idx` track the focused window
        // only — the boundary in
        // `sonicterm-app::app::WindowState.tabs` remains
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
                    _state.active_tab_idx = if _state.tab_count == 0 {
                        None
                    } else {
                        // When: tab_count still holds tabs the tracker steps back
                        // one slot so the next activate reads as a real change.
                        Some(cur.saturating_sub(1))
                    };
                }
                Some(cur) if cur > idx => {
                    // Indices above the removed one shift down by one.
                    _state.active_tab_idx = Some(cur - 1);
                }
                _ => {
                    // When: active_tab_idx is unset, or idx sits after the closed
                    // tab, the tracker is left exactly as it was.
                }
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
                // When: tab_count is 1 with an unset active_tab_idx the tracker
                // adopts the only slot; no switch occurred, so no Render.
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
                // When: tab_count is 1 with an unset active_tab_idx the tracker
                // adopts the only slot; no switch occurred, so no Render.
                _state.active_tab_idx = Some(0);
            }
        }
        AppIntent::GoToTab { window, idx } => {
            // When: GoToTab names a slot past the end it is clamped to the last
            // valid tab, matching the boundary's saturating `tabs.activate(i)`.

            let n = _state.tab_count as usize;
            if n == 0 {
                // When: n is zero there is no tab to activate, so the intent is
                // dropped without touching the tracker or emitting a Render.
                return;
            }
            let clamped = idx.min(n - 1);
            if _state.active_tab_idx != Some(clamped) {
                _state.active_tab_idx = Some(clamped);
                out.push(AppEffect::Render { window, reason: RedrawReason::TabSwitch });
            }
        }
        AppIntent::TearOutTab { src_window, src_tab } => {
            // Source window loses one tab; a fresh top-level window
            // is opened to host it. The boundary's `tear_out_tab`
            // path then re-issues a `NewTab` Intent on the new
            // window once winit has returned its WindowId.
            //
            // The WindowOpen cascade is emitted in the same batch so
            // consumers observe both halves of the tear-out in a single
            // `handle()` call. The reducer has no access to the
            // state-machine's `pending` queue, so the `out` batch keeps
            // that contract observable without changing the signature.
            _state.tab_count = _state.tab_count.saturating_sub(1);
            match _state.active_tab_idx {
                Some(cur) if cur == src_tab => {
                    _state.active_tab_idx = if _state.tab_count == 0 {
                        None
                    } else {
                        // When: tab_count still holds tabs the tracker steps back
                        // one slot so the next activate reads as a real change.
                        Some(cur.saturating_sub(1))
                    };
                }
                Some(cur) if cur > src_tab => {
                    _state.active_tab_idx = Some(cur - 1);
                }
                _ => {
                    // When: active_tab_idx is unset, or src_tab sits after the
                    // active tab, the tracker survives the tear-out unchanged.
                }
            }
            _state.live_window_count = _state.live_window_count.saturating_add(1);
            out.push(AppEffect::Render { window: src_window, reason: RedrawReason::TabRemoved });
            out.push(AppEffect::WindowOpen {
                role: crate::supporting::WindowRole::Child,
                initial_size: None,
            });
        }

        // ── Pane lifecycle / navigation ─────────────────────────────
        //
        // Intent → effect mapping:
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
                // When: pane_count still holds leaves the focus tracker is
                // clamped to the last surviving index rather than cleared.
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

        // ── Mouse ───────────────────────────────────────────────────
        //
        // Intent → effect mapping:
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
                // When: is_left is false the press is right, middle, or extra, so
                // UserInput lets the boundary repaint a paste or context menu.
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

        AppIntent::FilesDropped { .. } | AppIntent::Tick { .. } => {
            // When: the intent is FilesDropped or Tick it is record-only, so the
            // reducer mutates no state and emits no effect.
        }

        // ── ForegroundProcChanged ───────────────────────────────────
        //
        // Emits Render(TitleOrTab) when the process name actually
        // changed (transition-guarded like WindowFocused). The
        // boundary's per-pane proc snapshot remains source-of-truth.
        AppIntent::ForegroundProcChanged { pane: _, name } => {
            if _state.fg_proc_name != name {
                _state.fg_proc_name = name;
                out.push(AppEffect::Render {
                    window: sonicterm_types::WindowKey::new(0),
                    reason: RedrawReason::TitleOrTab,
                });
            }
        }

        // ── Selection ───────────────────────────────────────────────
        //
        // Every selection mutation emits Render(Selection).
        // Start/End/Clear additionally flip `selection_active`.
        // Extend always emits while a selection is active
        // (drag-extend repaint gate).
        AppIntent::SelectionStart { window, anchor: _, mode: _ } => {
            _state.selection_active = true;
            out.push(AppEffect::Render { window, reason: RedrawReason::Selection });
        }
        AppIntent::SelectionExtend { window, to: _ } => {
            if _state.selection_active {
                out.push(AppEffect::Render { window, reason: RedrawReason::Selection });
            }
        }
        AppIntent::SelectionEnd { window } => {
            if _state.selection_active {
                _state.selection_active = false;
                out.push(AppEffect::Render { window, reason: RedrawReason::Selection });
            }
        }
        AppIntent::ClearSelection { window } => {
            if _state.selection_active {
                _state.selection_active = false;
                out.push(AppEffect::Render { window, reason: RedrawReason::Selection });
            }
        }

        // ── Search overlay ──────────────────────────────────────────
        //
        // Open/Close are transition-guarded (Render(Overlay) only on
        // the actual open/close). Query and Step always emit while
        // the overlay is open (search-result repaint gate).
        AppIntent::OpenSearch { window } => {
            if !_state.search_open {
                _state.search_open = true;
                out.push(AppEffect::Render { window, reason: RedrawReason::Overlay });
            }
        }
        AppIntent::CloseSearch { window } => {
            if _state.search_open {
                _state.search_open = false;
                out.push(AppEffect::Render { window, reason: RedrawReason::Overlay });
            }
        }
        AppIntent::SearchQuery { window, q: _ } | AppIntent::SearchStep { window, forward: _ } => {
            if _state.search_open {
                out.push(AppEffect::Render { window, reason: RedrawReason::Overlay });
            }
        }

        // ── Command palette ─────────────────────────────────────────
        //
        // Toggle flips `palette_open` and emits Render(Overlay) on
        // every transition. Filter/Step emit while open. Submit
        // closes the palette (emits Overlay) and the cascaded Intent
        // the choice translates to arrives as a separate
        // dispatch_intent from the boundary's palette handler
        // (see overlays.rs).
        AppIntent::ToggleCommandPalette { window } => {
            _state.palette_open = !_state.palette_open;
            out.push(AppEffect::Render { window, reason: RedrawReason::Overlay });
        }
        AppIntent::PaletteFilter { window, filter: _ }
        | AppIntent::PaletteStep { window, delta: _ } => {
            if _state.palette_open {
                out.push(AppEffect::Render { window, reason: RedrawReason::Overlay });
            }
        }
        AppIntent::PaletteSubmit { window, choice: _ } => {
            if _state.palette_open {
                _state.palette_open = false;
                out.push(AppEffect::Render { window, reason: RedrawReason::Overlay });
            }
        }

        // ── OS drag outcome ─────────────────────────────────────────
        //
        // The drag completes (committed or not). Emit `OsDragEnd`
        // so the boundary's pending-drag table can settle.
        AppIntent::OsDragOutcome(outcome) => {
            out.push(AppEffect::OsDragEnd {
                src_window: outcome.src_window,
                committed: outcome.committed,
            });
        }

        // ── Broadcast scope ─────────────────────────────────────────
        //
        // Changing scope re-paints the title / tab strip (broadcast
        // indicator glyph). Transition-guarded — no-op set emits
        // nothing.
        AppIntent::SetBroadcastScope { scope } => {
            if _state.broadcast_scope != scope {
                _state.broadcast_scope = scope;
                out.push(AppEffect::Render {
                    window: sonicterm_types::WindowKey::new(0),
                    reason: RedrawReason::TitleOrTab,
                });
            }
        }
    }
}

#[cfg(test)]
#[path = "reducer_tests.rs"]
mod reducer_tests;
