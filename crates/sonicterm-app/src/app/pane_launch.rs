use std::path::PathBuf;

use sonicterm_io::pty::ShellSpawnOpts;
use sonicterm_types::{classify_shell, format_script_draft, DraftRejection, OpenScriptRequest};

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(super) struct PaneLaunch {
    pub(super) cwd: Option<PathBuf>,
    pub(super) script: Option<OpenScriptRequest>,
}

impl PaneLaunch {
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
