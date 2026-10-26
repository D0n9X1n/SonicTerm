//! Memoized hover lookup, completion application and window-local feedback.

use super::*;

impl App {
    /// Refreshes pointer feedback without treating a busy parser as a missing target.
    pub(in crate::app) fn refresh_target_hover(&mut self, window_id: WindowId) {
        let lookup = match self.pointer_target_cell(window_id) {
            Some((pane_id, row, col)) => self.hover_lookup(window_id, pane_id, row, col),
            None => Ok(None),
        };
        self.apply_target_lookup(window_id, lookup);
    }

    /// Resolves frame hover from the same held parser snapshots that will be presented.
    pub(in crate::app) fn refresh_target_hover_from_parsers<'a>(
        &mut self,
        window_id: WindowId,
        parsers: impl IntoIterator<Item = (u64, &'a sonicterm_vt::vt::Parser)>,
    ) {
        let target = self.pointer_target_cell(window_id).and_then(|(pane_id, row, col)| {
            let (_, parser) = parsers.into_iter().find(|(id, _)| *id == pane_id)?;
            let pane = self.windows.get(&window_id)?.panes.get(&pane_id)?;
            let viewport = pane.resolved_viewport(parser.grid());
            self.hover_target_from_parser(window_id, pane_id, row, col, parser, viewport)
        });
        self.apply_target_hover(window_id, target);
    }

    /// An event-time hover lookup of one pane cell: `Err` when the pane's parser is busy, so the
    /// caller keeps its feedback instead of treating contention as a missing target.
    pub(in crate::app) fn hover_lookup(
        &mut self,
        window_id: WindowId,
        pane_id: u64,
        viewport_row: u16,
        col: u16,
    ) -> Result<Option<CellTargetSnapshot>, ()> {
        let Some(pane) = self.windows.get(&window_id).and_then(|window| window.panes.get(&pane_id))
        else {
            // When: the owning pane is gone, this is genuine absence rather than transient parser contention.
            return Ok(None);
        };
        // The pane's parser handle outlives the borrow of `windows`, so the lookup can update the
        // window's hover state while the guard is held.
        let parser_handle = Arc::clone(&pane.parser);
        let parser = parser_handle.try_lock().ok_or(())?;
        let viewport = pane.resolved_viewport(parser.grid());
        Ok(self.hover_target_from_parser(window_id, pane_id, viewport_row, col, &parser, viewport))
    }

    /// Detect the hover target at one displayed pane cell and count the work. Hover goes through
    /// here; a click computes its own fresh target through [`Self::cell_target_at`].
    pub(super) fn hover_target_from_parser(
        &mut self,
        window_id: WindowId,
        pane_id: u64,
        viewport_row: u16,
        col: u16,
        parser: &sonicterm_vt::vt::Parser,
        viewport_top_abs: Option<u64>,
    ) -> Option<CellTargetSnapshot> {
        let grid = parser.grid();
        let view_top = GpuRenderer::resolved_view_top_abs_legacy(grid, viewport_top_abs);
        let frame = FrameRows::new(grid, view_top);
        let terminal = &self.config.terminal;
        let context = DetectionContext {
            pane_id,
            pointed: (viewport_row, col),
            view_top,
            rows_below: frame.rows_below(),
            scrollback_evicted: grid.scrollback_evicted(),
            screen_epoch: grid.screen_epoch(),
            size_generation: grid.size_generation(),
            alt_screen: grid.is_alt(),
            cwd_revision: parser.cwd_revision(),
            clickable_local_targets: terminal.clickable_local_targets,
            clickable_bare_names: terminal.clickable_bare_names,
            clickable_path_max_chars: terminal.effective_clickable_path_max_chars(),
        };
        let cached = self
            .windows
            .get(&window_id)
            .and_then(|window| window.hover_detection.lookup(&context, grid));
        if let Some(target) = cached {
            // When: the key and every read row's stamp match, the cached result stands; apply still runs.
            self.note_hover(|hover| {
                hover.lookups += 1;
                hover.cache_hits += 1;
            });
            return target;
        }
        let target = self.detect_cell_target(window_id, pane_id, viewport_row, col, parser, &frame);
        let rows_read = frame.rows_read();
        if let Some(window) = self.windows.get_mut(&window_id) {
            window.hover_detection.store(context, grid, frame.read_span(), target.clone());
        }
        self.note_hover(|hover| {
            hover.lookups += 1;
            hover.detections += 1;
            hover.rows_read += rows_read;
        });
        target
    }

    pub(super) fn apply_target_lookup(
        &mut self,
        window_id: WindowId,
        lookup: Result<Option<CellTargetSnapshot>, ()>,
    ) {
        let Ok(target) = lookup else {
            // When: parser contention prevents a fresh lookup, preserve the hint without extending modifier-only feedback.
            if !self.open_modifier_held(window_id) {
                if let Some(window) = self.windows.get_mut(&window_id) {
                    let was_active =
                        window.hovered_url.as_ref().is_some_and(|hover| hover.active());
                    let had_preview = window.link_preview.take().is_some();
                    if let Some(hover) = window.hovered_url.as_mut() {
                        hover.cells.active = false;
                    }
                    if was_active || had_preview {
                        window.hover_link = false;
                        if let Some(native) = window.window.as_ref() {
                            native.set_cursor(winit::window::CursorIcon::Default);
                        }
                    }
                }
            }
            // A busy lookup needs a coherent frame so stationary feedback cannot wait for unrelated input.
            if let Some(window) = self.windows.get(&window_id) {
                window.request_window_redraw();
            }
            return;
        };
        self.apply_target_hover(window_id, target);
    }

