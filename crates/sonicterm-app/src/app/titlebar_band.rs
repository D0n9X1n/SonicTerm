//! The integrated titlebar band at run time: each window's band metrics, its native title's
//! visibility, the caption buttons' state, and the presses SonicTerm handles in the band (window
//! drag, the titlebar double-click, caption buttons and the edges of a self-framed window).

use std::time::{Duration, Instant};

use sonicterm_ui::window_band::{
    caption_button_at, resize_edge_at, CaptionButton, CaptionState, ResizeEdge,
};
use winit::window::{CursorIcon, ResizeDirection, WindowId};

use sonicterm_cfg::status_line::StatusSegmentId;

use super::redraw::RedrawCause;
use super::window_setup::titlebar;
use super::{App, WindowState};

/// The double-click time and rectangle half-width and half-height (raster px) where the platform
/// gives no click count: the system's own settings on Windows, its defaults elsewhere.
fn double_click_limits() -> (Duration, (f32, f32)) {
    titlebar::system_double_click_limits()
}

/// What a left press in an integrated window's band does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BandPress {
    /// Not in the band, or the window has none: routing continues.
    Outside,
    /// On a caption button, which acts on release over the same button.
    Caption(CaptionButton),
    /// On a window edge of a self-framed window: resize from it.
    Edge(ResizeEdge),
    /// On a segment: the status line's own press routing handles it.
    Segment,
    /// On empty band space: drag the window, or run the double-click action.
    Empty,
}

/// Classify a left press at raster-px `(pointer_x, pointer_y)` in `window`, caption buttons and edges first.
pub(crate) fn classify_band_press(
    window: &WindowState,
    pointer_x: f32,
    pointer_y: f32,
) -> BandPress {
    let Some(renderer) = window.renderer.as_ref() else {
        // When: the window has no renderer, it draws no band.
        return BandPress::Outside;
    };
    let band = renderer.window_band();
    if !band.integrated {
        // When: the titlebar is native, the band is the system's.
        return BandPress::Outside;
    }
    let (width, height) = renderer.surface_size();
    let (width, height) = (width as f32, height as f32);
    let scale = renderer.scale_factor();
    let maximized = window.window.as_ref().is_some_and(|native| native.is_maximized());
    let band_height = renderer.band_height();
    // Buttons first: the Close button's top and right edges overlap the resize zones.
    if let Some(button) = caption_button_at(&band, width, band_height, scale, pointer_x, pointer_y)
    {
        // When: caption_button_at finds a self-drawn button under the pointer, it owns the press.
        return BandPress::Caption(button);
    }
    if let Some(edge) =
        resize_edge_at(&band, (width, height), scale, maximized, pointer_x, pointer_y)
    {
        // When: a self-framed window's edge is under the pointer, it resizes from there.
        return BandPress::Edge(edge);
    }
    if pointer_y < 0.0 || pointer_y >= band_height {
        // When: pointer_y is outside 0..band_height, the press is below the band and the panes'.
        return BandPress::Outside;
    }
    let on_segment = [window.status_strip.as_ref(), window.title_band.as_ref()]
        .into_iter()
        .flatten()
        .any(|hit| {
            hit.segment_at(pointer_x, pointer_y).is_some_and(|segment| segment.click.is_some())
        });
    if on_segment {
        BandPress::Segment
    } else {
        // When: on_segment is false, no clickable segment is under the pointer: titlebar space.
        BandPress::Empty
    }
}

/// The winit resize direction of `edge`.
fn resize_direction(edge: ResizeEdge) -> ResizeDirection {
    match edge {
        ResizeEdge::North => ResizeDirection::North,
        ResizeEdge::South => ResizeDirection::South,
        ResizeEdge::East => ResizeDirection::East,
        ResizeEdge::West => ResizeDirection::West,
        ResizeEdge::NorthEast => ResizeDirection::NorthEast,
        ResizeEdge::NorthWest => ResizeDirection::NorthWest,
        ResizeEdge::SouthEast => ResizeDirection::SouthEast,
        ResizeEdge::SouthWest => ResizeDirection::SouthWest,
    }
}

