//! The report windows the App can open beside its terminals: Memory Usage and About.

/// The pending open and shown text of each report window.
#[derive(Default)]
pub(in crate::app) struct ReportWindows {
    /// The Memory Usage report's pending open, refresh generation and last rendered text.
    pub(in crate::app) memory_report: super::memory_report_window::MemoryReportState,
    /// The About window's pending open, shown text and last config load error.
    pub(in crate::app) about: super::about_window::AboutState,
}
