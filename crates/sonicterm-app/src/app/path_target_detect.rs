//! Visible-frame cell target detection and source-owned pointer lookup.

use super::*;

impl App {
    pub(in crate::app) fn cell_target_at(
        &self,
        window_id: WindowId,
        pane_id: u64,
        viewport_row: u16,
        col: u16,
    ) -> Option<CellTargetSnapshot> {
        self.cell_target_lookup(window_id, pane_id, viewport_row, col).ok().flatten()
    }

    pub(super) fn cell_target_lookup(
        &self,
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
        let parser = pane.parser.try_lock().ok_or(())?;
        Ok(self.cell_target_from_parser(
            window_id,
            pane_id,
            viewport_row,
            col,
            &parser,
            pane.resolved_viewport(parser.grid()),
        ))
    }

    pub(super) fn cell_target_from_parser(
        &self,
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
        self.detect_cell_target(window_id, pane_id, viewport_row, col, parser, &frame)
    }

    /// Detect the target at displayed cell (`viewport_row`, `col`), reading rows only through `frame`.
    pub(super) fn detect_cell_target(
        &self,
        window_id: WindowId,
        pane_id: u64,
        viewport_row: u16,
        col: u16,
        parser: &sonicterm_vt::vt::Parser,
        frame: &FrameRows<'_>,
    ) -> Option<CellTargetSnapshot> {
        let grid = frame.grid();
        let view_top = frame.view_top();
        if viewport_row >= grid.rows {
            // When: `viewport_row` is past the frame's rows, the pointer is on no displayed cell.
            return None;
        }
        let absolute_row = view_top.checked_add(u64::from(viewport_row))?;
        let row = frame.row(absolute_row)?;
        let cell = row.iter().nth(usize::from(col))?;
        let rejected = |destination: &str, reason| {
            Some(CellTargetSnapshot {
                label: None,
                pane_id,
                hover_cells: None,
                display: destination.to_owned(),
                explicit_hyperlink: cell.hyperlink().is_some(),
                target: ResolvedCellTarget::Rejected(reason),
            })
        };
        let local_snapshot =
            |destination: &str,
             local: DetectedTarget,
             explicit_hyperlink: bool,
             hover_cells: Option<sonicterm_render_model::inputs::HoveredUrlCells>| {
                if !self.config.terminal.clickable_local_targets {
                    // When: clickable_local_targets is false, an OSC 8 wrapper cannot bypass local-target policy.
                    return rejected(destination, "path-error-disabled");
                }
                let cwd = relative_text_cwd(parser);
                let resolved_path = resolve_detected_path(
                    &local,
                    PathStyle::native(),
                    cwd.as_ref(),
                    self.home_dir.as_deref(),
                    &self.local_hostname,
                )?;
                // Highlight capacity cannot revoke an independently stored OSC 8 destination.
                let probe_cells = hover_cells.or_else(|| {
                    sonicterm_render_model::inputs::HoveredUrlCells::single(
                        pane_id,
                        viewport_row,
                        col,
                        col.checked_add(1)?,
                        false,
                    )
                })?;
                let spans = probe_cells
                    .spans()
                    .iter()
                    .map(|span| AbsoluteCellSpan {
                        row: view_top + u64::from(span.row),
                        start_col: span.start_col,
                        end_col: span.end_col,
                    })
                    .collect::<SmallVec<[AbsoluteCellSpan; 2]>>();
                let rows = spans
                    .iter()
                    .map(|span| {
                        Some(PathRowIdentity {
                            row: span.row,
                            fingerprint: row_fingerprint(frame.row(span.row)?),
                        })
                    })
                    .collect::<Option<SmallVec<[PathRowIdentity; 2]>>>()?;
                let key = PathProbeKey {
                    window_id,
                    pane_id,
                    pointed: AbsoluteCell { row: absolute_row, col },
                    view_top,
                    candidates: vec![PathProbeCandidate {
                        spans,
                        target: local,
                        resolved_path,
                        joined: false,
                    }],
                    rows,
                    cwd,
                    cwd_revision: parser.cwd_revision(),
                    link_destination: Some(destination.to_owned()),
                    scrollback_evicted: grid.scrollback_evicted(),
                    screen_epoch: grid.screen_epoch(),
                    alt_screen: grid.is_alt(),
                };
                Some(CellTargetSnapshot {
                    label: None,
                    pane_id,
                    hover_cells,
                    display: destination.to_owned(),
                    explicit_hyperlink,
                    target: ResolvedCellTarget::Path(key),
                })
            };
        if let Some(hyperlink_id) = cell.hyperlink() {
            // When: `cell` carries `hyperlink_id`, preserve its OSC 8 URI provenance instead of scanning the displayed text as a path.
            let uri = parser.hyperlinks().lookup(hyperlink_id)?.uri.to_string();
            let local = match sonicterm_cfg::url_scan::local_link_target(&uri, PathStyle::native())
            {
                Ok(local) => local,
                Err(reason) => {
                    // When: local_link_target returns Err, preserve the rejection instead of falling back to URI dispatch.
                    return rejected(&uri, reason);
                }
            };
            if let Some(local) = local {
                // When: local_link_target yields a local destination, require the bounded filesystem probe before activation.
                return local_snapshot(
                    &uri,
                    local,
                    true,
                    hyperlink_hover_cells(frame, pane_id, viewport_row, col, hyperlink_id),
                );
            }
            let hover_cells =
                hyperlink_hover_cells(frame, pane_id, viewport_row, col, hyperlink_id);
            return Some(CellTargetSnapshot {
                pane_id,
                hover_cells,
                display: uri.clone(),
                explicit_hyperlink: true,
                target: ResolvedCellTarget::Uri(uri),
                label: hover_cells.as_ref().map(|cells| hyperlink_label(frame, cells)),
            });
        }

        let style = PathStyle::native();
        let clickable_local_targets = self.config.terminal.clickable_local_targets;
        let clickable_bare_names = self.config.terminal.clickable_bare_names;
        let pointed = AbsoluteCell { row: absolute_row, col };
        let logical = match hardwrap_uri_in(frame, pointed) {
            HardwrapUri::Complete(candidate) => {
                LogicalPathScan { candidates: vec![candidate], rows: SmallVec::new() }
            }
            HardwrapUri::Incomplete => {
                // When: hardwrap_uri_at_cell is Incomplete, never fall back to its valid-looking prefix.
                return None;
            }
            HardwrapUri::NotApplicable => {
                // When: `hardwrap_uri_in` is NotApplicable, the bare program-broken rule decides next.
                match bare_hardwrap_uri_in(frame, pointed) {
                    HardwrapUri::Complete(candidate) => {
                        LogicalPathScan { candidates: vec![candidate], rows: SmallVec::new() }
                    }
                    HardwrapUri::Incomplete => {
                        // When: `bare_hardwrap_uri_in` is Incomplete, never fall back to its prefix.
                        return None;
                    }
                    HardwrapUri::NotApplicable => {
                        // When: `bare_hardwrap_uri_in` is NotApplicable, the path rule decides next.
                        match bare_hardwrap_path_in(frame, pointed, style) {
                            HardwrapPathScan::Incomplete => {
                                // When: `bare_hardwrap_path_in` is Incomplete, never fall back to its prefix.
                                return None;
                            }
                            HardwrapPathScan::Joined(joined) => merge_hardwrap_fragments(
                                joined,
                                logical_path_scan_in(frame, pointed, style, clickable_bare_names),
                                pointed,
                            ),
                            HardwrapPathScan::NotApplicable => {
                                logical_path_scan_in(frame, pointed, style, clickable_bare_names)?
                            }
                        }
                    }
                }
            }
        };
        if let Some(LogicalTargetCandidate { target: DetectedTarget::Uri(uri), spans, .. }) =
            logical.candidates.first()
        {
            // When: logical URI provenance wins, preserve its complete destination and every visible fragment.
            let spans = spans
                .iter()
                .map(|span| {
                    Some(sonicterm_render_model::inputs::HoveredUrlSpan {
                        row: u16::try_from(span.row.checked_sub(view_top)?).ok()?,
                        start_col: span.start_col,
                        end_col: span.end_col,
                    })
                })
                .collect::<Option<Vec<_>>>()?;
            let hover_cells =
                sonicterm_render_model::inputs::HoveredUrlCells::new(pane_id, spans, false)?;
            let local = match sonicterm_cfg::url_scan::local_link_target(uri, style) {
                Ok(local) => local,
                Err(reason) => {
                    // When: local_link_target rejects the complete URI, no fragment may fall back to navigation.
                    return rejected(uri, reason);
                }
            };
            if let Some(local) = local {
                // When: local is Some, keep full-span file URI activation behind filesystem authorization.
                return local_snapshot(uri, local, false, Some(hover_cells));
            }
            return Some(CellTargetSnapshot {
                label: None,
                pane_id,
                hover_cells: Some(hover_cells),
                display: uri.clone(),
                explicit_hyperlink: false,
                target: ResolvedCellTarget::Uri(uri.clone()),
            });
        }
        if !clickable_local_targets {
            // When: `clickable_local_targets` is false, leave non-URI text inert before resolving any candidate set.
            return None;
        }

        let cwd = relative_text_cwd(parser);
        let max_chars = self.config.terminal.effective_clickable_path_max_chars();
        let mut candidates = logical
            .candidates
            .into_iter()
            .filter(|candidate| {
                detected_target_enabled(
                    &candidate.target,
                    clickable_local_targets,
                    clickable_bare_names,
                ) && detected_target_chars(&candidate.target) <= max_chars
            })
            .filter_map(|candidate| {
                let resolved_path = resolve_detected_path(
                    &candidate.target,
                    style,
                    cwd.as_ref(),
                    self.home_dir.as_deref(),
                    &self.local_hostname,
                )?;
                Some(PathProbeCandidate {
                    spans: candidate.spans,
                    target: candidate.target,
                    resolved_path,
                    joined: candidate.joined,
                })
            })
            .collect::<Vec<_>>();
        // Joined candidates come first, so the hover shows the joined path.
        candidates.sort_by(|left, right| {
            right
                .joined
                .cmp(&left.joined)
                .then_with(|| left.probe_order().cmp(&right.probe_order()))
        });
        candidates.dedup();
        let display = candidates.first()?.display().to_string();
        let key = PathProbeKey {
            window_id,
            pane_id,
            pointed,
            view_top,
            candidates,
            rows: logical.rows,
            cwd,
            cwd_revision: parser.cwd_revision(),
            link_destination: None,
            scrollback_evicted: grid.scrollback_evicted(),
            screen_epoch: grid.screen_epoch(),
            alt_screen: grid.is_alt(),
        };
        Some(CellTargetSnapshot {
            label: None,
            pane_id,
            hover_cells: None,
            display,
            explicit_hyperlink: false,
            target: ResolvedCellTarget::Path(key),
        })
    }

