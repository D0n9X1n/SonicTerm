//! The App's configuration sources: the platform's normalizer, the theme and keymap loaders a
//! reload or theme change reads through, an isolated smoke run's config path, and the startup load
//! outcomes waiting for the first window. The loaded `Config` itself stays on the App, which every
//! path reads.

/// Where the App's configuration comes from and how it is normalized.
pub(in crate::app) struct ConfigState {
    /// Native-platform capability policy applied before config affects app state.
    pub(crate) config_normalizer: super::ConfigNormalizer,
    /// Optional theme loader, set by `run_with`; used to reload a theme by name live.
    pub(crate) theme_loader: Option<super::ThemeLoader>,
    /// Optional keymap loader, set by `run_with`.
    pub(crate) keymap_loader: Option<super::KeymapLoader>,
    /// Explicit config path used by an isolated runtime smoke; ordinary runs use the user path.
    pub(in crate::app) runtime_config_path: Option<std::path::PathBuf>,
    /// Startup config, theme and keymap load outcomes, until the first main window records them.
    pub(in crate::app) startup_loads: Option<super::config_apply::StartupLoads>,
    /// The open theme picker's original theme and loaded candidates; `None` when it is closed.
    pub(in crate::app) theme_picker: Option<super::overlays::ThemePickerState>,
}
