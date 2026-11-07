//! The App's cross-window interaction state: the command palette and what it is attached to, rename
//! and tab-edit targets, pointer captures held by fields, OS drag and drop in both directions, and
//! input broadcast. It spans windows, so it belongs to no single window's state.

use super::*;

/// Cross-window interaction: the palette, edit targets, field pointer captures, OS drag and drop, broadcast.
pub(in crate::app) struct InteractionState {
    pub(in crate::app) command_palette: CommandPalette,
    /// Which window the (single, modal) command palette is attached to.
    /// `None` means it is closed OR attached to the main window; `Some(id)`
    /// means that child window. Both the main and child render paths consult
    /// it so the palette paints only on the window it was opened from —
    /// without it, Cmd+Shift+P typed in a torn-out child opened the palette
    /// on the original main window.
    pub(in crate::app) palette_attached_window: Option<WindowId>,
    /// Stable editor target, including main; an absent key never falls back to another window.
    pub(in crate::app) window_rename_target: Option<sonicterm_types::WindowKey>,
    /// Tab captured when a rename or color editor opened; submit edits only that live tab.
    pub(in crate::app) tab_edit_target: Option<overlays::TabEditTarget>,
    /// One modal press retains its source and target until release or an intervening input change.
    pub(in crate::app) palette_pointer_capture: Option<overlays::PalettePointerCapture>,
    /// Left-button gesture inside a palette or search query, bound to its source window and field.
    pub(in crate::app) field_pointer_capture: Option<field_pointer::FieldPointerCapture>,
    /// Windows whose cancelled field gesture still owes a left release after another window
    /// started a field gesture; bounded by live windows and cleared on release or close.
    pub(in crate::app) field_owed_releases: Vec<winit::window::WindowId>,
    /// Set the moment a held-tab drag
    /// crosses [`os_drag::OS_DRAG_THRESHOLD_PX`] from its press point,
    /// before the user releases the button. Guards
    /// [`Self::try_os_drag_handoff`] in the `CursorMoved` path so the
    /// OS-level drag session starts mid-gesture (cursor still down)
    /// rather than waiting until mouse-up — which was too late for
    /// `DoDragDrop` to capture the cursor across windows. Cleared on
    /// `cancel_drag_session` and at every fresh mouse-down so a new
    /// gesture re-arms cleanly.
    pub(in crate::app) os_drag_handoff_started: bool,
    /// Platform OS-drag backend. Distinct from `os_drag_sink` (wire-format
    /// publication): Windows drives OLE `DoDragDrop`; macOS currently publishes
    /// the pasteboard payload and posts a cancelled outcome without cursor capture.
    /// Installed by the platform bin (`sonicterm-mac` / `sonicterm-windows`)
    /// at startup. `None` in tests + on platforms without an impl.
    pub(in crate::app) os_drag_backend: Option<Box<dyn os_drag::OsTabDragBackend>>,
    /// Shared mailbox the [`os_drag::OsTabDragBackend`] writes pending
    /// drag outcomes into. Drained by `do_user_event` on every
    /// `UserEvent::DragMoved` / `DragEnded` wake.
    pub(in crate::app) os_drag_pending: Arc<os_drag::PendingDragOutcome>,
    /// Shared tab-bar snapshot registry. The App publishes the live
    /// per-window tab bar layout into this on every redraw (see
    /// `publish_os_drag_bar_snapshot`); a Phase-C2 OS-drag backend
    /// reads from it inside its drop callback (Windows
    /// IDropTarget::Drop / macOS NSDraggingDestination::performDrop)
    /// to resolve the raw screen-coordinate drop into a real
    /// `(WindowId, slot)` pair before posting a `DroppedOnBar` outcome.
    pub(in crate::app) os_drag_bars: Arc<os_drag::TabBarRegistry>,
    /// Captured window and tab identity until the native drag outcome is consumed.
    pub(in crate::app) os_drag_source: Option<(WindowId, sonicterm_ui::tabs::TabId)>,
    /// OS-drag tab payloads received before the main [`WindowState`] exists.
    /// Startup pasteboard / OLE deliveries can arrive before `do_resumed`
    /// inserts `main_window_id`; queue them so the destination tab is created
    /// after main is available instead of silently dropping the payload.
    pub(in crate::app) pending_os_drag_payloads: Vec<crate::os_drag::TabPayload>,
    /// Native winit paths collected by source window until the current event-loop turn ends.
    pub(in crate::app) pending_winit_file_drops: HashMap<WindowId, Vec<PathBuf>>,
    /// Broadcast-input mode. When enabled, bytes typed into `source_pane`
    /// are mirrored into matching receiver panes after the source PTY write.
    pub(in crate::app) broadcast: BroadcastState,
}
