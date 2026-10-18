//! The frozen launcher command line shared by the Linux and Windows binaries.
//!
//! ```text
//! sonicterm [TERMINAL-OPTION...] [(-e | --command | --) PROGRAM [ARG...]]
//! sonicterm --open-script PATH
//! sonicterm --runtime-smoke
//! sonicterm --help
//! ```
//!
//! Terminal options are read only before the first `-e`, `--command` or `--`;
//! every later token belongs to the program, verbatim. Values stay
//! [`OsString`]/[`PathBuf`] end to end, so nothing is lost to UTF-8 decoding.
//! Usage errors carry the argument position and the broken rule, never the
//! argument's value.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

use sonicterm_types::{CommandLaunch, CwdSource, TerminalLaunch};

/// The exit status for a command line that breaks the grammar.
pub const USAGE_EXIT_CODE: u8 = 2;

/// Which binary is parsing: the grammar is shared, the private options are not.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CliDialect {
    /// `sonicterm` on Linux.
    Linux,
    /// `sonicterm-windows.exe`, which adds `--refresh-shell-associations` and `--tear-out-payload`.
    Windows,
}

/// Terminal options and an optional command, as parsed.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct TerminalCli {
    /// `-T`/`--title=`.
    pub title: Option<String>,
    /// `--dir=`/`--working-directory=`, already absolute and checked to be a directory.
    pub dir: Option<PathBuf>,
    /// `--hold`.
    pub hold: bool,
    /// The program after `-e`, `--command` or `--`.
    pub command: Option<CommandLaunch>,
}

/// What a parsed command line asks for.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LaunchCli {
    /// Open a terminal window running a shell or a command.
    Terminal(TerminalCli),
    /// Open a script as a draft; the path is as given.
    OpenScript(PathBuf),
    /// The release smoke mode.
    RuntimeSmoke,
    /// Print usage and exit 0.
    Help,
    /// Windows only: refresh shell associations without a window.
    RefreshShellAssociations,
    /// Windows only, private: open a torn-out tab from this JSON payload.
    TearOut(OsString),
}

/// The grammar rule an argument broke.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UsageRule {
    /// A positional argument before the delimiter.
    Positional,
    /// An option that takes a value was last.
    MissingValue,
    /// `--title`, `--dir` or `--working-directory` without `=VALUE`.
    JoinedValueRequired,
    /// A public option given twice.
    Repeated,
    /// Two launch modes, or a mode with terminal options.
    Conflict,
    /// `--dir=` with an empty value.
    EmptyDir,
    /// `--dir` names no existing directory, or a relative one with no process cwd.
    DirNotDirectory,
    /// A title that is not valid Unicode.
    NonUnicodeTitle,
    /// An option this binary does not know.
    UnknownOption,
    /// A delimiter with no program after it.
    MissingProgram,
    /// A delimiter followed by an empty program.
    EmptyProgram,
}

impl UsageRule {
    /// Stable kebab-case name for logs.
    #[must_use]
    pub fn code(self) -> &'static str {
        match self {
            Self::Positional => "positional-argument",
            Self::MissingValue => "missing-value",
            Self::JoinedValueRequired => "joined-value-required",
            Self::Repeated => "repeated-option",
            Self::Conflict => "conflicting-modes",
            Self::EmptyDir => "empty-dir",
            Self::DirNotDirectory => "dir-not-a-directory",
            Self::NonUnicodeTitle => "non-unicode-title",
            Self::UnknownOption => "unknown-option",
            Self::MissingProgram => "missing-program",
            Self::EmptyProgram => "empty-program",
        }
    }

    /// Human-readable explanation, free of argument values.
    #[must_use]
    pub fn describe(self) -> &'static str {
        match self {
            Self::Positional => "unexpected argument; put a program after -e, --command or --",
            Self::MissingValue => "this option needs a value",
            Self::JoinedValueRequired => "write this option as --option=VALUE",
            Self::Repeated => "this option may be given only once",
            Self::Conflict => "this option cannot be combined with the others",
            Self::EmptyDir => "the working directory is empty",
            Self::DirNotDirectory => "the working directory does not exist or is not a directory",
            Self::NonUnicodeTitle => "the title is not valid Unicode",
            Self::UnknownOption => "unknown option",
            Self::MissingProgram => "a program must follow -e, --command or --",
            Self::EmptyProgram => "the program name is empty",
        }
    }
}

/// A command line that breaks the grammar: the 1-based argument position and the rule.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct UsageError {
    /// 1-based index into the arguments after the binary name.
    pub position: usize,
    /// The broken rule.
    pub rule: UsageRule,
}

impl std::fmt::Display for UsageError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "argument {}: {}", self.position, self.rule.describe())
    }
}

impl std::error::Error for UsageError {}

