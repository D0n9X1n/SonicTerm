//! The installed monospace families, enumerated once per process off the UI thread.
//!
//! The first frame never waits for it: until the report arrives, the font stack's fallback chain
//! already draws with a platform default. On a start whose config names no `[font] family`, the
//! report picks the first platform default the host has installed, in memory only.

use std::path::{Path, PathBuf};

use super::{App, UserEvent};

/// Where the installed-family enumeration stands.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) enum FontCatalog {
    /// Not started: the main window does not exist yet.
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
    /// Whether the config file named `[font] family` when the worker read it.
    pub(crate) family_configured: bool,
}

/// Run `enumerate` on a worker thread and hand its report to `deliver`; returns at once.
///
/// The worker reads `config_path` first, so the report says whether this start's config names a
/// family. A worker that cannot start leaves the catalog loading, and nothing is adopted.
pub(crate) fn spawn<E, D>(config_path: Option<PathBuf>, enumerate: E, deliver: D)
where
    E: FnOnce() -> anyhow::Result<Vec<String>> + Send + 'static,
    D: FnOnce(FontCatalogReport) + Send + 'static,
{
    let job = move || {
        let family_configured = config_path.as_deref().is_some_and(config_sets_family);
        let families = enumerate().unwrap_or_else(|error| {
            tracing::warn!("installed font enumeration failed: {error:#}");
            Vec::new()
        });
        deliver(FontCatalogReport { families, family_configured });
    };
    if let Err(error) = std::thread::Builder::new().name("sonicterm-font-catalog".into()).spawn(job)
    {
        // The catalog then stays loading, and the picker lists only the current family.
        tracing::warn!("font catalog worker did not start: {error}");
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

/// The first of `candidates` that `installed` lists, compared without case and returned in its
/// installed spelling; `None` when none is listed, as for fontconfig's `monospace` alias.
pub(crate) fn first_installed(candidates: &[&str], installed: &[String]) -> Option<String> {
    candidates.iter().find_map(|candidate| {
        installed.iter().find(|family| family.eq_ignore_ascii_case(candidate)).cloned()
    })
}

impl App {
    /// Start the catalog worker once per process, after the main window exists and any isolated
    /// config path is installed. Without an event loop there is nothing to report back to.
    pub(super) fn start_font_catalog(&mut self) {
        if self.font_catalog != FontCatalog::Idle {
            // When: `font_catalog` already started or finished, a later main window adds nothing.
            return;
        }
        let Some(proxy) = self.event_loop_proxy.clone() else {
            // When: `event_loop_proxy` is None, as in unit tests, no worker could report back.
            return;
        };
        self.font_catalog = FontCatalog::Loading;
        let config_path =
            self.runtime_config_path.clone().or_else(sonicterm_cfg::config::Config::default_path);
        // No handle is kept or joined: once the event loop ends, `send_event` returns
        // `EventLoopClosed` on every backend, so a late report is dropped as the worker finishes.
        spawn(config_path, sonicterm_gpu::installed_monospace_families, move |report| {
            let _ = proxy.send_event(UserEvent::FontCatalogReady {
                families: report.families,
                family_configured: report.family_configured,
            });
        });
    }

    /// Record the worker's report and log the session family. On a start whose config names no
    /// family, while the live family is still the loaded platform default, the first installed
    /// platform default becomes the live family and the configured baseline, in memory only.
    pub(super) fn adopt_font_catalog(&mut self, families: Vec<String>, family_configured: bool) {
        self.adopt_font_catalog_from(
            sonicterm_gpu::DEFAULT_MONOSPACE_FAMILIES,
            families,
            family_configured,
        );
    }

    /// [`Self::adopt_font_catalog`] against explicit platform default `candidates`.
    pub(super) fn adopt_font_catalog_from(
        &mut self,
        candidates: &[&str],
        families: Vec<String>,
        family_configured: bool,
    ) {
        // Only the platform default may be replaced: a family set in a config the worker never
        // read, such as one built in memory, is as deliberate as one in the file.
        let on_default =
            candidates.first().is_some_and(|default| *default == self.config.font.family);
        let untouched = !family_configured
            && on_default
            && self.config.font.family == self.configured_font_family;
        let adopted = untouched.then(|| first_installed(candidates, &families)).flatten();
        if let Some(family) = adopted.filter(|family| *family != self.config.font.family) {
            // An installed default other than the live family becomes the session's family.
            self.configured_font_family = family.clone();
            self.set_font_family(&family);
        }
        tracing::info!(
            family = %self.config.font.family,
            monospace_families = families.len(),
            "font catalog ready"
        );
        self.font_catalog = FontCatalog::Ready(families);
        // A picker opened while the catalog loaded now lists it.
        self.refresh_font_family_picker();
    }
}

#[cfg(test)]
#[path = "font_catalog_tests.rs"]
mod font_catalog_tests;
