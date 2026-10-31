//! The native title of a terminal window: `#N Name` once the user names it, otherwise
//! `#N <active tab title>`, falling back to `#N SonicTerm` when the window has no titled tab.
//!
//! One writer, [`App::sync_native_titles`], runs once per event-loop pass for every terminal
//! window, independent of rendering, so a tab switch, close, transfer or tear-out reaches the
//! title even when no frame follows. Unchanged inputs are compared borrowed, with no allocation
//! and no native call; a changed title is written at most once per [`NATIVE_TITLE_MIN_INTERVAL`],
//! and a later change inside that interval is delivered on its trailing edge.

use std::time::{Duration, Instant};

use sonicterm_ui::tabs::Tab;
use winit::window::WindowId;

use super::{App, NATIVE_WINDOW_TITLE};

/// Longest name or tab body shown in a native title, in Unicode scalar values; the same limit a
/// window name has, so a program's OSC title cannot make a title longer than a user could.
pub(super) const MAX_NATIVE_TITLE_SCALARS: usize = 128;

/// Shortest time between two native title writes for one window. A program that rewrites its
/// OSC title on every update (a progress bar, a prompt clock) would otherwise make a native call
/// and, on Windows, a screen-reader name change per frame.
pub(super) const NATIVE_TITLE_MIN_INTERVAL: Duration = Duration::from_millis(250);

/// What a window's native title was last composed from and set to.
#[derive(Debug, Default)]
pub(crate) struct NativeTitleState {
    /// The window name the applied title was composed from.
    name: String,
    /// The active tab's displayed title the applied title was composed from; `None` without a tab.
    tab_title: Option<String>,
    /// The active tab's custom title the applied title was composed from.
    custom_title: Option<String>,
    /// The title last handed to the native window; `None` before the first write.
    applied: Option<String>,
    /// When `applied` was written.
    written_at: Option<Instant>,
    /// A composed title waiting for the throttle interval to pass.
    pending: Option<String>,
    /// Native writes made, for tests that pin the write count.
    #[cfg(test)]
    pub(crate) writes: u32,
}

impl NativeTitleState {
    /// Whether the title was composed from exactly these inputs, compared without allocating.
    fn composed_from(&self, name: &str, tab: Option<&Tab>) -> bool {
        self.name == name
            && self.tab_title.as_deref() == tab.map(|tab| tab.title.as_str())
            && self.custom_title.as_deref() == tab.and_then(|tab| tab.custom_title.as_deref())
    }

    /// Record the inputs a title is now composed from.
    fn remember_inputs(&mut self, name: &str, tab: Option<&Tab>) {
        name.clone_into(&mut self.name);
        self.tab_title = tab.map(|tab| tab.title.clone());
        self.custom_title = tab.and_then(|tab| tab.custom_title.clone());
    }

    /// When a throttled title is due, if one waits.
    fn pending_deadline(&self) -> Option<Instant> {
        self.pending.as_ref()?;
        Some(self.written_at? + NATIVE_TITLE_MIN_INTERVAL)
    }
}

/// The part of a tab's title a window title shows, borrowed: the custom title verbatim when the
/// user set one, otherwise the body after the formatter's `#<digits> <icon> ` prefix, or the
/// whole title when it does not have exactly that shape (a `shell 1` placeholder, a `-T` title).
pub(super) fn tab_display_body(tab: &Tab) -> &str {
    if let Some(custom) = tab.custom_title.as_deref() {
        // When: the user named the tab, that name is shown as typed, never re-parsed.
        return custom.trim();
    }
    let title = tab.title.trim();
    formatted_body(title).unwrap_or(title)
}

