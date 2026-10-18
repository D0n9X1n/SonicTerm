use std::path::PathBuf;

use sonicterm_cfg::url_scan::PathStyle;
use sonicterm_io::pty::ShellSpawnOpts;
use sonicterm_vt::vt::Osc7Cwd;

use super::WindowState;
use sonicterm_types::{
    classify_shell, format_script_draft, CommandLaunch, CwdSource, DraftRejection, LaunchOrigin,
    OpenScriptRequest, TerminalLaunch,
};

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(super) struct PaneLaunch {
    pub(super) cwd: Option<PathBuf>,
    pub(super) script: Option<OpenScriptRequest>,
    /// A program to run directly instead of the shell; never combined with `script`.
    pub(super) command: Option<CommandLaunch>,
    /// Where `cwd` came from, for the launch log line.
    pub(super) cwd_source: Option<CwdSource>,
    /// Keep the pane after its process exits, whatever the status.
    pub(super) hold: bool,
    /// The launcher's title for the native window and first tab.
    pub(super) title: Option<String>,
    /// What asked for the launch: a launcher, a document open or a link.
    pub(super) origin: LaunchOrigin,
}

impl PaneLaunch {
    /// Fill only an ordinary shell launch from an accepted local OSC 7 directory.
    pub(super) fn inherit_cwd(
        mut self,
        cwd: Option<&Osc7Cwd>,
        style: PathStyle,
        local_hostname: &str,
    ) -> Self {
        if self.cwd.is_none() && self.script.is_none() && self.command.is_none() {
            // No explicit cwd, script or command owns the launch, so the source pane may supply a local directory.
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
        Self { cwd, script: Some(request), ..Self::default() }
    }

    /// The first pane of a launcher's terminal request.
    pub(super) fn for_terminal(launch: TerminalLaunch) -> Self {
        Self {
            cwd: launch.cwd,
            script: None,
            command: launch.command,
            cwd_source: launch.cwd_source,
            hold: launch.hold,
            title: launch.title,
            origin: launch.origin,
        }
    }

    /// The launch kind logged at spawn: `executable`, `url`, `command`, `script` or `shell`.
    pub(super) fn kind(&self) -> &'static str {
        if self.command.is_some() {
            // When: `command` is set, the program runs directly; `origin` says who asked for it.
            return match self.origin {
                LaunchOrigin::Launcher => "command",
                LaunchOrigin::Executable { .. } => "executable",
                LaunchOrigin::Url { .. } => "url",
            };
        }
        if self.script.is_some() {
            // When: `script` is set, a shell starts with the script typed as a draft.
            return "script";
        }
        "shell"
    }

    /// The link scheme for the launch log, when a link asked for this launch.
    pub(super) fn scheme(&self) -> Option<&'static str> {
        match self.origin {
            LaunchOrigin::Url { scheme } => Some(scheme),
            LaunchOrigin::Launcher | LaunchOrigin::Executable { .. } => None,
        }
    }

    /// Confirm, just before spawn, that a document-opened executable is still the classified file.
    ///
    /// macOS has no `fexecve`, so the path is checked again here: it must still
    /// name a regular file (not a symlink) with the classified device and inode.
    /// A narrow window between this check and `execve` remains. Other origins pass.
    pub(super) fn executable_still_matches(&self) -> bool {
        let LaunchOrigin::Executable { identity } = self.origin else {
            // When: `origin` is not a document-opened executable, there is no identity to confirm.
            return true;
        };
        let Some(command) = self.command.as_ref() else {
            // When: an executable origin carries no `command`, nothing would run; refuse it.
            return false;
        };
        file_identity_matches(std::path::Path::new(&command.program), identity)
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

/// Whether `path` is still a regular file, not a symlink, with `expected`'s device and inode.
#[cfg(unix)]
pub(super) fn file_identity_matches(
    path: &std::path::Path,
    expected: sonicterm_types::FileIdentity,
) -> bool {
    use std::os::unix::fs::MetadataExt;
    std::fs::symlink_metadata(path).is_ok_and(|metadata| {
        metadata.file_type().is_file()
            && metadata.dev() == expected.device
            && metadata.ino() == expected.inode
    })
}

/// No device/inode identity exists to confirm off Unix, so a verified-executable launch fails closed.
#[cfg(not(unix))]
pub(super) fn file_identity_matches(
    _path: &std::path::Path,
    _expected: sonicterm_types::FileIdentity,
) -> bool {
    false
}

#[cfg(test)]
#[path = "pane_launch_tests.rs"]
mod pane_launch_tests;
