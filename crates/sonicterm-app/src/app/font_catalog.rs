//! The installed monospace families for the font picker, and the family a fresh start adopts.
//!
//! Enumerating installed fonts parses every font file, so it runs on a worker thread and only the
//! first time the picker opens: a launch that never opens the picker never pays for it. A fresh
//! start's family comes from the face the body stack resolved, not from the catalog.

use std::path::Path;

use super::{App, UserEvent};

/// Where the installed-family enumeration stands.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) enum FontCatalog {
    /// Not started: the picker has not opened yet.
    #[default]
    Idle,
    /// The worker is enumerating.
    Loading,
    /// The installed monospace families, sorted.
    Ready(Vec<String>),
}

/// What the worker reports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FontCatalogReport {
    /// Installed monospace families, sorted.
    pub(crate) families: Vec<String>,
}

/// The function that enumerates installed monospace families; tests inject a counting one.
pub(crate) type FontEnumerator = fn() -> anyhow::Result<Vec<String>>;

/// Run `enumerate` on a worker thread and hand its report to `deliver`; returns its handle at once.
/// A worker that cannot start leaves the catalog loading and returns `None`.
pub(crate) fn spawn<E, D>(enumerate: E, deliver: D) -> Option<std::thread::JoinHandle<()>>
where
    E: FnOnce() -> anyhow::Result<Vec<String>> + Send + 'static,
    D: FnOnce(FontCatalogReport) + Send + 'static,
{
    let job = move || {
        let families = enumerate().unwrap_or_else(|error| {
            tracing::warn!("installed font enumeration failed: {error:#}");
            Vec::new()
        });
        deliver(FontCatalogReport { families });
    };
    match std::thread::Builder::new().name("sonicterm-font-catalog".into()).spawn(job) {
        Ok(handle) => Some(handle),
        Err(error) => {
            // The catalog then stays loading, and the picker lists only the current family.
            tracing::warn!("font catalog worker did not start: {error}");
            None
        }
    }
}

/// Whether the config file at `path` names `[font] family`. A missing file names none; a file that
/// cannot be read for another reason counts as naming one, so a read error never replaces a family.
pub(crate) fn config_sets_family(path: &Path) -> bool {
    // A missing file is a fresh install; any other read failure keeps whatever family it may name.
    match std::fs::read_to_string(path) {
        Ok(text) => sonicterm_cfg::config::Config::sets_font_family(&text),
        Err(error) => error.kind() != std::io::ErrorKind::NotFound,
    }
}

impl App {
    /// Start the catalog worker the first time the picker opens; later opens reuse the loading or
    /// ready catalog. Without an event loop, as in unit tests, the report is dropped.
    pub(super) fn start_font_catalog(&mut self) {
        if self.font_catalog != FontCatalog::Idle {
            // When: `font_catalog` already started or finished, no second enumeration runs.
            return;
        }
        self.font_catalog = FontCatalog::Loading;
        let proxy = self.event_loop_proxy.clone();
        // The handle is kept but never joined: finish_session does not wait for enumeration. After the
        // loop ends `send_event` returns `EventLoopClosed`, and a report after shutdown is ignored.
        self.font_catalog_worker = spawn(self.font_enumerator, move |report| {
            if let Some(proxy) = proxy {
                // When: `proxy` is Some, an event loop is running to receive the report.
                let _ = proxy.send_event(UserEvent::FontCatalogReady { families: report.families });
            }
        });
    }

    /// Record the worker's report as the picker's rows, refreshing an open picker in place.
    pub(super) fn adopt_font_catalog(&mut self, families: Vec<String>) {
        if self.session_finished.is_some() {
            // When: `session_finished` is set, the session has ended and a late report changes nothing.
            return;
        }
        tracing::info!(monospace_families = families.len(), "font catalog ready");
        self.font_catalog = FontCatalog::Ready(families);
        // A picker opened while the catalog loaded now lists it.
        self.refresh_font_family_picker();
    }

    /// At startup, when the config names no family and the live family is still the platform
    /// default, the family of the face the main renderer resolved for regular text becomes the live
    /// family and the baseline, in memory only. Logs the session family.
    pub(super) fn adopt_resolved_font_family(&mut self) {
        let resolved = self.main_renderer().and_then(|renderer| renderer.resolved_font_family());
        let config_path =
            self.runtime_config_path.clone().or_else(sonicterm_cfg::config::Config::default_path);
        let family_configured = config_path.as_deref().is_some_and(config_sets_family);
        let generation = self.font_family_generation;
        self.adopt_resolved_family_from(
            sonicterm_gpu::DEFAULT_MONOSPACE_FAMILIES[0],
            resolved,
            family_configured,
            generation,
        );
    }

    /// [`Self::adopt_resolved_font_family`] with an explicit platform `default`, the `resolved`
    /// face's family and the font-family `generation` the reading was taken at.
    pub(super) fn adopt_resolved_family_from(
        &mut self,
        default: &str,
        resolved: Option<String>,
        family_configured: bool,
        generation: u64,
    ) {
        // Only the platform default may be replaced, only when the config names no family, and only
        // while no reload, save or picker choice has moved the family since the reading was taken.
        let untouched = !family_configured
            && generation == self.font_family_generation
            && self.config.font.family == default
            && self.configured_font_family == default;
        let adopted = resolved.filter(|family| untouched && *family != self.config.font.family);
        if let Some(family) = adopted {
            // A face other than the default stood in for it, so it becomes the session's family.
            self.configured_font_family = family.clone();
            self.set_font_family(&family);
        }
        tracing::info!(family = %self.config.font.family, "font family resolved");
    }
}

#[cfg(test)]
#[path = "font_catalog_tests.rs"]
mod font_catalog_tests;
