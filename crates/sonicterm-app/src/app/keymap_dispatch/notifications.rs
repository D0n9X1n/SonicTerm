use std::time::Duration;

use crate::app::{App, FrontmostKind};

const NOTIFICATION_AUTO_CLOSE_DURATION: Duration = Duration::from_secs(5);

impl App {
    pub(in crate::app) fn show_notification_for_kind(
        &mut self,
        kind: FrontmostKind,
        level: sonicterm_ui::overlays::NotificationLevel,
        message: String,
    ) {
        self.show_notification_for_kind_until(kind, level, message, None);
    }

    pub(in crate::app) fn show_notification_for_kind_until(
        &mut self,
        kind: FrontmostKind,
        level: sonicterm_ui::overlays::NotificationLevel,
        message: String,
        expires_at: Option<std::time::Instant>,
    ) {
        let auto_close_at = std::time::Instant::now() + NOTIFICATION_AUTO_CLOSE_DURATION;
        let expires_at = Some(expires_at.map_or(auto_close_at, |at| at.min(auto_close_at)));
        let bubble = sonicterm_ui::overlays::NotificationBubble { level, message, expires_at };
        match kind {
            FrontmostKind::Child(id) => {
                // When: kind is FrontmostKind::Child(id), prefer that child's notification surface.
                if let Some(child) = self.windows.get_mut(&id) {
                    // When: windows.get_mut finds id, install the bubble and finish child routing.
                    child.notification = Some(bubble);
                    child.request_redraw();
                    return;
                }
            }
            FrontmostKind::Main | FrontmostKind::None | FrontmostKind::Other => {
                // When: kind is Main, None, or Other, use the main notification surface.
            }
        }
        if let Some(main) = self.main_mut() {
            main.notification = Some(bubble);
        }
        if let Some(window) = self.main_window() {
            window.request_redraw();
        }
    }

    pub(in crate::app) fn dismiss_notification_at(
        &mut self,
        kind: FrontmostKind,
        cursor_x: f32,
        cursor_y: f32,
    ) -> bool {
        let Some(layout) = self.notification_hit_layout(kind) else {
            // When: notification_hit_layout returns None, no visible close control can be hit.
            return false;
        };
        let inside = cursor_x >= layout.close.x
            && cursor_x < layout.close.x + layout.close.w
            && cursor_y >= layout.close.y
            && cursor_y < layout.close.y + layout.close.h;
        if !inside {
            // When: inside is false, the pointer missed the notification close control.
            return false;
        }
        match kind {
            FrontmostKind::Child(id) => {
                // When: kind is FrontmostKind::Child(id), dismiss that child's notification first.
                if let Some(child) = self.windows.get_mut(&id) {
                    // When: windows.get_mut finds id, clear its notification and finish child routing.
                    child.notification = None;
                    child.request_redraw();
                    return true;
                }
            }
            FrontmostKind::Main | FrontmostKind::None | FrontmostKind::Other => {
                // When: kind is Main, None, or Other, use the main notification surface.
            }
        }
        if let Some(main) = self.main_mut() {
            main.notification = None;
        }
        if let Some(window) = self.main_window() {
            window.request_redraw();
        }
        true
    }

    fn notification_hit_layout(
        &self,
        kind: FrontmostKind,
    ) -> Option<sonicterm_ui::overlays::NotificationBubbleLayout> {
        match kind {
            FrontmostKind::Child(id) => {
                let child = self.windows.get(&id)?;
                let message = child.notification.as_ref()?.message.clone();
                let renderer = child.renderer.as_ref()?;
                let window = child.window.as_ref()?;
                let size = window.inner_size();
                let tab_idx = child.tabs.active_index();
                let search_open =
                    child.tab_states.get(tab_idx).is_some_and(|tab| tab.search.is_some());
                let read_only = child.copy_mode.as_ref().is_some_and(|mode| mode.is_read_only());
                Some(
                    renderer
                        .notification_layout(
                            &message,
                            (size.width as f32, size.height as f32),
                            u8::from(read_only) + u8::from(search_open),
                        )
                        .geometry,
                )
            }
            FrontmostKind::Main | FrontmostKind::None | FrontmostKind::Other => {
                let main = self.main()?;
                let message = main.notification.as_ref()?.message.clone();
                let renderer = main.renderer.as_ref()?;
                let window = main.window.as_ref()?;
                let size = window.inner_size();
                let tab_idx = main.tabs.active_index();
                let search_open =
                    main.tab_states.get(tab_idx).is_some_and(|tab| tab.search.is_some());
                let read_only = main.copy_mode.as_ref().is_some_and(|mode| mode.is_read_only());
                Some(
                    renderer
                        .notification_layout(
                            &message,
                            (size.width as f32, size.height as f32),
                            u8::from(read_only) + u8::from(search_open),
                        )
                        .geometry,
                )
            }
        }
    }
}
