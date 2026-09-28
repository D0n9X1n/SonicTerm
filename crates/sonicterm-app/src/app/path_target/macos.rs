//! macOS path probes and direct-open. Application bundles and launcher-class directories
//! classify as reveal-only; actions run `/usr/bin/open` with fixed arguments, `-R --` to select
//! in Finder and `--` to navigate a directory that still classifies as authorized.

use super::*;

use super::unix::CommandSpec;
#[cfg(target_os = "macos")]
use super::unix::{classify_followed_target, run_command};

#[cfg(target_os = "macos")]
pub(super) fn classify_local_target(path: &Path) -> PathOpenDecision {
    classify_macos_target(path)
}

#[cfg(any(target_os = "macos", test))]
pub(super) fn macos_directory_policy(path: &Path) -> PathOpenDecision {
    const BLOCKED_EXTENSIONS: &[&str] = &[
        "app",
        "command",
        "terminal",
        "workflow",
        "scpt",
        "applescript",
        "pkg",
        "mpkg",
        "dmg",
        "webloc",
        "osascript",
    ];
    let extension = path.extension().and_then(|value| value.to_str()).unwrap_or_default();
    let blocked_extension =
        BLOCKED_EXTENSIONS.iter().any(|blocked| extension.eq_ignore_ascii_case(blocked));
    if blocked_extension {
        PathOpenDecision::Revealable(PathKind::Directory)
    } else {
        // When: blocked_extension is false, the directory can be navigated without launching a package.
        PathOpenDecision::Openable(PathKind::Directory)
    }
}

#[cfg(target_os = "macos")]
fn classify_macos_target(path: &Path) -> PathOpenDecision {
    let kind = match classify_followed_target(path) {
        Ok(kind) => kind,
        Err(decision) => {
            // When: `classify_followed_target` returns `Err`, retain its missing-or-blocked decision unchanged.
            return decision;
        }
    };
    if kind == PathKind::Directory {
        // When: `kind` is `Directory`, inspect bundle metadata before allowing Finder to open the target itself.
        let policy = macos_directory_policy(path);
        if matches!(policy, PathOpenDecision::Revealable(_)) {
            // When: matches! identifies Revealable policy, select the package instead of launching its handler.
            return policy;
        }
        let bundle_marker = path.join("Contents/Info.plist");
        match std::fs::symlink_metadata(bundle_marker) {
            Ok(_) => {
                // When: bundle_marker exists, select the package without invoking LaunchServices.
                return PathOpenDecision::Revealable(PathKind::Directory);
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                // When: `bundle_marker` is `NotFound`, the directory has no application-bundle marker and remains eligible.
            }
            Err(_) => {
                // When: reading `bundle_marker` fails for another reason, fail closed instead of assuming a safe directory.
                return PathOpenDecision::Blocked;
            }
        }
        return PathOpenDecision::Openable(PathKind::Directory);
    }

    PathOpenDecision::Openable(PathKind::File)
}

#[cfg(target_os = "macos")]
pub(super) fn reveal_native_file(path: &Path) -> io::Result<()> {
    let spec = path
        .to_str()
        .and_then(macos_reveal_spec)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "invalid reveal path"))?;
    run_command(spec)
}

#[cfg(target_os = "macos")]
fn macos_validated_open_spec(
    path: &Path,
    expected_decision: PathOpenDecision,
) -> io::Result<CommandSpec> {
    if classify_macos_target(path) != expected_decision {
        // When: `classify_macos_target` no longer matches `expected_decision`, reject changed identity, kind, or action.
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "changed or blocked macOS target",
        ));
    }
    let text = path
        .to_str()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "path is not UTF-8"))?;
    match expected_decision {
        PathOpenDecision::Openable(PathKind::Directory) => macos_open_spec(text),
        PathOpenDecision::Openable(PathKind::File) => macos_reveal_spec(text),
        PathOpenDecision::Revealable(_) => macos_reveal_spec(text),
        PathOpenDecision::SourceReveal | PathOpenDecision::Blocked | PathOpenDecision::Missing => {
            None
        }
    }
    .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "invalid macOS path"))
}

#[cfg(target_os = "macos")]
pub(super) fn open_native_path(path: &Path, expected_decision: PathOpenDecision) -> io::Result<()> {
    run_command(macos_validated_open_spec(path, expected_decision)?)
}

#[cfg(any(target_os = "macos", test))]
pub(super) fn macos_open_spec(path: &str) -> Option<CommandSpec> {
    let path = normalize_posix_absolute(path)?;
    Some(CommandSpec {
        program: PathBuf::from("/usr/bin/open"),
        args: vec!["--".to_string(), path],
    })
}

#[cfg(any(target_os = "macos", test))]
pub(super) fn macos_reveal_spec(path: &str) -> Option<CommandSpec> {
    let path = normalize_posix_absolute(path)?;
    Some(CommandSpec {
        program: PathBuf::from("/usr/bin/open"),
        args: vec!["-R".to_string(), "--".to_string(), path],
    })
}

#[cfg(test)]
#[path = "macos_tests.rs"]
mod macos_tests;
