use std::path::PathBuf;

use sonicterm_cfg::url_scan::PathStyle;
use sonicterm_io::pty::ShellSpawnOpts;
use sonicterm_vt::vt::Osc7Cwd;

use super::WindowState;
use sonicterm_types::{classify_shell, format_script_draft, DraftRejection, OpenScriptRequest};

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(super) struct PaneLaunch {
    pub(super) cwd: Option<PathBuf>,
    pub(super) script: Option<OpenScriptRequest>,
}

impl PaneLaunch {
    /// Fill only an ordinary shell launch from an accepted local OSC 7 directory.
    pub(super) fn inherit_cwd(
        mut self,
        cwd: Option<&Osc7Cwd>,
        style: PathStyle,
        local_hostname: &str,
    ) -> Self {
        if self.cwd.is_none() && self.script.is_none() {
            // No explicit cwd or script owns the launch, so the source pane may supply a local directory.
            self.cwd = cwd
                .and_then(|cwd| super::path_target::local_launch_cwd(cwd, style, local_hostname));
        }
        self
    }

    /// Snapshot only this window's active live pane, without waiting on its parser worker.
    pub(super) fn from_window(window: Option<&WindowState>, local_hostname: &str) -> Self {
        let cwd = window.and_then(|window| {
            let tab = window.tab_states.get(window.tabs.active_index())?;
            if !tab.tree.leaves().contains(&tab.active_pane) {
                // When: active_pane is not a live tab leaf, stale focus cannot supply a launch directory.
                return None;
            }
            let pane = window.panes.get(&tab.active_pane)?;
            let parser = pane.parser.try_lock()?;
            let cwd = parser.osc7_cwd()?;
            // Bound cloning while holding the parser, not just the later native-path normalization.
            if cwd.path.len() > 4096 || cwd.authority.len() > 4096 {
                // When: cwd.path or cwd.authority exceeds launch bounds, keep the shell's default rather than copying it.
                return None;
            }
            Some(cwd.clone())
        });
        Self::default().inherit_cwd(cwd.as_ref(), PathStyle::native(), local_hostname)
    }

    pub(super) fn for_script(request: OpenScriptRequest) -> Self {
        let cwd = request.pane_cwd().map(PathBuf::from);
        Self { cwd, script: Some(request) }
    }

    pub(super) fn shell_spawn_opts(
        &self,
        term_program: String,
        shell: Option<String>,
    ) -> ShellSpawnOpts {
        ShellSpawnOpts { term_program, shell, cwd: self.cwd.clone(), ..ShellSpawnOpts::default() }
    }

    pub(super) fn draft_for_shell(
        &self,
        shell_program_path: &str,
    ) -> Result<Option<String>, DraftRejection> {
        self.script
            .as_ref()
            .map(|request| {
                format_script_draft(classify_shell(shell_program_path), &request.launch_path)
            })
            .transpose()
    }

    pub(super) fn draft_rejection_message(&self, rejection: DraftRejection) -> String {
        let path = self
            .script
            .as_ref()
            .map(|request| request.original_path.display().to_string())
            .unwrap_or_else(|| "the requested script".to_string());
        let reason = match rejection {
            DraftRejection::NonUnicodePath => "the path is not valid Unicode",
            DraftRejection::NonAbsolutePath => "the launch path is not absolute",
            DraftRejection::ControlCharacter => "the path contains a control character",
            DraftRejection::UnsupportedPair => {
                "the active shell cannot safely run this script type"
            }
            DraftRejection::CmdUnsafeCharacter => {
                "the path contains characters expanded by Command Prompt"
            }
        };
        format!("Script command was not prefilled for {path}: {reason}")
    }
}

#[cfg(test)]
#[path = "pane_launch_tests.rs"]
mod pane_launch_tests;