/// The cursor a resize edge shows.
pub(crate) fn edge_cursor(edge: ResizeEdge) -> CursorIcon {
    match edge {
        ResizeEdge::North | ResizeEdge::South => CursorIcon::NsResize,
        ResizeEdge::East | ResizeEdge::West => CursorIcon::EwResize,
        ResizeEdge::NorthEast | ResizeEdge::SouthWest => CursorIcon::NeswResize,
        ResizeEdge::NorthWest | ResizeEdge::SouthEast => CursorIcon::NwseResize,
    }
}

/// A window's caption state for this pass: an overlay that opened, or a band that moved or
/// resized, cancels the press for good, so its release can no longer act even after the overlay
/// closes; hover clears with it.
pub(crate) fn settle_caption(
    state: CaptionState,
    modal_open: bool,
    band_changed: bool,
) -> CaptionState {
    if modal_open || band_changed {
        // When: modal_open or band_changed, the held button no longer owns a release.
        return CaptionState { pressed: None, hovered: None, ..state };
    }
    state
}

/// The caption button a left release runs: the one pressed, when the release is over that same
/// button and no overlay opened while it was held; `None` cancels the press.
pub(crate) fn release_action(
    pressed: Option<CaptionButton>,
    under: Option<CaptionButton>,
    modal: bool,
) -> Option<CaptionButton> {
    pressed.filter(|button| !modal && Some(*button) == under)
}

/// Whether an empty-band press at `at`, `position`, follows `last` closely enough in time and
/// place to complete a double-click; `click_count` is the platform's own count when it has one.
pub(crate) fn is_double_click(
    click_count: Option<u32>,
    last: Option<(Instant, (f32, f32))>,
    at: Instant,
    position: (f32, f32),
    limits: (Duration, (f32, f32)),
) -> bool {
    if let Some(count) = click_count {
        // When: the platform counted the clicks itself, its count decides.
        return count >= 2;
    }
    let (time, (half_width, half_height)) = limits;
    last.is_some_and(|(then, (last_x, last_y))| {
        at.saturating_duration_since(then) <= time
            && (position.0 - last_x).abs() <= half_width
            && (position.1 - last_y).abs() <= half_height
    })
}

impl App {
    /// The window the open command palette (or checklist, rename editor) belongs to; `None` while
    /// none is open.
    pub(in crate::app) fn palette_window(&self) -> Option<WindowId> {
        self.interaction
            .command_palette
            .is_open()
            .then(|| self.interaction.palette_attached_window.or(self.registry.main_window_id))
            .flatten()
    }

    /// Cancel `window_id`'s held caption button when its own palette is open. Runs before every
    /// window event, so a palette that opens and closes between a press and its release cancels
    /// the press before the release arrives; another window's palette never does.
    pub(in crate::app) fn cancel_caption_under_palette(&mut self, window_id: WindowId) {
        if self.palette_window() != Some(window_id) {
            // When: palette_window is not window_id, no overlay of this window took the pointer.
            return;
        }
        let pressed = self
            .registry
            .windows
            .get(&window_id)
            .is_some_and(|window| window.caption.pressed.is_some());
        if pressed {
            // The held button no longer owns a release.
            self.cancel_window_band(window_id);
        }
    }

