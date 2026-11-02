//! The native title of a terminal window: `#N Name` once the user names it, otherwise
//! `#N <active tab title>`, falling back to `#N SonicTerm` when the window has no titled tab.
//!
//! One writer, [`App::sync_native_titles`], runs once per event-loop pass for every terminal
//! window, independent of rendering, so a tab switch, close, transfer or tear-out reaches the
//! title even when no frame follows. Unchanged inputs are compared borrowed, with no allocation
//! and no native call. A different active tab (a switch, a close, a drag out) or a rename is
//! written at once; a retitle of the same tab is written at most once per
//! [`NATIVE_TITLE_MIN_INTERVAL`], and a later one inside that interval lands on its trailing edge.

use std::time::{Duration, Instant};

use sonicterm_ui::tabs::{Tab, TabId};
use winit::window::WindowId;

use super::native_title_font;
use super::{App, NATIVE_WINDOW_TITLE};

/// Longest name or tab body shown in a native title, in Unicode scalar values: the window-name
/// limit, so a program's OSC title cannot make a title longer than a user could.
pub(super) const MAX_NATIVE_TITLE_SCALARS: usize =
    sonicterm_ui::command_palette::MAX_WINDOW_NAME_SCALARS;

/// Shortest time between two native title writes for one window. A program that rewrites its
/// OSC title on every update (a progress bar, a prompt clock) would otherwise make a native call
/// and, on Windows, a screen-reader name change per frame.
pub(super) const NATIVE_TITLE_MIN_INTERVAL: Duration = Duration::from_millis(250);

/// What a window's native title was last composed from and set to.
#[derive(Debug, Default)]
pub(crate) struct NativeTitleState {
    /// The window name the applied title was composed from.
    name: String,
    /// The active tab the applied title was composed from; `None` without a tab.
    tab_id: Option<TabId>,
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
            && self.tab_id == tab.map(|tab| tab.id)
            && self.tab_title.as_deref() == tab.map(|tab| tab.title.as_str())
            && self.custom_title.as_deref() == tab.and_then(|tab| tab.custom_title.as_deref())
    }

    /// Record the inputs a title is now composed from.
    fn remember_inputs(&mut self, name: &str, tab: Option<&Tab>) {
        name.clone_into(&mut self.name);
        self.tab_id = tab.map(|tab| tab.id);
        self.tab_title = tab.map(|tab| tab.title.clone());
        self.custom_title = tab.and_then(|tab| tab.custom_title.clone());
    }

    /// When a throttled title is due, if one waits.
    fn pending_deadline(&self) -> Option<Instant> {
        self.pending.as_ref()?;
        Some(self.written_at? + NATIVE_TITLE_MIN_INTERVAL)
    }
}

/// The part of a tab's title a window title shows, borrowed: the custom title as typed when the
/// user set one, otherwise the body after the formatter's `#<digits> <icon> ` prefix, or the whole
/// title when it does not have exactly that shape (a `shell 1` placeholder, a `-T` title). A body
/// that is blank once its controls are replaced falls back to the application name.
pub(super) fn tab_display_body(tab: &Tab) -> &str {
    if let Some(custom) = tab.custom_title.as_deref() {
        // When: the user named the tab, that name is shown as typed, never re-parsed.
        return custom;
    }
    formatted_body(&tab.title).unwrap_or(&tab.title)
}

/// The body of a title the tab formatter produced, `#<digits> <icon> <body>`, where the icon is
/// one the formatter can emit and is followed by a space; `None` for any other shape.
fn formatted_body(title: &str) -> Option<&str> {
    let rest = title.trim_start().strip_prefix('#')?;
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
    // The body is everything after the icon's separator, unsanitized; a blank body falls back
    // to the application name only after its controls are replaced.
    after_index.as_str().strip_prefix(' ')
}

/// `text` safe and short enough for a native title: each control character replaced by U+FFFD
/// (a NUL would panic X11), then every private-use glyph `draws` rejects left out, the spaces it
/// leaves collapsed, the ends trimmed, and at most [`MAX_NATIVE_TITLE_SCALARS`] scalars kept.
/// Replacing first keeps a tab or newline beside an icon visible; capping last keeps a run of
/// dropped icons from using up the cap.
fn title_part(text: &str, draws: impl Fn(char) -> bool) -> String {
    let replaced = sonicterm_types::replace_control_characters(text);
    let mut part = String::with_capacity(replaced.len());
    let mut dropped_since_space = false;
    for character in replaced.chars() {
        if is_private_use(character) && !draws(character) {
            // When: is_private_use(character) and draws(character) is false, the glyph would draw as a box.
            dropped_since_space = true;
            continue;
        }
        if character == ' ' && dropped_since_space && part.ends_with(' ') {
            // When: character is a space, dropped_since_space and part ends with one, that space already separates.
            continue;
        }
        if character == ' ' {
            dropped_since_space = false;
        }
        part.push(character);
    }
    part.trim().chars().take(MAX_NATIVE_TITLE_SCALARS).collect()
}

