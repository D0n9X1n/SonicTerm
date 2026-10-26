//! Host-aware CWD and local path resolution for both path grammars.

use super::*;

/// Return the first configured home variable that is valid for the native path grammar.
pub(in crate::app) fn native_home_dir() -> Option<PathBuf> {
    let variables = if cfg!(target_os = "windows") {
        // When: `target_os` is Windows, prefer `USERPROFILE` before validating fallback `HOME`.
        ["USERPROFILE", "HOME"]
    } else {
        // When: `target_os` is not Windows, prefer `HOME` before validating fallback `USERPROFILE`.
        ["HOME", "USERPROFILE"]
    };
    variables.into_iter().filter_map(std::env::var_os).find_map(|value| {
        let value = value.to_str()?;
        match PathStyle::native() {
            PathStyle::Posix => normalize_posix_cwd(value).map(PathBuf::from),
            PathStyle::Windows => normalize_windows_home(value).map(PathBuf::from),
        }
    })
}

pub(in crate::app) fn resolve_path_candidate(
    candidate: &str,
    style: PathStyle,
    cwd: Option<&Osc7Cwd>,
    home: Option<&Path>,
    local_hostname: &str,
) -> Option<PathBuf> {
    let home_relative = is_home_relative(candidate, style);
    let relative =
        is_explicit_relative(candidate, style) || is_contextual_relative(candidate, style);
    let combined = if home_relative {
        let home = home?.to_str()?;
        let suffix = &candidate[2..];
        match style {
            PathStyle::Posix => {
                let home = normalize_posix_cwd(home)?;
                format!("{}/{}", home.trim_end_matches('/'), suffix)
            }
            PathStyle::Windows => {
                let home = normalize_windows_home(home)?;
                format!("{}\\{}", home.trim_end_matches(['/', '\\']), suffix)
            }
        }
    } else if relative {
        // When: `candidate` is dot-relative or separator-relative, resolve it only from this pane's trusted local CWD.
        let cwd = cwd.filter(|cwd| authority_is_local(&cwd.authority, local_hostname))?;
        match style {
            PathStyle::Posix => format!("{}/{}", cwd.path.trim_end_matches('/'), candidate),
            PathStyle::Windows => {
                let cwd = normalize_windows_cwd(&cwd.path)?;
                format!("{}\\{}", cwd.trim_end_matches(['/', '\\']), candidate)
            }
        }
    } else {
        // When: `candidate` is absolute rather than home-relative or dot-relative, resolve it without contextual state.
        candidate.to_string()
    };
    match style {
        PathStyle::Posix => normalize_posix_cwd(&combined).map(PathBuf::from),
        PathStyle::Windows => normalize_windows_absolute(&combined).map(PathBuf::from),
    }
}

pub(in crate::app) fn resolve_detected_path(
    target: &DetectedTarget,
    style: PathStyle,
    cwd: Option<&Osc7Cwd>,
    home: Option<&Path>,
    local_hostname: &str,
) -> Option<PathBuf> {
    match target {
        DetectedTarget::Uri(_) => None,
        DetectedTarget::SourceReference(reference) => {
            let path = if reference.explicit_path {
                DetectedTarget::PathCandidate(reference.path.clone())
            } else {
                // When: reference.explicit_path is false, retain the bare filename's CWD-only provenance.
                DetectedTarget::BareName(reference.path.clone())
            };
            resolve_detected_path(&path, style, cwd, home, local_hostname)
        }
        DetectedTarget::PathCandidate(candidate) => {
            resolve_path_candidate(candidate, style, cwd, home, local_hostname)
        }
        DetectedTarget::BareName(candidate) => {
            // When: `target` is `BareName`, require one safe component and the exact pane's trusted local CWD.
            if candidate.is_empty()
                || candidate.len() > 4096
                || matches!(candidate.as_str(), "." | "..")
                || candidate.contains(['/', '\\'])
                || candidate.chars().any(char::is_control)
            {
                // When: `candidate` is empty, overlong, special, separated, or controlled, reject contextual resolution.
                return None;
            }
            let cwd = cwd.filter(|cwd| authority_is_local(&cwd.authority, local_hostname))?;
            match style {
                PathStyle::Posix => {
                    let cwd = normalize_posix_cwd(&cwd.path)?;
                    normalize_posix_absolute(&format!("{}/{candidate}", cwd.trim_end_matches('/')))
                        .map(PathBuf::from)
                }
                PathStyle::Windows => {
                    // When: `style` is Windows, apply native component restrictions before joining the trusted CWD.
                    if candidate.contains(':')
                        || candidate.ends_with(['.', ' '])
                        || candidate
                            .chars()
                            .any(|character| matches!(character, '<' | '>' | '"' | '|' | '?' | '*'))
                    {
                        // When: Windows `candidate` contains reserved, ADS, or normalization-sensitive syntax, leave it inert.
                        return None;
                    }
                    let cwd = normalize_windows_cwd(&cwd.path)?;
                    normalize_windows_absolute(&format!(
                        "{}\\{candidate}",
                        cwd.trim_end_matches(['/', '\\'])
                    ))
                    .map(PathBuf::from)
                }
            }
        }
    }
}

