//! Local-target click authorization, failure reporting and explicit copy.

use super::*;

impl CellTargetSnapshot {
    pub(super) fn explicit_path_text(&self) -> Option<String> {
        match &self.target {
            ResolvedCellTarget::Rejected(_) => Some(self.display.clone()),
            ResolvedCellTarget::Path(key) => key.link_destination.clone().or_else(|| {
                sonicterm_cfg::url_scan::explicit_path_feedback(
                    key.candidates.iter().map(|candidate| &candidate.target),
                    PathStyle::native(),
                )
                .map(str::to_owned)
            }),
            ResolvedCellTarget::Uri(_) => None,
        }
    }

    pub(super) fn preview(
        &self,
        modifier_held: bool,
        pointer: (f32, f32),
    ) -> Option<sonicterm_render_model::inputs::LinkPreview> {
        let ResolvedCellTarget::Uri(uri) = &self.target else {
            // When: target is a path probe rather than Uri, keep filesystem authorization separate from URI previews.
            return None;
        };
        if !modifier_held {
            // When: modifier_held is false, leave ordinary terminal hover free of destination overlays.
            return None;
        }
        Some(sonicterm_render_model::inputs::LinkPreview {
            uri: uri.clone(),
            pointer,
            available: sonicterm_cfg::url_open::validate(uri).is_ok(),
        })
    }

    pub(super) fn hovered(&self, active: bool) -> Option<super::super::hovered_url::HoveredUrl> {
        let mut cells = self.hover_cells?;
        cells.active = active;
        Some(super::super::hovered_url::HoveredUrl {
            cells,
            url: self.display.clone(),
            label: self.label.clone(),
        })
    }
}

impl App {
    pub(super) fn report_or_copy_path_failure(
        &mut self,
        window_id: WindowId,
        pane_id: u64,
        reason: &str,
        path: &str,
    ) {
        if self.copy_confirmed_failure(window_id, pane_id, path) {
            // When: copy_confirmed_failure consumes the confirmation, retain its clipboard result notification.
            return;
        }
        let reason = if reason.starts_with("path-error-") {
            self.i18n.translate(reason)
        } else {
            // When: reason has no path-error key prefix, retain the native diagnostic text.
            reason.to_owned()
        };
        self.report_failed_target(window_id, pane_id, &reason, path);
    }

    pub(super) fn copy_confirmed_failure(
        &mut self,
        window_id: WindowId,
        pane_id: u64,
        target: &str,
    ) -> bool {
        let Some(window) = self.windows.get_mut(&window_id) else {
            // When: window_id has closed, no confirmation remains actionable.
            return false;
        };
        let Some(failed) = window.path_probe.failed_click.take() else {
            // When: no failed click is armed, this click must attempt the normal action.
            return false;
        };
        if failed.pane_id != pane_id
            || failed.target != target
            || !window.notification.as_ref().is_some_and(|bubble| {
                bubble.message == failed.message
                    && bubble.expires_at.is_some_and(|expiry| expiry > std::time::Instant::now())
            })
        {
            // When: target identity or the visible error differs, do not reuse a previous copy confirmation.
            return false;
        }
        let copied = self.set_clipboard_text(target.to_owned());
        let status = self.i18n.translate(if copied {
            "path-error-copied"
        } else {
            "path-error-copy-failed"
        });
        let prefix = self.i18n.translate("path-error-title");
        let reason = sonicterm_ui::overlays::link_preview_text(&failed.reason);
        let path = sonicterm_ui::overlays::link_preview_text(target);
        self.show_notification_for_kind(
            super::super::FrontmostKind::Child(window_id),
            sonicterm_ui::overlays::NotificationLevel::Error,
            format!("{status}\n{path}\n{prefix}: {reason}"),
        );
        true
    }

    pub(in crate::app) fn report_failed_target(
        &mut self,
        window_id: WindowId,
        pane_id: u64,
        reason: &str,
        target: &str,
    ) {
        if !self.windows.get(&window_id).is_some_and(|window| window.panes.contains_key(&pane_id)) {
            // When: window_id no longer owns pane_id, a late failure must not overwrite the clipboard.
            return;
        }
        let prefix = self.i18n.translate("path-error-title");
        let escaped_reason = sonicterm_ui::overlays::link_preview_text(reason);
        let escaped_target = sonicterm_ui::overlays::link_preview_text(target);
        let instruction = self.i18n.translate("path-error-copy-again");
        let message = format!("{instruction}\n{escaped_target}\n{prefix}: {escaped_reason}");
        self.show_notification_for_kind(
            super::super::FrontmostKind::Child(window_id),
            sonicterm_ui::overlays::NotificationLevel::Error,
            message.clone(),
        );
        self.windows.get_mut(&window_id).unwrap().path_probe.failed_click =
            Some(FailedTargetClick {
                pane_id,
                target: target.to_owned(),
                message,
                reason: reason.to_owned(),
            });
    }

