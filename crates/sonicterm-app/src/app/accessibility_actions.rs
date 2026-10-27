//! Validated accessibility actions and source-window status focus navigation.

use super::*;

impl App {
    /// The segment `node` names when the target its request resolved is still what the window
    /// exposes: an assistive technology is connected, and the committed layout has the same
    /// segment, pane, action, settings and resize epoch, with a click action for a click.
    pub(super) fn request_current(
        &self,
        window_id: WindowId,
        node: u64,
        target: Option<RequestTarget>,
        click: bool,
    ) -> Option<StatusSegmentId> {
        self.accessibility.get(&window_id).filter(|state| state.active)?;
        let id = segment_for_node(node)?;
        let target = target.filter(|target| target.id == id)?;
        let structure = self.exposed_structure(window_id);
        let exposed = structure.segments.iter().find(|exposed| exposed.id == id)?;
        let current = target_of(&structure, exposed);
        (current == target && (!click || current.action.is_some())).then_some(id)
    }

    /// Health's severity and text and the tab bar's new-mark count of `window_id` now: an
    /// assistive technology's baseline.
    pub(super) fn accessible_baseline(
        &self,
        window_id: WindowId,
    ) -> Option<(Severity, Option<String>, u64)> {
        let inputs = self.accessible_inputs(window_id)?;
        let view = self.accessible_view(&[StatusSegmentId::Health], &inputs);
        let (severity, text) = health_state(&view, &self.i18n);
        Some((severity, text, self.windows.get(&window_id)?.tabs.last_mark().0))
    }

    /// Move strip focus in `window_id` to presented segment `id`. The strip takes the window's keys,
    /// so an active IME composition ends with its preedit; the ring is drawn on the next frame.
    pub(in crate::app) fn set_status_focus(&mut self, window_id: WindowId, id: StatusSegmentId) {
        let Self { windows, accessibility, .. } = self;
        let Some(window) = windows.get_mut(&window_id) else {
            // When: `windows` has no `window_id`, there is no strip to focus.
            return;
        };
        let presented = window
            .status_strip
            .as_ref()
            .is_some_and(|strip| strip.segments.iter().any(|segment| segment.id == id));
        let Some(state) = accessibility.get_mut(&window_id).filter(|_| presented) else {
            // When: the window has no accessibility or `presented` is false, focus cannot move there.
            return;
        };
        if state.focus.is_none() {
            // Taking the keys from the terminal ends a composition in progress.
            window.ime.cancel();
        }
        state.focus = Some(id);
        state.focus_tab = window.tabs.active().map(|tab| tab.id);
        self.apply_focus_ring(window_id);
    }

    /// Draw `window_id`'s ring for its strip focus with the current `strong_focus`, and ask for a
    /// frame through the dispatch record when the ring changed.
    pub(super) fn apply_focus_ring(&mut self, window_id: WindowId) {
        let strong = self.config.accessibility.strong_focus;
        let Self { windows, accessibility, .. } = self;
        let Some(state) = accessibility.get_mut(&window_id) else {
            // When: `accessibility` has no entry, the window draws no ring.
            return;
        };
        let ring = state.focus.map(|id| (id, strong));
        if state.ring == ring {
            // When: `ring` is unchanged, nothing is redrawn.
            return;
        }
        state.ring = ring;
        let Some(window) = windows.get_mut(&window_id) else {
            // When: `windows` has no `window_id`, there is no frame to draw it in.
            return;
        };
        if let Some(renderer) = window.renderer.as_mut() {
            renderer.set_status_line_focus(ring);
        }
        window.mark_redraw(RedrawCause::StatusLine);
        self.request_redraw_once(window_id);
    }

    /// Redraw every focused segment's ring with the current `strong_focus`, after a reload.
    pub(in crate::app) fn refresh_status_focus_rings(&mut self) {
        let focused: Vec<WindowId> = self
            .accessibility
            .iter()
            .filter(|(_, state)| state.focus.is_some())
            .map(|(window_id, _)| *window_id)
            .collect();
        for window_id in focused {
            self.apply_focus_ring(window_id);
        }
    }

    /// End strip focus in `window_id`, so keys go to the terminal again.
    pub(in crate::app) fn end_status_focus(&mut self, window_id: WindowId) {
        let Some(state) = self.accessibility.get_mut(&window_id) else {
            // When: `accessibility` has no entry, the window never had strip focus.
            return;
        };
        if state.focus.take().is_none() {
            // When: `focus` was already `None`, there is nothing to end.
            return;
        }
        state.focus_tab = None;
        self.apply_focus_ring(window_id);
    }

    /// End `window_id`'s strip focus for a pointer press outside the strip, before any other press
    /// handling sees it.
    pub(in crate::app) fn end_status_focus_outside(&mut self, window_id: WindowId) {
        let inside = self.windows.get(&window_id).is_some_and(|window| {
            let pointer_y = window.cursor_pos.1 as f32;
            window.status_strip.as_ref().is_some_and(|strip| strip.contains(pointer_y))
        });
        if !inside {
            self.end_status_focus(window_id);
        }
    }

