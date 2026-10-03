//! The command line both crate roots share: `--list` everywhere, `--run` on macOS and Windows,
//! and on Windows the role program a pane's shell runs.

#[cfg(any(target_os = "macos", windows, test))]
use std::ffi::OsStr;
use std::ffi::OsString;
#[cfg(any(target_os = "macos", windows, test))]
use std::path::Path;
use std::path::PathBuf;
use std::process::ExitCode;

use crate::scenarios;

/// Exit code for a refusal before any window opens.
pub(crate) const REFUSED: u8 = 2;

const USAGE: &str =
    "usage: perf_scenarios --list\n       perf_scenarios --run <ID> [--variant <name>] \
[--managed] [--short] [--laps] [--harness-hash <hex>] <scratch>";

/// Run the command line; `allocation_counter` reads the counting allocator when one is installed.
pub(crate) fn run(allocation_counter: Option<fn() -> u64>) -> ExitCode {
    let mut args = Vec::new();
    for arg in std::env::args_os().skip(1) {
        match arg.into_string() {
            Ok(arg) => args.push(arg),
            Err(arg) => {
                // When: an argument is not UTF-8, it cannot name a scenario or a scratch path.
                eprintln!("perf_scenarios: argument {arg:?} is not UTF-8");
                return ExitCode::from(REFUSED);
            }
        }
    }
    // ConPTY starts each pane's configured shell, this binary, with no arguments and the
    // probe's scratch variable inherited; that process is a role program, not the harness.
    #[cfg(windows)]
    if let Some(scratch) = program_scratch(&args, std::env::var_os(crate::workload::SCRATCH_ENV)) {
        return ExitCode::from(crate::workload::run_program(&scratch));
    }
    ExitCode::from(run_code(&args, allocation_counter))
}

/// The scratch directory when this process is a pane's role program: no arguments, and the
/// scratch variable set and non-empty. Any argument means the harness's own command line.
#[cfg_attr(not(any(windows, test)), allow(dead_code))]
fn program_scratch(args: &[String], scratch: Option<OsString>) -> Option<PathBuf> {
    if !args.is_empty() {
        // When: arguments were given, this is `--list`, `--run` or a usage error, never a pane.
        return None;
    }
    scratch.filter(|value| !value.is_empty()).map(PathBuf::from)
}

/// The exit code for `args`: 0 for `--list`, the run's code for `--run`, 2 for anything else.
fn run_code(args: &[String], allocation_counter: Option<fn() -> u64>) -> u8 {
    match args.split_first() {
        Some((flag, [])) if flag == "--list" => {
            println!("{}", scenarios::list_json());
            0
        }
        Some((flag, rest)) if flag == "--run" => run_scenario(rest, allocation_counter),
        _ => {
            eprintln!("{USAGE}");
            REFUSED
        }
    }
}

/// Off macOS and Windows a run exercises nothing and writes nothing.
#[cfg(not(any(target_os = "macos", windows)))]
fn run_scenario(_args: &[String], _allocation_counter: Option<fn() -> u64>) -> u8 {
    println!("NOT_EXERCISED: this opt-in example requires macOS or Windows");
    0
}

/// Validate a run, refusing with exit 2 before any window opens, then measure it.
#[cfg(any(target_os = "macos", windows))]
fn run_scenario(args: &[String], allocation_counter: Option<fn() -> u64>) -> u8 {
    let checked = parse_run(args).and_then(|request| {
        check_environment(
            std::env::var_os("NO_COLOR").as_deref(),
            std::env::var_os("RUST_LOG").as_deref(),
        )?;
        let temp_root = std::env::temp_dir()
            .canonicalize()
            .map_err(|error| format!("cannot resolve the temp directory: {error}"))?;
        check_scratch(&request.scratch, &temp_root)?;
        Ok(request)
    });
    match checked {
        Ok(request) => crate::probe::run(&request, allocation_counter),
        Err(reason) => {
            // When: any check failed, nothing has been created and no window has opened.
            eprintln!("perf_scenarios: refused: {reason}\n{USAGE}");
            REFUSED
        }
    }
}

/// A validated `--run` request.
#[cfg(any(target_os = "macos", windows, test))]
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RunArgs {
    /// Scenario id from the catalog.
    pub(crate) scenario: &'static str,
    /// Variant from the scenario's list.
    pub(crate) variant: &'static str,
    /// The comparison script acknowledges sessions and answers checkpoints.
    pub(crate) managed: bool,
    /// Shorten every hold to 5 s and S3's flood.
    pub(crate) short: bool,
    /// Log at `debug`, which adds the per-frame `render_timing` line.
    pub(crate) laps: bool,
    /// The harness hash to record, in hex.
    pub(crate) harness_hash: Option<String>,
    /// The scratch directory exactly as given on the command line.
    pub(crate) scratch: String,
}