impl UsageError {
    /// Log this error at `warn` with the position and rule only.
    pub fn log(&self) {
        tracing::warn!(
            target: "sonicterm_app::launch",
            position = self.position,
            rule = self.rule.code(),
            "launch arguments rejected"
        );
    }
}

/// The usage text for `dialect`.
#[must_use]
pub fn usage(dialect: CliDialect) -> &'static str {
    match dialect {
        CliDialect::Linux => LINUX_USAGE,
        CliDialect::Windows => WINDOWS_USAGE,
    }
}

const LINUX_USAGE: &str = "\
usage: sonicterm [OPTION...] [(-e | --command | --) PROGRAM [ARG...]]
       sonicterm --open-script PATH
       sonicterm --help

Options (only before -e, --command or --):
  -T TITLE, --title=TITLE      window title and first tab label
  --dir=PATH, --working-directory=PATH
                               first tab's working directory
  --hold                       keep the first pane open after its process exits

-e, --command and -- take one program name and pass every later argument to it
unchanged, without a shell: -e \"cmd args\" names a program called \"cmd args\".
--open-script PATH types a command that runs PATH at the prompt; it does not run it.
";

const WINDOWS_USAGE: &str = "\
usage: sonicterm-windows.exe [OPTION...] [(-e | --command | --) PROGRAM [ARG...]]
       sonicterm-windows.exe --open-script PATH
       sonicterm-windows.exe --refresh-shell-associations
       sonicterm-windows.exe --help

Options (only before -e, --command or --):
  -T TITLE, --title=TITLE      window title and first tab label
  --dir=PATH, --working-directory=PATH
                               first tab's working directory
  --hold                       keep the first pane open after its process exits

-e, --command and -- take one program name and pass every later argument to it
unchanged, without a shell: -e \"cmd args\" names a program called \"cmd args\".
Batch files (.bat, .cmd) are refused; run them with -e cmd /c SCRIPT.
--open-script PATH types a command that runs PATH at the prompt; it does not run it.
";

/// Positions at which each option was seen, so conflicts and repeats name the later one.
#[derive(Default)]
struct Seen {
    title: Option<usize>,
    dir: Option<(usize, OsString)>,
    hold: Option<usize>,
    open_script: Option<(usize, OsString)>,
    runtime_smoke: Option<usize>,
    refresh: Option<usize>,
    tear_out: Option<(usize, OsString)>,
    delimiter: Option<usize>,
    token_count: usize,
}

/// Parse `args` (including the binary name first) under `dialect`.
///
/// `process_cwd` resolves a relative `--dir`; `None` makes a relative one an error.
pub fn parse_launch_args<I, S>(
    args: I,
    dialect: CliDialect,
    process_cwd: Option<&Path>,
) -> Result<LaunchCli, UsageError>
where
    I: IntoIterator<Item = S>,
    S: Into<OsString>,
{
    let mut tokens =
        args.into_iter().map(Into::into).skip(1).enumerate().map(|(index, arg)| (index + 1, arg));
    let mut seen = Seen::default();
    let mut title = None;
    let mut command = None;
    while let Some((position, arg)) = tokens.next() {
        seen.token_count += 1;
        if is_delimiter(&arg) {
            // When: `is_delimiter` matches, every later token is the program and its arguments.
            seen.delimiter = Some(position);
            command = Some(take_command(position, &mut tokens)?);
            break;
        }
        if arg == "--help" {
            // When: `arg` is `--help` before any error, usage wins over every other option.
            return Ok(LaunchCli::Help);
        }
        if arg == "-T" {
            // When: `arg` is `-T`, take the next token verbatim, even one starting with `-`.
            let (value_position, value) =
                tokens.next().ok_or(UsageError { position, rule: UsageRule::MissingValue })?;
            seen.token_count += 1;
            title = Some(record_title(&mut seen, position, value_position, value)?);
            continue;
        }
        if let Some(value) = strip_option_prefix(&arg, "--title=") {
            // When: `--title=` joins its value, which must be Unicode.
            title = Some(record_title(&mut seen, position, position, value)?);
            continue;
        }
        if let Some(value) = strip_option_prefix(&arg, "--dir=")
            .or_else(|| strip_option_prefix(&arg, "--working-directory="))
        {
            // When: `--dir=` or `--working-directory=` joins its value; both spellings share one slot.
            if seen.dir.is_some() {
                // When: `seen.dir` already holds a directory, the second spelling repeats it.
                return Err(UsageError { position, rule: UsageRule::Repeated });
            }
            if value.is_empty() {
                // When: `value` is empty, there is no directory to start in.
                return Err(UsageError { position, rule: UsageRule::EmptyDir });
            }
            seen.dir = Some((position, value));
            continue;
        }
        if arg == "--title" || arg == "--dir" || arg == "--working-directory" {
            // When: `arg` is a joined-value option written bare; the next token must not become its value.
            return Err(UsageError { position, rule: UsageRule::JoinedValueRequired });
        }
        if arg == "--hold" {
            // When: `arg` is the `--hold` flag; a second one is a repeat.
            record_flag(&mut seen.hold, position)?;
            continue;
        }
        if arg == "--open-script" {
            // When: `arg` is `--open-script`, take the next token verbatim as the script path.
            let (_, value) =
                tokens.next().ok_or(UsageError { position, rule: UsageRule::MissingValue })?;
            seen.token_count += 1;
            if seen.open_script.is_some() {
                // When: `seen.open_script` is set, only one script may open per launch.
                return Err(UsageError { position, rule: UsageRule::Repeated });
            }
            seen.open_script = Some((position, value));
            continue;
        }
        if arg == "--runtime-smoke" {
            // When: `arg` is `--runtime-smoke`, select the smoke mode; exclusivity is checked after parsing.
            record_flag(&mut seen.runtime_smoke, position)?;
            continue;
        }
        if dialect == CliDialect::Windows && arg == "--refresh-shell-associations" {
            // When: Windows `--refresh-shell-associations` runs without a window; exclusivity is checked after.
            record_flag(&mut seen.refresh, position)?;
            continue;
        }
        if dialect == CliDialect::Windows && arg == "--tear-out-payload" {
            // When: Windows `--tear-out-payload` is private and keeps its last value when repeated.
            let (_, value) =
                tokens.next().ok_or(UsageError { position, rule: UsageRule::MissingValue })?;
            seen.token_count += 1;
            seen.tear_out = Some((position, value));
            continue;
        }
        if is_option_like(&arg) {
            // When: `is_option_like` matches an option this binary does not know.
            return Err(UsageError { position, rule: UsageRule::UnknownOption });
        }
        return Err(UsageError { position, rule: UsageRule::Positional });
    }
    resolve_mode(seen, title, command, process_cwd)
}