    /// Bring every terminal window's band up to date with its platform metrics, hide its native
    /// title text while the band shows the name, and refit its panes when the band moved them.
    pub(in crate::app) fn refresh_window_bands(&mut self) {
        let mut refits = Vec::new();
        let palette_window = self.palette_window();
        for (window_id, window) in &mut self.registry.windows {
            let Some(native) = window.window.as_ref() else {
                // When: the window has no native window yet, there is nothing to measure.
                continue;
            };
            let band = titlebar::read_window_band(native, window.titlebar);
            titlebar::sync_fullscreen_shadow(native, window.titlebar);
            let focused = native.has_focus();
            let maximized = native.is_maximized();
            let Some(renderer) = window.renderer.as_mut() else {
                // When: a test-seeded window has no renderer, it draws no band.
                continue;
            };
            let caption = settle_caption(
                CaptionState { maximized, focused, ..window.caption },
                palette_window == Some(*window_id),
                band != renderer.window_band(),
            );
            if renderer.set_window_band(band) {
                // The band's height or insets changed: the panes refit to the new top inset.
                refits.push(*window_id);
            }
            // The snap layouts hook follows the band drawn now: buttons, height and scale.
            titlebar::sync_snap_layouts(
                native,
                band,
                renderer.band_height(),
                renderer.scale_factor(),
            );
            if caption != window.caption {
                window.caption = caption;
            }
            if renderer.set_caption_state(window.caption) {
                window.mark_redraw(RedrawCause::StatusLine);
                self.dispatch_redraws.request(*window_id, window);
            }
        }
        for window_id in refits {
            self.refit_window_panes(window_id);
        }
    }

    /// Refit `window_id`'s panes to its renderer's current insets and draw them.
    fn refit_window_panes(&mut self, window_id: WindowId) {
        let Some(window) = self.registry.windows.get_mut(&window_id) else {
            // When: window_id has no entry in windows, it closed and has no panes to refit.
            return;
        };
        let rects = App::compute_pane_rects_for(window);
        let Some(renderer) = window.renderer.as_ref() else {
            // When: the window has no renderer, there are no cell metrics to refit against.
            return;
        };
        let (cell_width, cell_height) = renderer.cell_size();
        let inset = [
            renderer.padding_left_px(),
            renderer.padding_right_px(),
            renderer.padding_top_px(),
            renderer.padding_bottom_px(),
        ];
        super::resize_panes_to_rects(&window.panes, &rects, cell_width, cell_height, inset);
        window.mark_redraw(RedrawCause::Topology);
        self.dispatch_redraws.request(window_id, window);
        self.refresh_all_window_minimums();
    }

    /// Route a left press at raster-px `(pointer_x, pointer_y)` in `window_id`'s band. Returns
    /// `true` when the band consumed it; its release is then consumed too.
    pub(in crate::app) fn press_window_band(
        &mut self,
        window_id: WindowId,
        pointer_x: f32,
        pointer_y: f32,
    ) -> bool {
        let Some(window) = self.registry.windows.get_mut(&window_id) else {
            // When: window_id has no entry in windows, it closed and nothing consumes the press.
            return false;
        };
        let press = classify_band_press(window, pointer_x, pointer_y);
        match press {
            BandPress::Outside => {
                // When: Outside, the panes take the press.
                return false;
            }
            BandPress::Segment => {
                // When: Segment, the strip's own routing runs the click; the band still owns the
                // release, so it never reaches a pane or the tab bar.
                window.band_press_consumed = true;
                return false;
            }
            BandPress::Caption(button) => {
                // Caption, the button is pressed now and acts on release over itself.
                window.caption.pressed = Some(button);
                window.caption.hovered = Some(button);
            }
            BandPress::Edge(edge) => {
                // When: Edge, the system resizes the window from that edge until release.
                window.caption = CaptionState { pressed: None, hovered: None, ..window.caption };
                if let Some(native) = window.window.as_ref() {
                    // When: the native window exists; a failed start leaves it where it is.
                    let _ = native.drag_resize_window(resize_direction(edge));
                }
            }
            BandPress::Empty => {
                // When: Empty band space, a press drags the window and a double-click runs the
                // titlebar action.
                let now = Instant::now();
                let double = is_double_click(
                    titlebar::native_click_count(),
                    window.band_last_press,
                    now,
                    (pointer_x, pointer_y),
                    double_click_limits(),
                );
                window.caption = CaptionState { pressed: None, ..window.caption };
                if double {
                    // double completes a double-click, the system titlebar action runs.
                    window.band_last_press = None;
                    if let Some(native) = window.window.as_ref() {
                        // the native window exists, zoom, minimise or nothing, as set.
                        titlebar::run_double_click(native, titlebar::system_double_click_action());
                    }
                } else {
                    // When: double is false, a single press drags the window until release.
                    window.band_last_press = Some((now, (pointer_x, pointer_y)));
                    if let Some(native) = window.window.as_ref() {
                        // When: the native window exists; a failed start leaves it in place.
                        let _ = native.drag_window();
                    }
                }
            }
        }
        window.band_press_consumed = true;
        self.sync_caption_state(window_id);
        true
    }

