//! Foreground-process probe for a pty's controlling shell.
//!
//! Used by the tab-title renderer to pick a Nerd Font icon based on what's
//! actually running in the pane right now (zsh vs nvim vs ssh vs cargo).
//!
//! macOS uses `libproc`'s `pidpath` + a simple `proc_listpids`-based walk
//! to find the deepest descendant of the shell pid: when you type `nvim
//! foo`, the shell forks `nvim` and waits — `nvim` becomes the foreground
//! process and we want its name, not the shell's. Linux/Windows are stubs
//! for v1; we'll fill them in when those platforms come online.

/// Best-effort foreground process name for the pty whose shell has the
/// given `pid`. Returns the *basename* (no path, no leading `-`), or `None`
/// if the platform layer can't determine it.
///
/// Conventions to keep callers stable:
/// - Login shells often appear as `-zsh`; the leading `-` is stripped.
/// - We walk descendants of `pid` and prefer the deepest one (i.e. what
///   the shell is currently waiting on) so opening `nvim` reports `"nvim"`,
///   not `"zsh"`.
#[cfg(target_os = "macos")]
pub fn foreground_process(pid: u32) -> Option<String> {
    macos::foreground_process(pid)
}

#[cfg(windows)]
pub fn foreground_process(pid: u32) -> Option<String> {
    crate::foreground_proc::current_foreground_pid(pid).map(|(_pid, name)| name)
}

#[cfg(not(any(target_os = "macos", windows)))]
pub fn foreground_process(_pid: u32) -> Option<String> {
    None
}

/// Normalize a process name reported by the OS into a stable key.
/// - strips a leading `-` (login-shell convention)
/// - returns only the file basename
/// - lowercases on macOS (libproc is case-preserving but everyone matches
///   on lowercase keys)
pub fn normalize_proc_name(raw: &str) -> String {
    let basename = raw.rsplit('/').next().unwrap_or(raw);
    let trimmed = basename.strip_prefix('-').unwrap_or(basename);
    trimmed.to_ascii_lowercase()
}

#[cfg(target_os = "macos")]
mod macos {
    use libproc::libproc::bsd_info::BSDInfo;
    use libproc::libproc::proc_pid::{pidinfo, pidpath};
    use libproc::processes::{pids_by_type, ProcFilter};

    use super::normalize_proc_name;

    pub fn foreground_process(pid: u32) -> Option<String> {
        // Walk the process table once and find the deepest descendant of
        // `pid`. This is O(N) in total processes which on macOS is ~600 —
        // negligible (sub-millisecond) and we only call it from the tab-
        // title refresh path (≤ once per render).
        let all = pids_by_type(ProcFilter::All).ok()?;

        // Build a (child_pid, parent_pid) list, skipping ourselves and
        // entries we can't introspect (kernel, restricted, gone).
        let mut entries: Vec<(u32, u32)> = Vec::with_capacity(all.len());
        for p in all {
            if p == 0 {
                continue;
            }
            if let Ok(info) = pidinfo::<BSDInfo>(p as i32, 0) {
                entries.push((p, info.pbi_ppid));
            }
        }

        // BFS from `pid` downward; track the deepest pid found.
        let mut deepest = pid;
        let mut deepest_depth = 0usize;
        let mut frontier: Vec<(u32, usize)> = vec![(pid, 0)];
        while let Some((cur, depth)) = frontier.pop() {
            for (child, parent) in entries.iter() {
                if *parent == cur {
                    let next = depth + 1;
                    if next > deepest_depth {
                        deepest_depth = next;
                        deepest = *child;
                    }
                    frontier.push((*child, next));
                }
            }
        }

        let path = pidpath(deepest as i32).ok()?;
        Some(normalize_proc_name(&path))
    }
}

// Unit tests for `normalize_proc_name` live in `tests/proc_info.rs`.
