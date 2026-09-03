//! Foreground-process probe for a pty's controlling shell.
//!
//! Used by the tab-title renderer to pick a Nerd Font icon based on what's
//! actually running in the pane right now (zsh vs nvim vs ssh vs cargo).
//!
//! macOS uses `libproc`; Windows snapshots the native process table. Both
//! walk to the deepest shell descendant so `nvim foo` reports `nvim`, not the
//! waiting shell. Other platforms return no foreground-process name.

/// Foreground process identity and privilege presentation state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForegroundProcess {
    /// Normalized executable basename used by tab-title icon matching.
    pub name: String,
    /// Whether this foreground process requires a privilege warning.
    pub privileged: bool,
}

/// Best-effort foreground process for the pty whose shell has the given `pid`.
///
/// The name is a normalized basename. Windows also reports whether the selected
/// foreground process requires a privilege warning; other platforms leave that
/// field false because process-level privilege is captured at native startup.
#[cfg(target_os = "macos")]
pub fn foreground_process_info(pid: u32) -> Option<ForegroundProcess> {
    macos::foreground_process(pid).map(|name| ForegroundProcess { name, privileged: false })
}

/// Best-effort foreground process for the deepest Windows shell descendant.
#[cfg(windows)]
pub fn foreground_process_info(pid: u32) -> Option<ForegroundProcess> {
    crate::foreground_proc::current_foreground_process(pid)
        .map(|process| ForegroundProcess { name: process.name, privileged: process.privileged })
}

/// Reports no foreground process on platforms without an implementation.
#[cfg(not(any(target_os = "macos", windows)))]
pub fn foreground_process_info(_pid: u32) -> Option<ForegroundProcess> {
    None
}

/// Best-effort foreground processes for several Windows pty shell pids.
///
/// One native process-table snapshot serves every input pid. Results preserve
/// input order and represent per-process races independently with `None`.
#[cfg(windows)]
pub fn foreground_processes_info(pids: &[u32]) -> Vec<Option<ForegroundProcess>> {
    crate::foreground_proc::current_foreground_processes(pids)
        .into_iter()
        .map(|process| {
            process.map(|process| ForegroundProcess {
                name: process.name,
                privileged: process.privileged,
            })
        })
        .collect()
}

/// Best-effort foreground process name for tab-title icon matching.
pub fn foreground_process(pid: u32) -> Option<String> {
    foreground_process_info(pid).map(|process| process.name)
}

/// Normalize a process name reported by the OS into a stable exact-match key.
///
/// Both POSIX and Windows paths reduce to their basename. One login-shell `-`
/// prefix and one case-insensitive `.exe` suffix are removed before matching.
pub fn normalize_proc_name(raw: &str) -> String {
    let basename = raw.rsplit(['/', '\\']).next().unwrap_or(raw);
    let basename = basename.strip_prefix('-').unwrap_or(basename);
    let lowercase = basename.to_ascii_lowercase();
    lowercase.strip_suffix(".exe").unwrap_or(&lowercase).to_string()
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
                // When: `p` is the macOS kernel task, never a descendant of a user shell.
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

#[cfg(test)]
#[path = "proc_info_tests.rs"]
mod proc_info_tests;