    /// Route a left release in `window_id`. Returns whether it ends a press the band consumed (a
    /// caption button acts if the release is over the button it pressed) and whether that button
    /// was Close, which the caller replays as the window's close request.
    pub(in crate::app) fn release_window_band(
        &mut self,
        window_id: WindowId,
        pointer_x: f32,
        pointer_y: f32,
    ) -> (bool, bool) {
        let Some(window) = self.registry.windows.get_mut(&window_id) else {
            // When: window_id has no entry in windows, it closed and owns no release.
            return (false, false);
        };
        if !std::mem::take(&mut window.band_press_consumed) {
            // When: band_press_consumed was false, the release belongs to the usual routing.
            return (false, false);
        }
        let pressed = window.caption.pressed.take();
        let under = match classify_band_press(window, pointer_x, pointer_y) {
            BandPress::Caption(button) => Some(button),
            _ => None,
        };
        // This window's overlay, open at release, takes the pointer: the press is cancelled.
        let modal = self.palette_window() == Some(window_id);
        let action = release_action(pressed, under, modal);
        self.sync_caption_state(window_id);
        let close = action.is_some_and(|button| self.run_caption_button(window_id, button));
        (true, close)
    }

    /// Track the pointer at raster-px `(pointer_x, pointer_y)` over `window_id`'s band: hover a
    /// caption button, show a resize cursor on an edge. A pointer that leaves its pressed button
    /// cancels the press, since capture loss is not reported. Returns the cursor an edge wants.
    pub(in crate::app) fn hover_window_band(
        &mut self,
        window_id: WindowId,
        pointer_x: f32,
        pointer_y: f32,
    ) -> Option<CursorIcon> {
        let window = self.registry.windows.get_mut(&window_id)?;
        let press = classify_band_press(window, pointer_x, pointer_y);
        let hovered = match press {
            BandPress::Caption(button) => Some(button),
            _ => None,
        };
        // A pointer that left its pressed button cancels the press: its release can no longer act.
        if window.caption.pressed.is_some() && window.caption.pressed != hovered {
            window.caption.pressed = None;
        }
        window.caption.hovered = hovered;
        self.sync_caption_state(window_id);
        match press {
            BandPress::Edge(edge) => Some(edge_cursor(edge)),
            _ => None,
        }
    }

    /// Clear `window_id`'s caption hover and press: the pointer left, focus moved, the window
    /// resized or went full screen, or a modal opened.
    pub(in crate::app) fn cancel_window_band(&mut self, window_id: WindowId) {
        let Some(window) = self.registry.windows.get_mut(&window_id) else {
            // When: window_id has no entry in windows, it closed with no caption state left.
            return;
        };
        window.caption.hovered = None;
        window.caption.pressed = None;
        self.sync_caption_state(window_id);
    }