#[cfg(test)]
pub(super) fn token_bounds(cells: &[&Cell], col: usize) -> (usize, usize) {
    let mut start = col;
    while start > 0 && !cell_delimiter(cells[start - 1]) {
        start -= 1;
    }
    let mut end = col + 1;
    while end < cells.len() && !cell_delimiter(cells[end]) {
        end += 1;
    }
    (start, end)
}

#[cfg(test)]
pub(super) fn cell_delimiter(cell: &Cell) -> bool {
    if cell.flags.contains(CellFlags::WIDE_CONT) {
        // When: `cell` is a wide continuation, keep it attached to its lead cell instead of treating its stored space as a delimiter.
        return false;
    }
    cell.ch.is_whitespace()
        || cell.ch.is_control()
        || matches!(cell.ch, '"' | '\'' | '`' | '<' | '>')
}

pub(super) fn row_fingerprint(row: &Row) -> u64 {
    use std::hash::{Hash, Hasher};

    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    row.hash(&mut hasher);
    hasher.finish()
}

pub(super) fn unsafe_path_cell(cell: &&Cell) -> bool {
    cell.flags.intersects(CellFlags::WIDE | CellFlags::WIDE_CONT)
        || cell.extras().is_some_and(|extras| !extras.is_empty())
}

/// Resolve a bounded local OSC 7 snapshot to a native absolute launch directory without filesystem I/O.
pub(in crate::app) fn local_launch_cwd(
    cwd: &Osc7Cwd,
    style: PathStyle,
    local_hostname: &str,
) -> Option<PathBuf> {
    if cwd.path.len() > 4096
        || cwd.path.chars().any(char::is_control)
        || !authority_is_local(&cwd.authority, local_hostname)
    {
        // When: cwd is oversized, controlled, or remote, it must not redirect a new local shell.
        return None;
    }
    match style {
        PathStyle::Posix => normalize_posix_cwd(&cwd.path).map(PathBuf::from),
        PathStyle::Windows => {
            // When: style is Windows, normalize OSC 7 drive paths without treating a relative drive designator as absolute.
            if cwd.path.len() == 2 && cwd.path.as_bytes()[1] == b':' {
                // When: cwd.path is a bare drive designator, do not promote it to that drive's root.
                return None;
            }
            normalize_windows_cwd(&cwd.path).map(PathBuf::from)
        }
    }
}

pub(super) fn authority_is_local(authority: &str, local_hostname: &str) -> bool {
    authority.is_empty()
        || authority.eq_ignore_ascii_case("localhost")
        || (!local_hostname.is_empty() && authority.eq_ignore_ascii_case(local_hostname))
}

pub(super) fn is_home_relative(candidate: &str, style: PathStyle) -> bool {
    match style {
        PathStyle::Posix => candidate.starts_with("~/"),
        PathStyle::Windows => candidate.starts_with("~/") || candidate.starts_with("~\\"),
    }
}

pub(super) fn is_contextual_relative(candidate: &str, style: PathStyle) -> bool {
    match style {
        PathStyle::Posix => !candidate.starts_with('/') && candidate.contains('/'),
        PathStyle::Windows => {
            let bytes = candidate.as_bytes();
            let drive_absolute = bytes.len() >= 3
                && bytes[0].is_ascii_alphabetic()
                && bytes[1] == b':'
                && matches!(bytes[2], b'/' | b'\\');
            !drive_absolute && candidate.contains(['/', '\\'])
        }
    }
}

pub(super) fn is_explicit_relative(candidate: &str, style: PathStyle) -> bool {
    match style {
        PathStyle::Posix => candidate.starts_with("./") || candidate.starts_with("../"),
        PathStyle::Windows => {
            candidate.starts_with("./")
                || candidate.starts_with(".\\")
                || candidate.starts_with("../")
                || candidate.starts_with("..\\")
        }
    }
}

pub(super) fn normalize_posix_cwd(path: &str) -> Option<String> {
    if path == "/" {
        Some(path.to_string())
    } else {
        // When: `path` is not the POSIX root, require an ordinary named absolute CWD.
        normalize_posix_absolute(path)
    }
}

