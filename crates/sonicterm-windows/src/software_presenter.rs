//! How `software_render_mode` resolves on Windows.
//!
//! Only the configuration decision lives here. The software presentation path
//! itself is `sonicterm_gpu::software_windows::WindowsSoftwareFrame`, which is
//! what a degraded renderer actually constructs and presents through GDI.
//!
//! A second, unused presenter used to sit in this file — a retained BGRA
//! surface with dirty-rectangle support that nothing constructed. It duplicated
//! the live path down to the 160 MiB clamp, and having two made it easy to
//! verify the wrong one.

use sonicterm_cfg::config::SoftwareRenderMode;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowsSoftwarePresenterPreference {
    /// Prefer the normal wgpu path unless adapter detection proves it is WARP.
    Auto,
    /// Use the Win32 retained-BGRA presenter immediately.
    Force,
    /// Never use the Win32 retained-BGRA presenter.
    Off,
}

impl WindowsSoftwarePresenterPreference {
    #[must_use]
    pub fn from_config(mode: SoftwareRenderMode) -> Self {
        match mode {
            SoftwareRenderMode::Auto => Self::Auto,
            SoftwareRenderMode::Force => Self::Force,
            SoftwareRenderMode::Off => Self::Off,
        }
    }

    /// Whether the software presenter applies, given runtime adapter detection.
    ///
    /// **No production caller.** The only one passed a hardcoded `false`, so
    /// under `Auto` — the default — it always answered "no" whatever the host
    /// had; it gated a log line and was removed rather than corrected, because
    /// `app/event_loop.rs` already logs `software-render degrade engaged` with
    /// both `detected` and `mode` at the moment those are real.
    ///
    /// A real caller cannot exist here yet for a structural reason:
    /// `detected_software_adapter` comes from the renderer, and the renderer is
    /// built inside `WindowsShell` — after this crate's startup code runs. The
    /// caller will arrive with [`SoftwareSurface`], which is also not yet
    /// constructed outside tests.
    ///
    /// Kept because the decision it encodes is verified against the app's copy
    /// (`should_degrade_for_software_render`) across the whole `(mode,
    /// detected)` domain, and that agreement is what stops a half-degraded
    /// renderer when the presenter is wired up.
    ///
    /// `dead_code` is allowed rather than silenced by deletion: removing the
    /// method would take the cross-layer agreement check with it, and that
    /// check is the only thing holding the two copies of this decision
    /// together until a production caller exists.
    #[allow(dead_code)]
    #[must_use]
    pub fn should_use(self, detected_software_adapter: bool) -> bool {
        match self {
            Self::Auto => detected_software_adapter,
            Self::Force => true,
            Self::Off => false,
        }
    }

    #[must_use]
    pub fn forces_opaque_window(self) -> bool {
        matches!(self, Self::Force)
    }
}

#[cfg(test)]
#[path = "software_presenter_tests.rs"]
mod software_presenter_tests;