/// Whether `character` is in a Unicode private-use area, where Nerd Font icons live.
pub(super) fn is_private_use(character: char) -> bool {
    matches!(character, '\u{E000}'..='\u{F8FF}' | '\u{F0000}'..='\u{FFFFD}' | '\u{100000}'..='\u{10FFFD}')
}

/// The native title of terminal window number `number`: its `name` when set, otherwise its
/// active tab's `body`, otherwise the application name. Icons the platform's title font cannot
/// draw are left out; the tab bar, which draws with SonicTerm's own fonts, keeps them.
pub(super) fn window_title_text(number: u64, name: &str, body: Option<&str>) -> String {
    window_title_text_with(number, name, body, native_title_font::title_font_draws)
}

/// [`window_title_text`] with the title font's coverage given by `draws`.
fn window_title_text_with(
    number: u64,
    name: &str,
    body: Option<&str>,
    draws: impl Fn(char) -> bool + Copy,
) -> String {
    let shown = [Some(name), body]
        .into_iter()
        .flatten()
        .map(|text| title_part(text, draws))
        .find(|part| !part.is_empty())
        .unwrap_or_else(|| NATIVE_WINDOW_TITLE.to_owned());
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
    /// next due, so the event loop wakes to deliver it. Each window keeps its own interval.
    pub(super) fn sync_native_titles(&mut self, now: Instant) -> Option<Instant> {
        let Self { window_keys, windows, .. } = self;
        windows
            .iter_mut()
            .filter_map(|(id, window)| sync_window(window_keys.get(*id)?, window, now, false))
            .min()
    }

    /// The event-loop wake a throttled title needs, after bringing every title up to date.
    pub(super) fn native_title_wake(&mut self, now: Instant) -> Option<super::redraw::DueWork> {
        let deadline = self.sync_native_titles(now)?;
        Some(super::redraw::DueWork {
            owner: None,
            cause: super::redraw::DueCause::NativeTitle,
            deadline,
        })
    }

    /// The title last written to window `id`'s native window, for tests in other modules.
    #[cfg(test)]
    pub(super) fn applied_native_title(&self, id: WindowId) -> Option<&str> {
        self.windows.get(&id)?.native_title.applied.as_deref()
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
        sync_window(key, self.windows.get_mut(&id)?, now, immediate)
    }
}

/// Bring one terminal window's native title up to date; see [`App::sync_native_title`].
fn sync_window(
    key: sonicterm_types::WindowKey,
    window: &mut super::WindowState,
    now: Instant,
    immediate: bool,
) -> Option<Instant> {
    if window.role.is_report() {
        // When: a report window shows its fixed name, which its creation already set.
        return None;
    }
    let tab = window.tabs.active();
    let state = &mut window.native_title;
    // A new active tab or name is the user's own action and is written at once; only a retitle of
    // the same tab, which a program can repeat every frame, waits out the interval.
    let title = if state.applied.is_some() && state.composed_from(&window.custom_window_name, tab) {
        // When: applied and composed_from hold, the inputs are unchanged and nothing is recomposed;
        // only a pending title can be written, and only once its deadline passes.
        let due = state.pending_deadline().filter(|deadline| now < *deadline && !immediate);
        match state.pending.take() {
            Some(pending) if due.is_none() => pending,
            pending => {
                // When: pending is None, or due is Some because it is not yet due, nothing is written.
                state.pending = pending;
                return due;
            }
        }
    } else {
        // When: not applied or not composed_from these inputs, the title is composed again.
        let user_change =
            state.tab_id != tab.map(|tab| tab.id) || state.name != window.custom_window_name;
        state.remember_inputs(&window.custom_window_name, tab);
        let title =
            window_title_text(key.raw(), &window.custom_window_name, tab.map(tab_display_body));
        if state.applied.as_deref() == Some(title.as_str()) {
            // When: different inputs compose the same title, so it stays and nothing waits.
            state.pending = None;
            return None;
        }
        let throttled = state.written_at.is_some_and(|at| now < at + NATIVE_TITLE_MIN_INTERVAL);
        if throttled && !immediate && !user_change {
            // When: throttled, not immediate and no user_change, a program's retitle inside the interval waits.
            state.pending = Some(title);
            return state.pending_deadline();
        }
        title
    };
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

#[cfg(test)]
#[path = "native_title_tests.rs"]
mod native_title_tests;