pub(super) fn normalize_posix_absolute(path: &str) -> Option<String> {
    if !path.starts_with('/') || path.starts_with("//") || path.contains('\\') {
        // When: `path` is not a single-root POSIX absolute path, reject relative, network, and cross-platform forms.
        return None;
    }
    let mut components = Vec::new();
    for component in path.split('/') {
        match component {
            "" | "." => {
                // When: `component` is empty or `.`, omit the non-naming POSIX segment.
            }
            ".." => {
                // Pop one lexical ancestor while clamping traversal at the root.
                components.pop();
            }
            value => components.push(value),
        }
    }
    (!components.is_empty()).then(|| format!("/{}", components.join("/")))
}

pub(super) fn normalize_windows_home(path: &str) -> Option<String> {
    let bytes = path.as_bytes();
    if bytes.len() < 3
        || !bytes[0].is_ascii_alphabetic()
        || bytes[1] != b':'
        || !matches!(bytes[2], b'/' | b'\\')
    {
        // When: `bytes` lack a drive-root separator, reject drive-relative home input instead of promoting `C:` to `C:\`.
        return None;
    }
    normalize_windows_absolute(path)
}

pub(super) fn normalize_windows_cwd(path: &str) -> Option<String> {
    let normalized = path.replace('/', "\\");
    let normalized = if normalized.len() >= 4
        && normalized.starts_with('\\')
        && normalized.as_bytes()[1].is_ascii_alphabetic()
        && normalized.as_bytes()[2] == b':'
        && normalized.as_bytes()[3] == b'\\'
    {
        normalized[1..].to_string()
    } else {
        // When: `normalized` is not the OSC 7 `/C:/...` drive form, retain it for ordinary absolute validation.
        normalized
    };
    normalize_windows_absolute(&normalized)
}

pub(super) fn normalize_windows_absolute(path: &str) -> Option<String> {
    if path.starts_with("\\\\") || path.starts_with("//") {
        // When: `path` starts with a double separator, reject unsupported UNC and network targets.
        return None;
    }
    let path = path.replace('/', "\\");
    let bytes = path.as_bytes();
    if bytes.len() == 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
        // When: `bytes` are exactly a drive designator, normalize the Windows root with its trailing separator.
        return Some(format!("{}:\\", bytes[0] as char));
    }
    if bytes.len() < 3 || !bytes[0].is_ascii_alphabetic() || bytes[1] != b':' || bytes[2] != b'\\' {
        // When: `bytes` do not form a drive-rooted absolute path, reject relative and malformed Windows input.
        return None;
    }
    let drive = (bytes[0] as char).to_ascii_uppercase();
    let mut components = Vec::new();
    for component in path[3..].split('\\') {
        match component {
            "" | "." => {
                // When: `component` is empty or `.`, omit the non-naming Windows segment.
            }
            ".." => {
                components.pop();
            }
            value
                if value.contains(':')
                    || value.chars().any(|character| {
                        matches!(character, '<' | '>' | '"' | '|' | '?' | '*')
                    }) =>
            {
                // When: `value` contains a reserved Windows path character, reject the component.
                return None;
            }
            value if value.ends_with(['.', ' ']) => {
                // When: `value` ends in a dot or space, reject Windows normalization aliases before probing.
                return None;
            }
            value => components.push(value),
        }
    }
    if components.is_empty() {
        Some(format!("{drive}:\\"))
    } else {
        // When: `components` contains named segments, append them beneath the normalized drive root.
        Some(format!("{drive}:\\{}", components.join("\\")))
    }
}

pub(super) fn detected_target_enabled(
    target: &DetectedTarget,
    clickable_local_targets: bool,
    clickable_bare_names: bool,
) -> bool {
    match target {
        DetectedTarget::Uri(_) => true,
        DetectedTarget::SourceReference(reference) => {
            clickable_local_targets && (reference.explicit_path || clickable_bare_names)
        }
        DetectedTarget::PathCandidate(_) => clickable_local_targets,
        DetectedTarget::BareName(_) => clickable_local_targets && clickable_bare_names,
    }
}

/// Unicode scalars of a detected filesystem target's displayed text, the unit of
/// `clickable_path_max_chars` and of probe tiers.
pub(super) fn detected_target_chars(target: &DetectedTarget) -> usize {
    match target {
        DetectedTarget::PathCandidate(text) | DetectedTarget::BareName(text) => {
            text.chars().count()
        }
        DetectedTarget::SourceReference(reference) => reference.display.chars().count(),
        DetectedTarget::Uri(uri) => uri.chars().count(),
    }
}

#[cfg(test)]
#[path = "path_target_resolve_tests.rs"]
mod path_target_resolve_tests;