/// Record a flag at `position`, refusing a repeat.
fn record_flag(slot: &mut Option<usize>, position: usize) -> Result<(), UsageError> {
    if slot.is_some() {
        // When: `slot` is already set, this flag was given earlier.
        return Err(UsageError { position, rule: UsageRule::Repeated });
    }
    *slot = Some(position);
    Ok(())
}

/// Record a title from `-T` or `--title=`, refusing a repeat or a non-Unicode value.
fn record_title(
    seen: &mut Seen,
    position: usize,
    value_position: usize,
    value: OsString,
) -> Result<String, UsageError> {
    record_flag(&mut seen.title, position)?;
    value
        .into_string()
        .map_err(|_| UsageError { position: value_position, rule: UsageRule::NonUnicodeTitle })
}

/// Take the program and its arguments after the delimiter at `position`.
fn take_command(
    position: usize,
    tokens: &mut impl Iterator<Item = (usize, OsString)>,
) -> Result<CommandLaunch, UsageError> {
    let (program_position, program) =
        tokens.next().ok_or(UsageError { position, rule: UsageRule::MissingProgram })?;
    if program.is_empty() {
        // When: `program` is empty, there is nothing to resolve; a shell is never substituted.
        return Err(UsageError { position: program_position, rule: UsageRule::EmptyProgram });
    }
    let args = tokens.map(|(_, arg)| arg).collect();
    Ok(CommandLaunch { program, args })
}

/// Check modes for conflicts and resolve `--dir`.
fn resolve_mode(
    seen: Seen,
    title: Option<String>,
    command: Option<CommandLaunch>,
    process_cwd: Option<&Path>,
) -> Result<LaunchCli, UsageError> {
    let terminal_positions =
        [seen.title, seen.dir.as_ref().map(|(position, _)| *position), seen.hold, seen.delimiter];
    let first_terminal = terminal_positions.iter().flatten().min().copied();
    for exclusive in [seen.runtime_smoke, seen.refresh].into_iter().flatten() {
        if seen.token_count > 1 {
            // When: `token_count` shows an exclusive flag sharing the command line, report another token.
            let position = if exclusive == 1 { 2 } else { 1 };
            return Err(UsageError { position, rule: UsageRule::Conflict });
        }
    }
    let private_modes = [
        seen.open_script.as_ref().map(|(position, _)| *position),
        seen.tear_out.as_ref().map(|(position, _)| *position),
    ];
    let present: Vec<usize> =
        private_modes.iter().flatten().copied().chain(first_terminal).collect();
    if present.len() > 1 {
        // When: two of script, tear-out and terminal options are present, report the later one.
        let position = present.iter().copied().max().unwrap_or(1);
        return Err(UsageError { position, rule: UsageRule::Conflict });
    }
    if seen.runtime_smoke.is_some() {
        // When: `runtime_smoke` stood alone, run the release smoke mode.
        return Ok(LaunchCli::RuntimeSmoke);
    }
    if seen.refresh.is_some() {
        // When: `refresh` stood alone, refresh associations without a window.
        return Ok(LaunchCli::RefreshShellAssociations);
    }
    if let Some((_, path)) = seen.open_script {
        // When: `open_script` stood alone, open the script as a draft.
        return Ok(LaunchCli::OpenScript(PathBuf::from(path)));
    }
    if let Some((_, payload)) = seen.tear_out {
        // When: `tear_out` stood alone, the binary decodes the payload.
        return Ok(LaunchCli::TearOut(payload));
    }
    let dir =
        seen.dir.map(|(position, value)| resolve_dir(position, &value, process_cwd)).transpose()?;
    Ok(LaunchCli::Terminal(TerminalCli { title, dir, hold: seen.hold.is_some(), command }))
}