    /// `focus_status_line`: focus the first presented clickable segment of `window_id`'s strip,
    /// or its first segment when none is clickable. With the strip off or empty it does nothing.
    pub(in crate::app) fn focus_status_line(&mut self, window_id: WindowId) {
        if !self.status_line_shown() {
            // When: `status_line_shown` is false, there is no strip to focus.
            return;
        }
        let Some(window) = self.windows.get(&window_id) else {
            // When: `windows` has no `window_id`, there is no strip to focus.
            return;
        };
        let mut segments: Vec<_> =
            window.status_strip.iter().flat_map(|strip| &strip.segments).collect();
        segments.sort_by(|left, right| left.x.total_cmp(&right.x));
        let target = segments
            .iter()
            .find(|segment| segment.click.is_some())
            .or(segments.first())
            .map(|segment| segment.id);
        if let Some(target) = target {
            self.set_status_focus(window_id, target);
        }
    }

    /// Handle a pressed key while `window_id`'s strip has focus. Returns `true` when the strip
    /// consumed it, so it never reaches the terminal: arrows, Home and End move, Enter and Space
    /// click, Escape and Tab return focus, and any other key returns focus too. A held key's
    /// repeat moves focus for a navigation key and is otherwise consumed with no effect.
    pub(in crate::app) fn status_focus_key(
        &mut self,
        window_id: WindowId,
        key: &Key,
        repeat: bool,
    ) -> bool {
        let Some(window) = self.windows.get(&window_id) else {
            // When: `windows` has no `window_id`, no strip owns the key.
            return false;
        };
        let Some(state) = self.accessibility.get(&window_id) else {
            // When: `accessibility` is `None`, no strip owns the key.
            return false;
        };
        let Some(focus) = state.focus else {
            // When: `focus` is `None`, the strip does not own keyboard focus.
            return false;
        };
        let tab_changed = state.focus_tab != window.tabs.active().map(|tab| tab.id);
        let ordered = self.committed_ids(window_id);
        let Some(position) = ordered.iter().position(|id| *id == focus).filter(|_| !tab_changed)
        else {
            // When: the focused segment is gone or the tab changed, focus ends and the key goes
            // to the terminal as usual.
            self.end_status_focus(window_id);
            return false;
        };
        let navigation = matches!(
            key,
            Key::Named(NamedKey::ArrowLeft | NamedKey::ArrowRight | NamedKey::Home | NamedKey::End)
        );
        if repeat && !navigation {
            // When: `repeat` of a key that is not `navigation`, the repeat is consumed and changes
            // nothing, so a held key never reaches the terminal while the strip has focus.
            return true;
        }
        let last = ordered.len() - 1;
        let move_to = |index: usize| Some(ordered[index]);
        let target = match key {
            Key::Named(NamedKey::ArrowLeft) => move_to(position.saturating_sub(1)),
            Key::Named(NamedKey::ArrowRight) => move_to((position + 1).min(last)),
            Key::Named(NamedKey::Home) => move_to(0),
            Key::Named(NamedKey::End) => move_to(last),
            Key::Named(NamedKey::Enter | NamedKey::Space) => {
                self.click_status_segment(window_id, focus);
                None
            }
            Key::Character(text) if text.as_str() == " " => {
                self.click_status_segment(window_id, focus);
                None
            }
            Key::Named(
                NamedKey::Shift
                | NamedKey::Control
                | NamedKey::Alt
                | NamedKey::Super
                | NamedKey::Meta
                | NamedKey::CapsLock
                | NamedKey::Fn,
            ) => {
                // A modifier alone keeps focus, so a chord can follow.
                None
            }
            _ => {
                // Escape, Tab and every other key return focus without reaching the terminal.
                self.end_status_focus(window_id);
                None
            }
        };
        if let Some(target) = target {
            self.set_status_focus(window_id, target);
        }
        true
    }

    /// Handle an IME event while `window_id`'s strip has focus. Returns `true` when the strip
    /// consumed it: preedit is suppressed, and a commit is discarded and returns focus, so a
    /// commit never reaches the terminal while the strip owns focus.
    pub(in crate::app) fn status_focus_ime(&mut self, window_id: WindowId, ime: &Ime) -> bool {
        let focused = self.accessibility.get(&window_id).is_some_and(|state| state.focus.is_some());
        if !focused {
            // When: `focused` is false, the IME event goes to its usual owner.
            return false;
        }
        match ime {
            Ime::Preedit(..) => true,
            Ime::Commit(_) => {
                self.end_status_focus(window_id);
                true
            }
            Ime::Enabled | Ime::Disabled => false,
        }
    }

    /// Click presented segment `id` of `window_id` through the same dispatcher as a mouse click,
    /// so READONLY and routing apply exactly as they do there. An inert or absent segment does
    /// nothing.
    pub(in crate::app) fn click_status_segment(
        &mut self,
        window_id: WindowId,
        id: StatusSegmentId,
    ) {
        let click: Option<SegmentClick> = self
            .windows
            .get(&window_id)
            .and_then(|window| window.status_strip.as_ref())
            .and_then(|strip| strip.segments.iter().find(|segment| segment.id == id))
            .and_then(|segment| segment.click.clone());
        if let Some(click) = click {
            self.run_status_click(window_id, click);
        }
    }
}

#[cfg(test)]
#[path = "accessibility_actions_tests.rs"]
mod accessibility_actions_tests;