/// The body of a title the tab formatter produced, `#<digits> <icon> <body>`, where the icon is
/// one the formatter can emit; `None` for any other shape.
fn formatted_body(title: &str) -> Option<&str> {
    let rest = title.strip_prefix('#')?;
    let digits = rest.find(|character: char| !character.is_ascii_digit())?;
    if digits == 0 {
        // When: digits is zero, no digit follows `#` and the title only looks like a numbered one.
        return None;
    }
    let mut after_index = rest[digits..].strip_prefix(' ')?.chars();
    let icon = after_index.next()?;
    if !sonicterm_ui::tab_title::PROGRAM_ICONS.contains(&icon) {
        // When: icon is not a formatter glyph, the text after `#N` is a user's own words.
        return None;
    }
    let body = after_index.as_str().strip_prefix(' ')?.trim();
    (!body.is_empty()).then_some(body)
}

/// `text` safe and short enough for a native title: each control character replaced by U+FFFD
/// (a NUL would panic X11) and at most [`MAX_NATIVE_TITLE_SCALARS`] scalars.
fn title_part(text: &str) -> String {
    let capped: String = text.chars().take(MAX_NATIVE_TITLE_SCALARS).collect();
    sonicterm_types::replace_control_characters(&capped)
}

/// The native title of terminal window number `number`: its `name` when set, otherwise its
/// active tab's `body`, otherwise the application name.
pub(super) fn window_title_text(number: u64, name: &str, body: Option<&str>) -> String {
    let shown = [Some(name), body]
        .into_iter()
        .flatten()
        .map(str::trim)
        .find(|part| !part.is_empty())
        .map_or_else(|| NATIVE_WINDOW_TITLE.to_owned(), title_part);
    format!("#{number} {shown}")
}

impl App {
    /// The title terminal window `id` would show now; `None` for an unnumbered or unknown window.
    #[cfg(test)]
    pub(super) fn native_window_title(&self, id: WindowId) -> Option<String> {
        let key = self.window_keys.get(id)?;
        let window = self.windows.get(&id)?;
        let body = window.tabs.active().map(tab_display_body);
        Some(window_title_text(key.raw(), &window.custom_window_name, body))
    }

    /// Bring every terminal window's native title up to date; returns when a throttled title is
    /// next due, so the event loop wakes to deliver it.
    pub(super) fn sync_native_titles(&mut self, now: Instant) -> Option<Instant> {
        let ids: Vec<WindowId> = self.windows.keys().copied().collect();
        ids.into_iter().filter_map(|id| self.sync_native_title(id, now, false)).min()
    }

    /// Bring window `id`'s native title up to date. `immediate` writes a changed title now, for
    /// a user's own rename and before a smoke readback; otherwise writes are throttled. Returns
    /// the deadline of a title left pending.
    pub(super) fn sync_native_title(
        &mut self,
        id: WindowId,
        now: Instant,
        immediate: bool,
    ) -> Option<Instant> {
        let key = self.window_keys.get(id)?;
        let window = self.windows.get_mut(&id)?;
        if window.role.is_report() {
            // When: a report window shows its fixed name, which its creation already set.
            return None;
        }
        let tab = window.tabs.active();
        let state = &mut window.native_title;
        if state.applied.is_some()
            && state.pending.is_none()
            && state.composed_from(&window.custom_window_name, tab)
        {
            // When: nothing the title reads changed since it was applied, there is nothing to do.
            return None;
        }
        state.remember_inputs(&window.custom_window_name, tab);
        let title =
            window_title_text(key.raw(), &window.custom_window_name, tab.map(tab_display_body));
        if state.applied.as_deref() == Some(title.as_str()) {
            // When: different inputs compose the same title, so it stays and nothing waits.
            state.pending = None;
            return None;
        }
        let throttled = state.written_at.is_some_and(|at| now < at + NATIVE_TITLE_MIN_INTERVAL);
        if throttled && !immediate {
            // When: throttled and not immediate, the last write was inside the interval and this title waits.
            state.pending = Some(title);
            return state.pending_deadline();
        }
        if let Some(native) = window.window.as_ref() {
            native.set_title(&title);
        }
        state.applied = Some(title);
        state.written_at = Some(now);
        state.pending = None;
        #[cfg(test)]
        {
            state.writes += 1;
        }
        None
    }
}

#[cfg(test)]
#[path = "native_title_tests.rs"]
mod native_title_tests;