    pub(super) fn apply_target_hover(
        &mut self,
        window_id: WindowId,
        target: Option<CellTargetSnapshot>,
    ) {
        let modifier_held = self.open_modifier_held(window_id);
        let preview_allowed = self.frontmost_window == Some(window_id)
            && !(self.command_palette.is_open()
                && self.palette_attached_window.or(self.main_window_id) == Some(window_id));
        let mut probe_request = None;
        let mut hovered = None;
        let explicit_hyperlink = target.as_ref().is_some_and(|target| target.explicit_hyperlink);
        let mut visual_changed = false;

        if let Some(window) = self.windows.get_mut(&window_id) {
            window.hover_unresolved = false;
            let previous_hover = window.hovered_url.clone();
            let previous_link = window.hover_link;
            let show_preview = modifier_held
                && preview_allowed
                && !window.hidden
                && !window.mouse_down
                && window.splitter_drag.is_none()
                && window.scrollbar_drag.is_none()
                && window.drag_session.is_none();
            let pointer = (window.cursor_pos.0 as f32, window.cursor_pos.1 as f32);
            let mut preview =
                target.as_ref().and_then(|target| target.preview(show_preview, pointer));
            match target.as_ref() {
                Some(target @ CellTargetSnapshot { target: ResolvedCellTarget::Uri(_), .. }) => {
                    window.path_probe.invalidate();
                    hovered = target.hovered(modifier_held);
                }
                Some(target @ CellTargetSnapshot { target: ResolvedCellTarget::Path(key), .. }) => {
                    // Accept before request can advance the epoch; pending results never authorize clicks without a fresh key.
                    if let Some(result) = window.path_probe.pending_result.take() {
                        window.path_probe.accept(&result, Some(key));
                    }
                    // A current key needs no request, so it is compared rather than deep-cloned.
                    if !window.path_probe.is_current(key) {
                        probe_request = window.path_probe.request(key.clone());
                    }
                    if let Some(selection) =
                        window.path_probe.authorized_selection(key, modifier_held)
                    {
                        if show_preview {
                            let destination =
                                selection.candidate.resolved_path.to_string_lossy().into_owned();
                            preview = Some(sonicterm_render_model::inputs::LinkPreview {
                                uri: destination,
                                pointer,
                                available: true,
                            });
                        }
                        let cells =
                            selection.candidate.visible_cells(target.pane_id, key.view_top, true);
                        hovered = cells.map(|cells| super::super::hovered_url::HoveredUrl {
                            cells,
                            url: selection.candidate.display().to_string(),
                            label: None,
                        });
                    }
                }
                Some(CellTargetSnapshot { target: ResolvedCellTarget::Rejected(_), .. }) => {
                    // Rejected local targets have no resolved destination to preview.
                    window.path_probe.invalidate();
                }
                None => {
                    window.path_probe.invalidate();
                }
            }
            let preview_changed = window.link_preview != preview;
            window.link_preview = preview;
            window.hovered_url = hovered;
            window.hover_link = window.hovered_url.as_ref().is_some_and(|hover| hover.active())
                || explicit_hyperlink;
            visual_changed = previous_hover != window.hovered_url
                || previous_link != window.hover_link
                || preview_changed;
        }

        if let Some(request) = probe_request {
            self.note_hover(|hover| hover.probes_submitted += 1);
            let submitted = self.path_workers.as_ref().is_some_and(|workers| {
                workers
                    .probe(request)
                    .inspect_err(|error| {
                        tracing::warn!(%error, "path openability probe unavailable");
                    })
                    .is_ok()
            });
            if submitted {
                self.note_path_submit();
            }
        }
        if let Some(window) = self.windows.get(&window_id) {
            if visual_changed {
                if let Some(native) = window.window.as_ref() {
                    native.set_cursor(if window.hover_link {
                        winit::window::CursorIcon::Pointer
                    } else {
                        winit::window::CursorIcon::Default
                    });
                }
                window.request_window_redraw();
            }
        }
    }

    pub(in crate::app) fn clear_target_hover(&mut self, window_id: WindowId) {
        let Some(window) = self.windows.get_mut(&window_id) else {
            // When: `window_id` no longer identifies a live window, there is no hover or probe state left to clear.
            return;
        };
        window.hover_detection.clear();
        // The pointer left the targets, so the absence is resolved, not pending a re-detection.
        window.hover_unresolved = false;
        let probe_changed = window.path_probe.invalidate();
        let hover_changed = window.hovered_url.take().is_some();
        let link_changed = window.hover_link;
        let changed =
            probe_changed | hover_changed | link_changed | window.link_preview.take().is_some();
        window.hover_link = false;
        if changed {
            if let Some(native) = window.window.as_ref() {
                native.set_cursor(winit::window::CursorIcon::Default);
            }
            window.request_window_redraw();
        }
    }

    /// Retains a current probe completion for validation against the next fresh hover snapshot.
    pub(in crate::app) fn handle_path_probe_finished(&mut self, result: PathProbeResult) {
        let window_id = result.request.key.window_id;
        let Some(window) = self.windows.get_mut(&window_id) else {
            // When: windows no longer contains window_id, discard the result with its original owner.
            return;
        };
        if result.request.epoch != window.path_probe.epoch
            || window.path_probe.current.as_ref() != Some(&result.request.key)
        {
            // When: result differs from the current epoch or key, it must not replace a newer pending completion.
            return;
        }
        window.path_probe.pending_result = Some(result);
        window.request_window_redraw();
    }

    pub(in crate::app) fn open_modifier_held(&self, window_id: WindowId) -> bool {
        let modifiers =
            self.windows.get(&window_id).map(|window| window.modifiers).unwrap_or_default();
        open_modifier_in(modifiers)
    }
}

#[cfg(test)]
#[path = "path_target_hover_tests.rs"]
mod path_target_hover_tests;
