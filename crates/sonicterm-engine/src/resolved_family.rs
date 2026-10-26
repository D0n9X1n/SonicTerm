//! The family of the face a stack resolved for regular text.

use crate::FontStack;

impl FontStack {
    /// The family of the face this stack resolved for regular text: the configured family when it
    /// is installed, else the platform default that stood in. `None` when no face loaded.
    pub fn resolved_family(&self) -> Option<String> {
        let font = self.font_for_style(false, false).ok()?;
        let handles = font.clone_handles();
        handles.first().map(|face| face.names().family.clone())
    }
}

#[cfg(test)]
#[path = "resolved_family_tests.rs"]
mod resolved_family_tests;