    pub(in crate::app) fn activate_target_at(
        &mut self,
        window_id: WindowId,
        pane_id: u64,
        viewport_row: u16,
        col: u16,
    ) -> bool {
        let modifier_held = self.open_modifier_held(window_id);
        if !modifier_held {
            // When: `modifier_held` is false at click time, never activate a target authorized by earlier hover state.
            return false;
        }
        let Some(target) = self.cell_target_at(window_id, pane_id, viewport_row, col) else {
            // When: the clicked pane cell has no current `target`, leave the click available to ordinary selection handling.
            return false;
        };
        let explicit_path = target.explicit_path_text();
        match target.target {
            ResolvedCellTarget::Rejected(reason) => {
                // Rejected targets retain explicit filepath provenance but never dispatch a native action.
                self.report_or_copy_path_failure(window_id, pane_id, reason, &target.display);
                true
            }
            ResolvedCellTarget::Uri(uri) => {
                // When: target is Uri, retain URI navigation independently of filesystem authorization.
                if self.copy_confirmed_failure(window_id, pane_id, &uri) {
                    // When: copy_confirmed_failure consumes the second click, skip another URI navigation attempt.
                    return true;
                }
                if let Err(error) = sonicterm_cfg::url_open::open(&uri) {
                    tracing::warn!(%error, "URL open failed");
                    self.report_failed_target(window_id, pane_id, &error.to_string(), &uri);
                }
                true
            }
            ResolvedCellTarget::Path(key) => {
                // When: `target.target` is `Path`, activation requires the current typed probe result and bounded opener.
                if let Some(window) = self.windows.get_mut(&window_id) {
                    // A finished check can wait for a hover refresh; the click's own key validates it.
                    window.path_probe.accept_waiting_for(&key);
                }
                let Some(selection) = self.windows.get(&window_id).and_then(|window| {
                    window.path_probe.authorized_selection(&key, modifier_held).cloned()
                }) else {
                    // When: authorized_selection is absent, only explicit path syntax permits failure feedback.
                    let Some(path) = explicit_path else {
                        // When: explicit_path is absent, guessed bare names remain ordinary terminal text.
                        return false;
                    };
                    let reason = self
                        .windows
                        .get(&window_id)
                        .and_then(|window| window.path_probe.failure_for(&key))
                        .unwrap_or("path-error-pending");
                    tracing::debug!(
                        window_id = ?key.window_id,
                        pane_id = key.pane_id,
                        pointed = ?key.pointed,
                        view_top = key.view_top,
                        screen_epoch = key.screen_epoch,
                        scrollback_evicted = key.scrollback_evicted,
                        cwd_revision = key.cwd_revision,
                        cwd = ?key.cwd,
                        clicked_path = ?path,
                        candidates = ?key.candidates,
                        reason,
                        "local path activation unverified"
                    );
                    if reason == "path-error-missing" && key.link_destination.is_none() {
                        // When: `reason` is path-error-missing and `link_destination` is none, the click stays ordinary.
                        return false;
                    }
                    self.report_or_copy_path_failure(window_id, pane_id, reason, &path);
                    return true;
                };
                if selection.candidate.visible_cells(pane_id, key.view_top, true).is_none() {
                    // When: visible_cells returns None, do not activate a path without a drawable underline.
                    return false;
                }
                let copy = selection.candidate.resolved_path.to_string_lossy().into_owned();
                if self.copy_confirmed_failure(window_id, pane_id, &copy) {
                    // When: copy_confirmed_failure consumes this path's second click, do not enqueue another open.
                    return true;
                }
                let Some(workers) = &self.path_workers else {
                    // When: path_workers is absent, report that the requested operation cannot be queued.
                    self.report_failed_target(
                        window_id,
                        pane_id,
                        &self.i18n.translate("path-error-worker"),
                        &copy,
                    );
                    return true;
                };
                // Re-probe every contender at or before the selected tier so a file that appeared since hover wins.
                let contenders = key
                    .candidates
                    .iter()
                    .filter(|candidate| contends_with(candidate, &selection.candidate))
                    .cloned()
                    .collect::<Vec<_>>();
                match workers.open(
                    window_id,
                    pane_id,
                    selection.candidate.resolved_path,
                    selection.decision,
                    contenders,
                ) {
                    Ok(true) => {
                        self.note_path_submit();
                        true
                    }
                    Ok(false) => {
                        tracing::warn!("path open queue full; request dropped");
                        self.report_failed_target(
                            window_id,
                            pane_id,
                            &self.i18n.translate("path-error-busy"),
                            &copy,
                        );
                        true
                    }
                    Err(error) => {
                        tracing::warn!(%error, "path open unavailable");
                        self.report_failed_target(window_id, pane_id, &error.to_string(), &copy);
                        true
                    }
                }
            }
        }
    }
}

/// Whether `modifiers` holds the platform's target open modifier: Cmd on macOS, Ctrl elsewhere.
/// The underline preview, the click and the status line's link segment all use this one rule.
pub(in crate::app) fn open_modifier_in(modifiers: winit::keyboard::ModifiersState) -> bool {
    if cfg!(target_os = "macos") {
        // When: `target_os` is macOS, Cmd is the platform-native target activation modifier.
        modifiers.super_key()
    } else {
        // When: `target_os` is not macOS, Ctrl is the platform-native target activation modifier.
        modifiers.control_key()
    }
}

#[cfg(test)]
#[path = "path_target_activate_tests.rs"]
mod path_target_activate_tests;
