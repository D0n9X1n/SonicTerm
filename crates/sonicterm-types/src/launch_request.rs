//! Typed launch requests shared by the platform binaries and the app.
//!
//! A launcher asks SonicTerm for one of two things: a terminal (a shell, or a
//! program run directly with no shell), or a script draft from a file
//! association. The two never mix: a draft is typed at a prompt and waits for
//! Enter, while a command launch executes the program the launcher named.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

use crate::OpenScriptRequest;

/// A program a launcher asked SonicTerm to run directly, with no shell.
///
/// Every value stays an [`OsString`], so a path or argument that is not valid
/// Unicode reaches the child byte-for-byte (Unix) or code-unit-for-code-unit
/// (Windows).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommandLaunch {
    /// The program as the launcher spelled it; resolved at spawn time.
    pub program: OsString,
    /// The program's arguments, excluding the program itself.
    pub args: Vec<OsString>,
}

impl CommandLaunch {
    /// The program's file name, made printable, for tab labels, logs and messages.
    ///
    /// Only the last path component is kept, so a directory a launcher passed
    /// never reaches a log line.
    #[must_use]
    pub fn program_file_name(&self) -> String {
        printable_file_name(&self.program)
    }
}

/// The last component of `program`, with control characters replaced, or `program` when it has none.
#[must_use]
pub fn printable_file_name(program: &OsStr) -> String {
    let name = Path::new(program).file_name().unwrap_or(program);
    let printable = replace_control_characters(&name.to_string_lossy());
    if printable.is_empty() {
        // When: `printable` is empty (an empty or separator-only program), keep a stable label.
        return "program".to_string();
    }
    printable
}

/// `text` with every control character replaced by U+FFFD, so it cannot drive a terminal or a log.
#[must_use]
pub fn replace_control_characters(text: &str) -> String {
    text.chars().map(printable_char).collect()
}

/// The character that stands in for `character`: U+FFFD for a control character, else `character` itself.
fn printable_char(character: char) -> char {
    if character.is_control() {
        // When: `character` is a control character, it could move a cursor or forge a log line.
        return '\u{FFFD}';
    }
    character
}

/// Where a launch's first working directory came from, for logging.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CwdSource {
    /// An explicit `--dir=`/`--working-directory=` option.
    Dir,
    /// The process's own current directory.
    Process,
    /// The user's home directory.
    Home,
}

impl CwdSource {
    /// The stable log value for this source.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Dir => "--dir",
            Self::Process => "process",
            Self::Home => "home",
        }
    }
}

/// A terminal launch: a shell, or a command run directly, plus its options.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct TerminalLaunch {
    /// A program to run directly instead of the shell.
    pub command: Option<CommandLaunch>,
    /// The first pane's working directory; `None` keeps the platform default.
    pub cwd: Option<PathBuf>,
    /// Where `cwd` came from, for logging.
    pub cwd_source: Option<CwdSource>,
    /// Native window title and first tab label.
    pub title: Option<String>,
    /// Keep the pane after its process exits, whatever the status.
    pub hold: bool,
}

impl TerminalLaunch {
    /// The first tab's label: the title, else the program's file name, else `shell`.
    #[must_use]
    pub fn tab_title(&self) -> String {
        if let Some(title) = self.title.as_ref() {
            // When: the launcher supplied a title, it names the tab as well as the window.
            return title.clone();
        }
        self.command.as_ref().map_or_else(|| "shell".to_string(), CommandLaunch::program_file_name)
    }
}

/// One request a platform binary queues for the app before or after its window exists.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LaunchRequest {
    /// Open a terminal tab, running a shell or a command.
    Terminal(TerminalLaunch),
    /// Open a script as a draft at the shell prompt; never executed.
    Script(OpenScriptRequest),
}

/// How a pane's child process ended.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PaneExitStatus {
    /// The process exited with this code.
    Code(u32),
    /// The process was killed by this signal (its name, for example `SIGKILL`).
    Signal(String),
    /// The status could not be read.
    Unknown,
}

impl PaneExitStatus {
    /// Whether the process exited with code zero.
    #[must_use]
    pub fn is_clean(&self) -> bool {
        matches!(self, Self::Code(0))
    }

    /// The local status line written into a held pane's grid.
    #[must_use]
    pub fn status_line(&self) -> String {
        match self {
            Self::Code(code) => format!("[process exited with code {code}]"),
            Self::Signal(name) => {
                let printable: String =
                    name.chars().filter(|character| !character.is_control()).collect();
                format!("[process killed by signal {printable}]")
            }
            Self::Unknown => "[process exit status unknown]".to_string(),
        }
    }
}

#[cfg(test)]
#[path = "launch_request_tests.rs"]
mod launch_request_tests;