/// Parse the arguments after `--run`: an id first, then flags and one scratch path in any order.
#[cfg(any(target_os = "macos", windows, test))]
fn parse_run(args: &[String]) -> Result<RunArgs, String> {
    let mut rest = args.iter();
    let id = rest.next().ok_or("--run needs a scenario id")?;
    let spec = scenarios::find(id).ok_or_else(|| format!("unknown scenario {id}"))?;
    let mut variant = None;
    let mut harness_hash = None;
    let mut scratch = None;
    let (mut managed, mut short, mut laps) = (false, false, false);
    while let Some(arg) = rest.next() {
        match arg.as_str() {
            "--variant" => {
                let name = rest.next().ok_or("--variant needs a name")?;
                let listed = spec
                    .variants
                    .iter()
                    .find(|listed| **listed == name.as_str())
                    .ok_or_else(|| format!("{} has no variant {name}", spec.id))?;
                set_once(&mut variant, *listed, "--variant")?;
            }
            "--managed" => set_flag(&mut managed, "--managed")?,
            "--short" => set_flag(&mut short, "--short")?,
            "--laps" => set_flag(&mut laps, "--laps")?,
            "--harness-hash" => {
                let hash = rest.next().ok_or("--harness-hash needs a value")?;
                if hash.is_empty() || !hash.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                    // When: the hash is empty or not hex, it cannot name a harness build.
                    return Err(format!("--harness-hash {hash:?} is not hex"));
                }
                set_once(&mut harness_hash, hash.clone(), "--harness-hash")?;
            }
            flag if flag.starts_with("--") => return Err(format!("unknown flag {flag}")),
            path => set_once(&mut scratch, path.to_owned(), "the scratch directory")?,
        }
    }
    Ok(RunArgs {
        scenario: spec.id,
        variant: variant.unwrap_or(spec.variants[0]),
        managed,
        short,
        laps,
        harness_hash,
        scratch: scratch.ok_or("--run needs a scratch directory")?,
    })
}

#[cfg(any(target_os = "macos", windows, test))]
fn set_once<Item>(slot: &mut Option<Item>, value: Item, name: &str) -> Result<(), String> {
    if slot.replace(value).is_some() {
        // When: the option was already given, a second value would silently win.
        return Err(format!("{name} given twice"));
    }
    Ok(())
}

#[cfg(any(target_os = "macos", windows, test))]
fn set_flag(flag: &mut bool, name: &str) -> Result<(), String> {
    if std::mem::replace(flag, true) {
        // When: the flag was already set, the repeat is a typo worth refusing.
        return Err(format!("{name} given twice"));
    }
    Ok(())
}

/// Whether `text` holds a single quote or a control character, either of which ends a
/// single-quoted shell string or a TOML literal string early.
#[cfg(any(target_os = "macos", windows, test))]
fn breaks_quoted_literal(text: &str) -> bool {
    text.chars().any(|character| character == '\'' || character.is_control())
}

/// Refuse a harness path that would break the TOML literal string naming it as every pane's shell.
#[cfg(any(windows, test))]
pub(crate) fn check_harness_shell(shell: &str) -> Result<(), String> {
    if breaks_quoted_literal(shell) {
        // When: the path would end the TOML literal early, the config could name another program.
        return Err(format!("harness path {shell:?} holds a single quote or a control character"));
    }
    Ok(())
}

/// Refuse a scratch path that is not absolute, could break the generated shell script or TOML,
/// is not under `temp_root` (the canonical OS temp directory), or already exists.
#[cfg(any(target_os = "macos", windows, test))]
fn check_scratch(scratch: &str, temp_root: &Path) -> Result<PathBuf, String> {
    let path = PathBuf::from(scratch);
    if !path.is_absolute() {
        return Err(format!("scratch {scratch:?} is not absolute"));
    }
    // The path is pasted into a single-quoted shell string and a TOML literal string; only a
    // single quote or a control character ends either early. A Windows temp path has backslashes.
    if breaks_quoted_literal(scratch) {
        return Err(format!("scratch {scratch:?} holds a single quote or a control character"));
    }
    let parent = path
        .parent()
        .ok_or_else(|| format!("scratch {scratch:?} has no parent"))?
        .canonicalize()
        .map_err(|error| format!("parent of scratch {scratch:?}: {error}"))?;
    if !parent.starts_with(temp_root) {
        return Err(format!("scratch {scratch:?} is not under {}", temp_root.display()));
    }
    match std::fs::symlink_metadata(&path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(path),
        Ok(_) => Err(format!("scratch {scratch:?} already exists")),
        Err(error) => Err(format!("scratch {scratch:?}: {error}")),
    }
}

/// Refuse an inherited `NO_COLOR`, which changes rendering, or `RUST_LOG`, which replaces the
/// configured log level.
#[cfg(any(target_os = "macos", windows, test))]
fn check_environment(no_color: Option<&OsStr>, rust_log: Option<&OsStr>) -> Result<(), String> {
    if no_color.is_some() {
        return Err("remove the inherited NO_COLOR; it changes terminal colors".to_owned());
    }
    if rust_log.is_some() {
        return Err(
            "remove the inherited RUST_LOG; it replaces the configured log level".to_owned()
        );
    }
    Ok(())
}

#[cfg(test)]
#[path = "cli_tests.rs"]
mod cli_tests;