    /// Follow `event` for `window_id`'s band before the usual routing: this window's open palette,
    /// leaving, focus loss, a resize or a scale change cancels a caption press, and the pointer
    /// hovers caption buttons and edges.
    pub(in crate::app) fn track_window_band(
        &mut self,
        window_id: WindowId,
        event: &winit::event::WindowEvent,
    ) {
        use winit::event::WindowEvent;
        // Before anything else: this window's own palette, open now, cancels a held button.
        self.cancel_caption_under_palette(window_id);
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                let cursor =
                    self.hover_window_band(window_id, position.x as f32, position.y as f32);
                let window = self.registry.windows.get_mut(&window_id);
                if let Some(window) = window {
                    // the window is live, its resize cursor follows the pointer's edge.
                    let on_edge = cursor.is_some();
                    if let (Some(icon), Some(native)) = (cursor, window.window.as_ref()) {
                        // cursor names an edge icon and the native window exists, show it.
                        native.set_cursor(icon);
                    } else if window.band_edge_cursor && !on_edge {
                        // When: band_edge_cursor was set and the pointer left the edge, the
                        // default cursor returns until a pane or link sets its own.
                        if let Some(native) = window.window.as_ref() {
                            native.set_cursor(CursorIcon::Default);
                        }
                    }
                    window.band_edge_cursor = on_edge;
                }
            }
            WindowEvent::CursorLeft { .. }
            | WindowEvent::Focused(false)
            | WindowEvent::Resized(_)
            | WindowEvent::ScaleFactorChanged { .. } => {
                // CursorLeft, Focused(false), Resized or ScaleFactorChanged, a caption press
                // can no longer complete, so hover and press clear.
                self.cancel_window_band(window_id);
            }
            _ => {
                // When: any other event, the band's state stays.
            }
        }
    }

    /// Hand `window_id`'s caption state to its renderer, redrawing when it changed.
    fn sync_caption_state(&mut self, window_id: WindowId) {
        let Some(window) = self.registry.windows.get_mut(&window_id) else {
            // When: window_id has no entry in windows, there is no renderer to tell.
            return;
        };
        let state = window.caption;
        let changed =
            window.renderer.as_mut().is_some_and(|renderer| renderer.set_caption_state(state));
        if changed {
            // changed, the buttons look different, so the window redraws once.
            window.mark_redraw(RedrawCause::StatusLine);
            self.dispatch_redraws.request(window_id, window);
        }
    }

    /// Run minimise or maximise for `window_id`. Close is returned to the caller instead, which
    /// replays it as the window's own `CloseRequested`, so it takes the same lifecycle.
    fn run_caption_button(&mut self, window_id: WindowId, button: CaptionButton) -> bool {
        let Some(native) =
            self.registry.windows.get(&window_id).and_then(|window| window.window.clone())
        else {
            // When: window_id has no native window, there is nothing to minimise or close.
            return false;
        };
        match button {
            CaptionButton::Minimize => native.set_minimized(true),
            CaptionButton::Maximize => native.set_maximized(!native.is_maximized()),
            CaptionButton::Close => {
                // When: Close, the caller replays CloseRequested so the window closes normally.
                return true;
            }
        }
        false
    }

    /// Take the left release of a press the band consumed, before any overlay or pane routing,
    /// so a palette opened while a caption button was held can neither eat it nor leave the band
    /// waiting for a later release. Returns whether it was taken.
    pub(in crate::app) fn take_band_release(
        &mut self,
        event_loop: &winit::event_loop::ActiveEventLoop,
        window_id: WindowId,
        event: &winit::event::WindowEvent,
    ) -> bool {
        use winit::event::{ElementState, MouseButton, WindowEvent};
        let release = matches!(
            event,
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            }
        );
        let owned =
            self.registry.windows.get(&window_id).is_some_and(|window| window.band_press_consumed);
        if !(release && owned) {
            // When: not a left release, or the band took no press, routing continues as usual.
            return false;
        }
        self.route_band_left_input(event_loop, window_id, ElementState::Released)
    }

    /// Run caption `button` of `window_id` for an assistive technology's click: minimise and
    /// maximise at once, Close as the window's own close request on the next dispatch.
    pub(in crate::app) fn click_caption_button(
        &mut self,
        window_id: WindowId,
        button: CaptionButton,
    ) {
        if self.run_caption_button(window_id, button) {
            // Close closes the window as the system button would, through its own close request.
            self.pending_caption_close.push(window_id);
        }
    }

    /// The separate name band `window_id` presented, as assistive technology reads it: "Window"
    /// and the band's own spoken text (already redacted under `hide_sensitive`), with raster-px
    /// bounds and whether its click does something; `None` without a separate name band.
    pub(in crate::app) fn accessible_name_band(
        &self,
        window_id: WindowId,
    ) -> Option<(String, accesskit::Rect, bool)> {
        let window = self.registry.windows.get(&window_id)?;
        let band = window.title_band.as_ref()?;
        let placed = band.segments.iter().find(|segment| segment.id == StatusSegmentId::Window)?;
        let renderer = window.renderer.as_ref()?;
        let spoken = renderer
            .band_view()
            .segments
            .iter()
            .find(|segment| segment.id == StatusSegmentId::Window)?
            .spoken
            .clone();
        let bounds = accesskit::Rect {
            x0: f64::from(placed.x),
            y0: f64::from(band.top),
            x1: f64::from(placed.x + placed.width),
            y1: f64::from(band.top + band.height),
        };
        Some((format!("{}: {}", spoken.name, spoken.value), bounds, placed.click.is_some()))
    }

    /// The caption buttons `window_id` exposes to assistive technology, with localized names and
    /// raster-px bounds; empty without self-drawn buttons.
    pub(in crate::app) fn accessible_caption(
        &self,
        window_id: WindowId,
    ) -> Vec<(CaptionButton, String, accesskit::Rect)> {
        let Some(window) = self.registry.windows.get(&window_id) else {
            // When: window_id has no entry in windows, it exposes no buttons.
            return Vec::new();
        };
        let Some(renderer) = window.renderer.as_ref() else {
            // When: the window has no renderer, there is no band to read.
            return Vec::new();
        };
        let band = renderer.window_band();
        let (width, _) = renderer.surface_size();
        let maximized = window.window.as_ref().is_some_and(|native| native.is_maximized());
        sonicterm_ui::window_band::caption_buttons(
            &band,
            width as f32,
            renderer.band_height(),
            renderer.scale_factor(),
        )
        .into_iter()
        .map(|(button, rect)| {
            let key = match button {
                CaptionButton::Minimize => "caption-minimize",
                CaptionButton::Maximize if maximized => "caption-restore",
                CaptionButton::Maximize => "caption-maximize",
                CaptionButton::Close => "caption-close",
            };
            let bounds = accesskit::Rect {
                x0: f64::from(rect.left),
                y0: f64::from(rect.top),
                x1: f64::from(rect.left + rect.width),
                y1: f64::from(rect.top + rect.height),
            };
            (button, self.i18n.translate(key), bounds)
        })
        .collect()
    }

    /// Route a left button `state` change in `window_id` through the titlebar band first. Returns
    /// whether the band consumed it; a released Close button replays `CloseRequested`.
    pub(in crate::app) fn route_band_left_input(
        &mut self,
        event_loop: &winit::event_loop::ActiveEventLoop,
        window_id: WindowId,
        state: winit::event::ElementState,
    ) -> bool {
        let Some((cursor_x, cursor_y)) = self
            .registry
            .windows
            .get(&window_id)
            .map(|window| (window.cursor_pos.0 as f32, window.cursor_pos.1 as f32))
        else {
            // When: window_id has no entry in windows, the band has nothing to route.
            return false;
        };
        match state {
            winit::event::ElementState::Pressed => {
                self.press_window_band(window_id, cursor_x, cursor_y)
            }
            winit::event::ElementState::Released => {
                let (consumed, close) = self.release_window_band(window_id, cursor_x, cursor_y);
                if close {
                    // close, the Close button ends the window as the system button would.
                    winit::application::ApplicationHandler::window_event(
                        self,
                        event_loop,
                        window_id,
                        winit::event::WindowEvent::CloseRequested,
                    );
                }
                consumed
            }
        }
    }
}

#[cfg(test)]
#[path = "titlebar_band_tests.rs"]
mod titlebar_band_tests;
