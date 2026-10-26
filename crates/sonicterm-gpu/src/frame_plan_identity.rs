//! Frame identity comparison and exact changed-field classification.

use super::*;

impl ChangeClass {
    /// True when any narrow class, but not necessarily `full`, is set.
    pub(crate) fn any_class(self) -> bool {
        self.tab_band
            || self.status_band
            || self.cursor
            || self.focus
            || self.selection
            || self.scrollbar
    }
}

impl WindowIdentity {
    /// Classify every field that differs from `previous`.
    ///
    /// Both identities are destructured without a rest pattern, so a new field fails to compile
    /// here until it is given a class. `hovered_url_cells` has its own row path in the planner.
    pub(crate) fn classify(&self, previous: &Self) -> ChangeClass {
        let Self {
            selection,
            copy_mode,
            quick_select_hint_count,
            cursor_visible,
            tab,
            search_hash,
            palette_hash,
            ime_hash,
            notification_hash,
            width,
            height,
            tab_hash,
            status_hash,
            viewport_top_abs,
            cursor_shape,
            cursor_blink,
            window_focused,
            pane_focus_flash_bucket,
            hover_tab,
            close_override,
            broadcast_participants_hash,
            inline_media_hash,
            hovered_url_cells: _,
            process_privileged,
            subpixel_aa,
            background,
            style_rev,
            renderer_hash,
            overlay_active,
            cursor_cell,
        } = self;
        let Self {
            selection: previous_selection,
            copy_mode: previous_copy_mode,
            quick_select_hint_count: previous_quick_select_hint_count,
            cursor_visible: previous_cursor_visible,
            tab: previous_tab,
            search_hash: previous_search_hash,
            palette_hash: previous_palette_hash,
            ime_hash: previous_ime_hash,
            notification_hash: previous_notification_hash,
            width: previous_width,
            height: previous_height,
            tab_hash: previous_tab_hash,
            status_hash: previous_status_hash,
            viewport_top_abs: previous_viewport_top_abs,
            cursor_shape: previous_cursor_shape,
            cursor_blink: previous_cursor_blink,
            window_focused: previous_window_focused,
            pane_focus_flash_bucket: previous_pane_focus_flash_bucket,
            hover_tab: previous_hover_tab,
            close_override: previous_close_override,
            broadcast_participants_hash: previous_broadcast_participants_hash,
            inline_media_hash: previous_inline_media_hash,
            hovered_url_cells: _,
            process_privileged: previous_process_privileged,
            subpixel_aa: previous_subpixel_aa,
            background: previous_background,
            style_rev: previous_style_rev,
            renderer_hash: previous_renderer_hash,
            overlay_active: previous_overlay_active,
            cursor_cell: previous_cursor_cell,
        } = previous;
        ChangeClass {
            full: copy_mode != previous_copy_mode
                || quick_select_hint_count != previous_quick_select_hint_count
                || tab != previous_tab
                || search_hash != previous_search_hash
                || palette_hash != previous_palette_hash
                || ime_hash != previous_ime_hash
                || notification_hash != previous_notification_hash
                || width != previous_width
                || height != previous_height
                || viewport_top_abs != previous_viewport_top_abs
                || pane_focus_flash_bucket != previous_pane_focus_flash_bucket
                || broadcast_participants_hash != previous_broadcast_participants_hash
                || inline_media_hash != previous_inline_media_hash
                || subpixel_aa != previous_subpixel_aa
                || background != previous_background
                || style_rev != previous_style_rev
                || renderer_hash != previous_renderer_hash
                || overlay_active != previous_overlay_active,
            tab_band: tab_hash != previous_tab_hash
                || hover_tab != previous_hover_tab
                || close_override != previous_close_override
                || process_privileged != previous_process_privileged,
            status_band: status_hash != previous_status_hash,
            cursor: cursor_visible != previous_cursor_visible
                || cursor_shape != previous_cursor_shape
                || cursor_blink != previous_cursor_blink
                || cursor_cell != previous_cursor_cell,
            focus: window_focused != previous_window_focused,
            selection: selection != previous_selection,
            scrollbar: false,
        }
    }
}

impl PaneIdentity {
    /// Classify every field that differs from `previous`. Revision and dirty generation are dirt,
    /// damaged through the plan's dirty slots, so they set no class.
    pub(crate) fn classify(&self, previous: &Self) -> ChangeClass {
        let Self {
            id,
            revision: _,
            dirty_generation: _,
            rect,
            cols,
            rows,
            scrollback_len,
            viewport_top_abs,
            view_top_abs,
            is_active,
            is_alt,
            scrollbar_bucket,
        } = self;
        let Self {
            id: previous_id,
            revision: _,
            dirty_generation: _,
            rect: previous_rect,
            cols: previous_cols,
            rows: previous_rows,
            scrollback_len: previous_scrollback_len,
            viewport_top_abs: previous_viewport_top_abs,
            view_top_abs: previous_view_top_abs,
            is_active: previous_is_active,
            is_alt: previous_is_alt,
            scrollbar_bucket: previous_scrollbar_bucket,
        } = previous;
        ChangeClass {
            full: id != previous_id
                || rect != previous_rect
                || cols != previous_cols
                || rows != previous_rows
                || scrollback_len != previous_scrollback_len
                || viewport_top_abs != previous_viewport_top_abs
                || view_top_abs != previous_view_top_abs
                || is_active != previous_is_active
                || is_alt != previous_is_alt,
            scrollbar: scrollbar_bucket != previous_scrollbar_bucket,
            tab_band: false,
            status_band: false,
            cursor: false,
            focus: false,
            selection: false,
        }
    }
}

impl FrameKey {
    /// The key without its dirt fields, each pane's revision and dirty generation, which only say
    /// that rows changed. The completeness certificate compares scenes with it: a partial frame of the
    /// same scene adds its missing characters, and any other field change is a new scene.
    pub(crate) fn scene(&self) -> Self {
        let mut scene = self.clone();
        for pane in &mut scene.panes {
            pane.revision = 0;
            pane.dirty_generation = 0;
        }
        scene
    }
}

#[cfg(test)]
#[path = "frame_plan_identity_tests.rs"]
mod frame_plan_identity_tests;
