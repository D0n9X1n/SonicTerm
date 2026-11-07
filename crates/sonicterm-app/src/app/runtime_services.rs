//! The App's long-lived background services: the foreground-process probe and its schedule, the
//! MCP control server, the PTY reaper, GPU recovery, the font catalog worker and the runtime
//! smoke. Each runs or waits off the event loop; the App only starts, polls and stops them.

use super::*;

/// Background services the App starts, polls and stops: probes, MCP, the reaper, GPU recovery.
pub(in crate::app) struct RuntimeServices {
    /// Foreground-process probes: demand, the worker and its latest-value result map.
    pub(in crate::app) fg_probes: Arc<fg_probe::ForegroundProbes>,
    /// Activity and warning wakes for the foreground-process schedule; only Windows arms them.
    pub(in crate::app) foreground_schedule: fg_probe::ForegroundSchedule,
    /// The local control server agents connect to, while `[mcp] enabled` is on.
    pub(in crate::app) mcp_server: Option<mcp::McpServer>,
    /// One bounded driver owns every retired native transport after its pane leaves the UI.
    pub(in crate::app) pty_reaper: reaper_driver::ReaperDriver,
    /// One committed GPU context survives window closure and owns every recovery attempt.
    pub(in crate::app) gpu_recovery: Option<gpu_recovery::GpuRecovery>,
    /// The catalog worker, once the picker started it; kept so it is owned, never joined.
    pub(in crate::app) font_catalog_worker: Option<std::thread::JoinHandle<()>>,
    /// Hidden Linux package-smoke state; absent during ordinary application runs.
    pub(in crate::app) runtime_smoke: Option<runtime_smoke::RuntimeSmokeState>,
}
