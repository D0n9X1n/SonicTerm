//! The App's font state: the packaged font directories, the sizes and family the loaded config
//! asked for (what reset actions return to), and the installed-font catalog and picker state.

/// Font state the App owns apart from each window's renderer.
pub(in crate::app) struct FontState {
    /// Packaged font directories retained for every renderer and live font rebuild.
    pub(in crate::app) font_dirs: Vec<std::path::PathBuf>,
    /// Font size the loaded config asked for, in logical px. `ResetFontSize`
    /// returns here rather than to the compile-time default, so Cmd+0 restores
    /// the user's configured size instead of a value they never chose.
    ///
    /// Tracks the config this session has *loaded*, not the file on disk.
    /// Editing `sonicterm.toml` does not move it — the background watcher may
    /// apply other settings from that edit, but the reset target stays where
    /// the session started. Only an explicit `ReloadConfig` moves it.
    pub(in crate::app) configured_font_size: f32,
    /// Monochrome-text `weight_scale` the loaded config asked for. Follows the
    /// same rule as [`Self::configured_font_size`]: `ResetFontWeight` returns
    /// here, and only an explicit reload moves it.
    pub(in crate::app) configured_weight_scale: f32,
    /// Font family this session started with: the loaded config's `[font] family`, or the platform
    /// default the font catalog adopted when the config names none. Only an explicit reload, or a
    /// save, moves it after that.
    pub(in crate::app) configured_font_family: String,
    /// Installed monospace families, enumerated once per process off the UI thread.
    pub(in crate::app) font_catalog: super::font_catalog::FontCatalog,
    /// Enumerates installed monospace families for the catalog worker; tests inject a counter.
    pub(in crate::app) font_enumerator: super::font_catalog::FontEnumerator,
    /// The live family when the font picker opened, restored when the picker is cancelled.
    pub(in crate::app) font_picker_original: Option<String>,
    /// Moves on every reload, save and picker choice of the font family, so a catalog report the
    /// worker started before one of them is recognised as stale and adopts nothing.
    pub(in crate::app) font_family_generation: u64,
    /// Test recorder: each renderer-less window and the font request `apply_live_font` delivered to
    /// it, through the seam that delivers a renderer's.
    #[cfg(test)]
    pub(in crate::app) live_font_log:
        Vec<(winit::window::WindowId, super::live_font::LiveFontRequest)>,
}