/// Resolve `--dir` against `process_cwd` and require an existing directory.
fn resolve_dir(
    position: usize,
    value: &OsStr,
    process_cwd: Option<&Path>,
) -> Result<PathBuf, UsageError> {
    let given = Path::new(value);
    let resolved = if given.is_absolute() {
        given.to_path_buf()
    } else {
        // When: `given` is relative, it resolves against the process cwd, which must be known.
        process_cwd
            .filter(|cwd| cwd.is_absolute())
            .ok_or(UsageError { position, rule: UsageRule::DirNotDirectory })?
            .join(given)
    };
    if !resolved.is_dir() {
        // When: `resolved` is missing or a file, starting there would fail later; refuse now.
        return Err(UsageError { position, rule: UsageRule::DirNotDirectory });
    }
    Ok(resolved)
}

/// Whether `arg` is `-e`, `--command` or `--`.
fn is_delimiter(arg: &OsStr) -> bool {
    arg == "-e" || arg == "--command" || arg == "--"
}

/// Whether `arg` looks like an option rather than a positional value.
fn is_option_like(arg: &OsStr) -> bool {
    let lossy = arg.to_string_lossy();
    lossy.starts_with('-') && lossy.len() > 1
}

/// The rest of `arg` after the ASCII `prefix`, without decoding the rest.
fn strip_option_prefix(arg: &OsStr, prefix: &str) -> Option<OsString> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        arg.as_bytes()
            .strip_prefix(prefix.as_bytes())
            .map(|rest| OsStr::from_bytes(rest).to_os_string())
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::{OsStrExt, OsStringExt};
        let wide: Vec<u16> = arg.encode_wide().collect();
        let wide_prefix: Vec<u16> = prefix.encode_utf16().collect();
        wide.strip_prefix(wide_prefix.as_slice()).map(OsString::from_wide)
    }
    #[cfg(not(any(unix, windows)))]
    {
        arg.to_str().and_then(|text| text.strip_prefix(prefix)).map(OsString::from)
    }
}

/// Turn parsed terminal options into a launch, choosing the first pane's cwd.
///
/// Linux starts every launch without `--dir` in the process cwd when it is a
/// directory, else `home`. Windows does that for a command launch only; a
/// plain shell keeps its `%USERPROFILE%` default, because the installer's
/// shortcuts start the process in the install folder.
#[must_use]
pub fn terminal_launch(
    cli: TerminalCli,
    dialect: CliDialect,
    process_cwd: Option<PathBuf>,
    home: Option<PathBuf>,
) -> TerminalLaunch {
    let (cwd, cwd_source) = match cli.dir {
        Some(dir) => (Some(dir), Some(CwdSource::Dir)),
        None if dialect == CliDialect::Linux || cli.command.is_some() => {
            default_cwd(process_cwd, home)
        }
        None => (None, None),
    };
    TerminalLaunch { command: cli.command, cwd, cwd_source, title: cli.title, hold: cli.hold }
}

/// The process cwd when it is an absolute directory, else `home`.
fn default_cwd(
    process_cwd: Option<PathBuf>,
    home: Option<PathBuf>,
) -> (Option<PathBuf>, Option<CwdSource>) {
    if let Some(cwd) = process_cwd.filter(|cwd| cwd.is_absolute() && cwd.is_dir()) {
        // When: the `process_cwd` is a usable directory, the launcher meant to start there.
        return (Some(cwd), Some(CwdSource::Process));
    }
    match home.filter(|home| home.is_dir()) {
        Some(home) => (Some(home), Some(CwdSource::Home)),
        None => (None, None),
    }
}

/// The user's home directory as this platform names it.
#[must_use]
pub fn home_dir() -> Option<PathBuf> {
    #[cfg(windows)]
    let home = std::env::var_os("USERPROFILE");
    #[cfg(not(windows))]
    let home = std::env::var_os("HOME");
    home.filter(|value| !value.is_empty()).map(PathBuf::from)
}

#[cfg(test)]
#[path = "launch_cli_tests.rs"]
mod launch_cli_tests;
