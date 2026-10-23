//! `OSC 9;4` progress on the Dock tile's badge. AppKit has no Dock progress bar without a custom
//! tile view, so the badge shows text only; the Dock is app-wide.

use objc2_app_kit::NSApplication;
use objc2_foundation::{MainThreadMarker, NSString};
use sonicterm_app::taskbar_progress::{
    badge_text, ProgressReport, ProgressSurface, TaskbarProgressBackend,
};

/// The Dock tile badge.
#[derive(Debug, Default)]
pub(crate) struct DockBadge;

impl TaskbarProgressBackend for DockBadge {
    fn per_window(&self) -> bool {
        false
    }

    fn deliver(&mut self, _surface: ProgressSurface, report: Option<ProgressReport>) {
        let Some(main_thread) = MainThreadMarker::new() else {
            // When: no MainThreadMarker exists, this is not the AppKit thread, which never calls it.
            tracing::debug!(target: "sonicterm_mac::dock", "Dock badge skipped off the main thread");
            return;
        };
        let tile = NSApplication::sharedApplication(main_thread).dockTile();
        let label = badge_text(report).map(|text| NSString::from_str(&text));
        tile.setBadgeLabel(label.as_deref());
    }
}