    pub(super) fn pointer_target_cell(&self, window_id: WindowId) -> Option<(u64, u16, u16)> {
        let window = self.windows.get(&window_id)?;
        let (cursor_x_px, cursor_y_px) = (window.cursor_pos.0 as f32, window.cursor_pos.1 as f32);
        let (rendered_pane, row, col) = match window.renderer.as_ref() {
            Some(renderer) => renderer.pixel_to_pane_cell(cursor_x_px, cursor_y_px)?,
            None => Self::headless_pointer_cell(window, cursor_x_px, cursor_y_px)?,
        };
        let pane_id = if rendered_pane == 0 {
            // When: `rendered_pane` is zero, use `main_window_id` geometry for the main `window_id` and child geometry otherwise.
            if Some(window_id) == self.main_window_id {
                self.pane_at_cursor(cursor_x_px, cursor_y_px)?
            } else {
                super::super::pane_id_at_point(
                    &Self::compute_pane_rects_for(window),
                    cursor_x_px,
                    cursor_y_px,
                )?
            }
        } else {
            // When: `rendered_pane` is nonzero, retain the renderer-owned pane identity paired with `row` and `col`.
            rendered_pane
        };
        Some((pane_id, row, col))
    }

    /// The pane cell under a point in a window with no renderer: a headless test with a pane
    /// viewport resolves it on that viewport's uniform grid; otherwise there is none.
    #[cfg(test)]
    pub(super) fn headless_pointer_cell(
        window: &super::super::WindowState,
        cursor_x_px: f32,
        cursor_y_px: f32,
    ) -> Option<(u64, u16, u16)> {
        // A window without `test_pane_viewport` has no uniform grid to resolve the point on.
        let (_, cell_width_px, cell_height_px) = window.test_pane_viewport?;
        super::super::window_event::headless_pane_cell(
            &Self::compute_pane_rects_for(window),
            (cell_width_px, cell_height_px),
            cursor_x_px,
            cursor_y_px,
        )
    }

    /// The pane cell under a point in a window with no renderer: outside tests there is none.
    #[cfg(not(test))]
    pub(super) fn headless_pointer_cell(
        _window: &super::super::WindowState,
        _cursor_x_px: f32,
        _cursor_y_px: f32,
    ) -> Option<(u64, u16, u16)> {
        None
    }
}

#[cfg(test)]
#[path = "path_target_detect_tests.rs"]
mod path_target_detect_tests;
