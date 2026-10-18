//! The Windows branch of the default-handler command: open Settings › Default apps.
//!
//! Windows keeps the user's default-app choice to itself, so SonicTerm changes nothing here.
//! Apply opens the Settings page, preferring the per-app page for the HKLM registration the
//! MSI writes, and falling back to the general Default apps page. The report always lists
//! what to choose. Nothing in this module writes the registry or `UserChoice`.

use std::sync::Arc;

use sonicterm_app::default_handlers::{
    DefaultHandlerBackend, HandlerItem, HandlerKind, HandlerOutcome, HandlerPlan,
    HandlerResultSink, PlanMode,
};

/// The `RegisteredApplications` value name the MSI writes under HKLM.
pub(crate) const REGISTERED_APP_NAME: &str = "SonicTerm";
/// The script extensions the MSI registers SonicTerm for; each opens as a draft.
pub(crate) const SCRIPT_EXTENSIONS: &[&str] = &[".ps1", ".cmd", ".bat", ".sh"];
/// The general Settings › Apps › Default apps page.
pub(crate) const DEFAULT_APPS_URI: &str = "ms-settings:defaultapps";

/// The per-app Default apps page for an HKLM registration. Unverified on older builds.
pub(crate) fn per_app_settings_uri() -> String {
    format!("{DEFAULT_APPS_URI}?registeredAppMachine={REGISTERED_APP_NAME}")
}

/// Opens one `ms-settings:` URI; the error text is shown in the report.
pub(crate) type SettingsLauncher = Arc<dyn Fn(&str) -> Result<(), String> + Send + Sync>;

/// What to choose on the Settings page, shown before and after Apply.
pub(crate) fn choice_instructions() -> Vec<String> {
    vec![
        format!(
            "Choose SonicTerm for {}; scripts open in SonicTerm as drafts and never run.",
            SCRIPT_EXTENSIONS.join(", ")
        ),
        "If the page does not open: Settings › Apps › Default apps › SonicTerm.".to_string(),
    ]
}

/// Open the per-app page, else the general page, and report which opened.
pub(crate) fn open_default_apps(launcher: &SettingsLauncher, sink: &HandlerResultSink) {
    let per_app = per_app_settings_uri();
    let outcome = match launcher(&per_app) {
        Ok(()) => HandlerOutcome::Opened("SonicTerm's Default apps page".to_string()),
        Err(per_app_error) => match launcher(DEFAULT_APPS_URI) {
            Ok(()) => HandlerOutcome::Opened("Default apps".to_string()),
            Err(general_error) => {
                HandlerOutcome::Failed(format!("{per_app_error}; {general_error}"))
            }
        },
    };
    tracing::info!(
        target: "sonicterm_windows::default_handlers",
        outcome = %outcome.label(),
        "default apps settings requested"
    );
    sink.report(0, outcome);
}

/// The Settings backend the Windows palette command uses.
pub(crate) struct WindowsDefaultHandlers {
    launcher: SettingsLauncher,
}

impl WindowsDefaultHandlers {
    /// A backend opening Settings through `launcher`.
    pub(crate) fn new(launcher: SettingsLauncher) -> Self {
        Self { launcher }
    }
}

impl DefaultHandlerBackend for WindowsDefaultHandlers {
    fn platform(&self) -> &'static str {
        "windows"
    }

    fn plan(&mut self) -> Result<HandlerPlan, String> {
        Ok(HandlerPlan {
            mode: PlanMode::OpenSettings,
            items: vec![HandlerItem::new(HandlerKind::SettingsPage, "Default apps")],
            confirm_notes: SCRIPT_EXTENSIONS
                .iter()
                .map(|extension| format!("{extension} → SonicTerm"))
                .collect(),
            report_notes: choice_instructions(),
        })
    }

    fn apply(&mut self, _plan: &HandlerPlan, sink: HandlerResultSink) {
        let launcher = Arc::clone(&self.launcher);
        let worker_sink = sink.clone();
        let spawned = std::thread::Builder::new()
            .name("sonicterm-default-apps".to_string())
            .spawn(move || open_default_apps(&launcher, &worker_sink));
        if spawned.is_err() {
            // No worker exists to report, so the page counts as not opened.
            sink.report(0, HandlerOutcome::Failed("worker did not start".to_string()));
        }
    }
}

/// The production backend: `ShellExecuteExW` on the worker thread, in its own COM apartment.
#[cfg(target_os = "windows")]
pub(crate) fn native() -> WindowsDefaultHandlers {
    WindowsDefaultHandlers::new(Arc::new(shell::open_settings_uri))
}

#[cfg(target_os = "windows")]
mod shell {
    use windows::core::PCWSTR;
    use windows::Win32::System::Com::{
        CoInitializeEx, CoUninitialize, COINIT, COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE,
    };
    use windows::Win32::UI::Shell::{ShellExecuteExW, SEE_MASK_NOASYNC, SHELLEXECUTEINFOW};
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

    /// Shell handlers expect a single-threaded apartment; OLE1 DDE stays off.
    const SETTINGS_COINIT: COINIT = COINIT(COINIT_APARTMENTTHREADED.0 | COINIT_DISABLE_OLE1DDE.0);

    /// Encode `text` as a NUL-terminated UTF-16 buffer.
    fn wide_nul(text: &str) -> Vec<u16> {
        text.encode_utf16().chain(std::iter::once(0)).collect()
    }

    /// Ask the shell to open one `ms-settings:` URI; any other scheme is refused.
    pub(super) fn open_settings_uri(uri: &str) -> Result<(), String> {
        if !uri.starts_with("ms-settings:") {
            // When: `uri` is not an `ms-settings:` link, this launcher has no reason to open it.
            return Err("not a Settings link".to_string());
        }
        let initialized =
            // SAFETY: CoInitializeEx gets a null reserved pointer and a valid COINIT; its result is checked.
            unsafe { CoInitializeEx(None, SETTINGS_COINIT) };
        if let Err(error) = initialized.ok() {
            // When: `initialized` failed, no apartment was entered, so nothing may be uninitialized.
            return Err(format!("COM unavailable ({})", error.code().0));
        }
        let verb = wide_nul("open");
        let target = wide_nul(uri);
        let mut info = SHELLEXECUTEINFOW {
            cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
            fMask: SEE_MASK_NOASYNC,
            lpVerb: PCWSTR(verb.as_ptr()),
            lpFile: PCWSTR(target.as_ptr()),
            nShow: SW_SHOWNORMAL.0,
            ..Default::default()
        };
        let launched =
            // SAFETY: `info` is fully initialized with a matching cbSize; its verb and target buffers outlive the call.
            unsafe { ShellExecuteExW(&mut info) };
        // SAFETY: balances the successful CoInitializeEx above on this same thread.
        unsafe { CoUninitialize() };
        launched.map_err(|error| format!("Settings did not open ({})", error.code().0))
    }
}

#[cfg(test)]
#[path = "default_handlers_tests.rs"]
mod default_handlers_tests;
