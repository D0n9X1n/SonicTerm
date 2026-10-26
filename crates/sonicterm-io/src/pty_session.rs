#[cfg(unix)]
use super::*;

#[cfg(target_os = "macos")]
fn unix_session_pids(session_id: u32) -> std::io::Result<Vec<u32>> {
    use libproc::processes::{pids_by_type, ProcFilter};

    let mut members = Vec::new();
    for pid in pids_by_type(ProcFilter::All)? {
        if pid == 0 {
            // When: `pid` is 0, the kernel task is no session member, and `getsid(0)` would read our own session.
            continue;
        }
        if (
            // SAFETY: nonzero `pid` came from the process table; `getsid` only reads its session id.
            unsafe { libc::getsid(pid as libc::pid_t) }
        ) != session_id as libc::pid_t
        {
            // When: `getsid` does not match `session_id`, including `ESRCH` for a zombie or exiting process, `pid` is not listed.
            continue;
        }
        if !unix_process_is_active(pid)? {
            // When: `unix_process_is_active(pid)` is false, this zombie or reaped member needs no signal.
            continue;
        }
        members.push(pid);
    }
    Ok(members)
}

/// One read of a PTY session member's macOS process-table entry.
#[cfg(target_os = "macos")]
#[derive(Debug, PartialEq, Eq)]
enum MacosProcessState {
    /// The pid has no process-table entry (`ESRCH`): the member was reaped.
    Gone,
    /// The kernel returned the member's entry.
    Read {
        /// The parent pid when read; `1` once `launchd` has adopted an orphan.
        ppid: u32,
        /// The process group id when read; a member still in the session's group has the session's id.
        pgid: u32,
        /// The kernel `p_stat` value, such as `SRUN` or `SZOMB`.
        status: u32,
        /// Whether the kernel flags the member as working its way through exit.
        in_exit: bool,
        /// The kernel's command name, at most 16 bytes.
        command: String,
    },
    /// The entry could not be read; `errno` is 0 for an incomplete record.
    Unreadable { errno: i32 },
}

/// `PROC_FLAG_INEXIT` from `<sys/proc_info.h>`, which `libc` does not export.
#[cfg(target_os = "macos")]
const MACOS_PROC_FLAG_INEXIT: u32 = 0x4;

/// Reads a session member's state, command, and in-exit flag.
///
/// `PROC_PIDT_SHORTBSDINFO` has no same-user check, so a member that changed
/// credentials stays readable, and argument 1 also finds zombies, for which
/// `getsid` reports `ESRCH`.
#[cfg(target_os = "macos")]
fn macos_process_state(pid: u32) -> MacosProcessState {
    let mut info: libc::proc_bsdshortinfo =
        // SAFETY: `proc_bsdshortinfo` holds only integers and byte arrays, so all-zero bytes are a valid value.
        unsafe { std::mem::zeroed() };
    let size = std::mem::size_of::<libc::proc_bsdshortinfo>() as libc::c_int;
    let written =
        // SAFETY: `info` is writable storage of exactly `size` bytes, and the call writes at most `size` bytes.
        unsafe {
            libc::proc_pidinfo(
                pid as libc::c_int,
                libc::PROC_PIDT_SHORTBSDINFO,
                1,
                std::ptr::addr_of_mut!(info).cast(),
                size,
            )
        };
    if written == size {
        // When: `written == size`, the kernel filled the whole record, so every field is valid.
        let command = info
            .pbsi_comm
            .iter()
            .map(|byte| byte.to_ne_bytes()[0])
            .take_while(|byte| *byte != 0)
            .collect::<Vec<u8>>();
        return MacosProcessState::Read {
            ppid: info.pbsi_ppid,
            pgid: info.pbsi_pgid,
            status: info.pbsi_status,
            in_exit: info.pbsi_flags & MACOS_PROC_FLAG_INEXIT != 0,
            command: String::from_utf8_lossy(&command).into_owned(),
        };
    }
    if written > 0 {
        // When: `written` is positive but short, the record is incomplete and no errno describes it.
        return MacosProcessState::Unreadable { errno: 0 };
    }
    match std::io::Error::last_os_error().raw_os_error() {
        Some(libc::ESRCH) => MacosProcessState::Gone,
        errno => MacosProcessState::Unreadable { errno: errno.unwrap_or(0) },
    }
}

/// Whether a macOS session member still needs a signal and a report.
///
/// Only a zombie or a reaped pid is inactive, as Linux drops `Z`, `X`, and `x`.
/// An unreadable member stays active, so cleanup never reports success it could
/// not observe. A member flagged in exit also stays, since it can still hold the
/// PTY, although the scan's earlier `getsid` match can already drop it.
#[cfg(target_os = "macos")]
fn macos_state_is_active(state: &MacosProcessState) -> bool {
    !matches!(state, MacosProcessState::Gone | MacosProcessState::Read { status: libc::SZOMB, .. })
}

/// Names a surviving session member for the termination error.
///
/// Its parent, process group, kernel state, in-exit flag, and command are one
/// fresh read when the error is built, not proof of ancestry or identity, and
/// `kill=` is the outcome of the latest bounded pass that listed it.
#[cfg(target_os = "macos")]
fn describe_session_member(pid: u32, last_signal: KillOutcome) -> String {
    format_session_survivor(pid, &macos_process_state(pid), last_signal)
}

/// Formats one survivor from its process-table state and latest signal outcome.
///
/// `kill=` describes the latest pass that listed the pid: `ok` means the kernel
/// accepted SIGKILL, an errno that it refused, and `skipped-recheck` that the
/// pre-signal recheck skipped it; `unlisted` means no pass listed it before the
/// final scan. Missing history is never errno 0.
#[cfg(target_os = "macos")]
fn format_session_survivor(
    pid: u32,
    state: &MacosProcessState,
    last_signal: KillOutcome,
) -> String {
    let kill = kill_text(last_signal);
    match state {
        MacosProcessState::Read { ppid, pgid, status, in_exit, command } => format!(
            "pid={pid} ppid={ppid} pgid={pgid} state={} in_exit={in_exit} comm={command:?} kill={kill}",
            macos_status_name(*status)
        ),
        MacosProcessState::Gone => format!("pid={pid} state=gone kill={kill}"),
        MacosProcessState::Unreadable { errno: 0 } => {
            format!("pid={pid} state=unreadable(short record) kill={kill}")
        }
        MacosProcessState::Unreadable { errno } => {
            format!("pid={pid} state=unreadable({}) kill={kill}", errno_name(*errno))
        }
    }
}

/// Names a SIGKILL outcome for the survivor report.
#[cfg(target_os = "macos")]
fn kill_text(signal: KillOutcome) -> String {
    match signal {
        KillOutcome::Sent => "ok".to_owned(),
        KillOutcome::Refused(Some(errno)) => errno_name(errno),
        KillOutcome::Refused(None) => "refused".to_owned(),
        KillOutcome::SkippedRecheck => "skipped-recheck".to_owned(),
        KillOutcome::Unlisted => "unlisted".to_owned(),
    }
}

/// Reports the session's group SIGKILL result once, after the survivor list.
#[cfg(target_os = "macos")]
fn describe_group_kill(group_kill: KillOutcome) -> String {
    format!("; group_kill={}", kill_text(group_kill))
}

/// Names an errno for the survivor report, falling back to its number.
#[cfg(target_os = "macos")]
fn errno_name(errno: i32) -> String {
    match errno {
        libc::EPERM => "EPERM".to_owned(),
        libc::ESRCH => "ESRCH".to_owned(),
        libc::EINVAL => "EINVAL".to_owned(),
        other => format!("errno {other}"),
    }
}

/// Names a macOS `p_stat` value.
#[cfg(target_os = "macos")]
fn macos_status_name(status: u32) -> String {
    match status {
        libc::SIDL => "SIDL".to_owned(),
        libc::SRUN => "SRUN".to_owned(),
        libc::SSLEEP => "SSLEEP".to_owned(),
        libc::SSTOP => "SSTOP".to_owned(),
        libc::SZOMB => "SZOMB".to_owned(),
        other => format!("p_stat {other}"),
    }
}

/// Names a surviving session member by pid; only macOS reports its ids, state, command, and kill outcome here.
#[cfg(all(any(unix, test), not(target_os = "macos")))]
fn describe_session_member(pid: u32, _last_signal: KillOutcome) -> String {
    pid.to_string()
}

/// Adds nothing on other platforms, which keep the pid-only survivor list.
#[cfg(all(any(unix, test), not(target_os = "macos")))]
fn describe_group_kill(_group_kill: KillOutcome) -> String {
    String::new()
}

#[cfg(any(all(unix, not(target_os = "macos")), test))]
fn linux_proc_stat_is_active(stat: &str) -> Option<bool> {
    let (_, suffix) = stat.rsplit_once(") ")?;
    let state = suffix.as_bytes().first().copied()?;
    Some(!matches!(state, b'Z' | b'X' | b'x'))
}

#[cfg(all(unix, not(target_os = "macos")))]
pub(super) fn unix_process_is_active(pid: u32) -> std::io::Result<bool> {
    match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
        Ok(stat) => Ok(linux_proc_stat_is_active(&stat).unwrap_or(true)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

/// Whether `pid` is still a live macOS session member; only zombies and reaped pids are not.
#[cfg(target_os = "macos")]
pub(super) fn unix_process_is_active(pid: u32) -> std::io::Result<bool> {
    Ok(macos_state_is_active(&macos_process_state(pid)))
}

#[cfg(all(unix, not(target_os = "macos")))]
fn unix_session_pids(session_id: u32) -> std::io::Result<Vec<u32>> {
    let mut pids = Vec::new();
    for entry in std::fs::read_dir("/proc")? {
        let entry = entry?;
        let Some(pid) = entry.file_name().to_string_lossy().parse::<u32>().ok() else {
            // When: a `/proc` entry is not a numeric `pid`, it cannot identify a session member.
            continue;
        };
        if (
            // SAFETY: `pid` was parsed from a `/proc` process entry; `getsid` only reads its session id.
            unsafe { libc::getsid(pid as libc::pid_t) }
        ) == session_id as libc::pid_t
        {
            // When: `libc::getsid(pid as libc::pid_t) == session_id as libc::pid_t`, inspect this session member's state.
            if !unix_process_is_active(pid)? {
                // When: `unix_process_is_active(pid)` is false, this terminated session member needs no signal.
                continue;
            }
            pids.push(pid);
        }
    }
    Ok(pids)
}

#[cfg(unix)]
pub(super) fn terminate_unix_session(session_id: u32) -> std::io::Result<()> {
    // Signal the shell's original process group even if process-table access
    // is restricted.
    let group_sent =
        // SAFETY: negative `session_id` targets the child-created process group; `kill` receives no pointers.
        unsafe { libc::kill(-(session_id as libc::pid_t), libc::SIGKILL) };
    let group_kill = kill_outcome(group_sent);
    terminate_session_members(
        group_kill,
        || {
            Ok(unix_session_pids(session_id)?
                .into_iter()
                .filter(|pid| *pid != session_id && *pid != std::process::id())
                .collect())
        },
        |pid| {
            // Recheck membership immediately before signalling.
            if
            // SAFETY: pid was just enumerated; getsid reads its session without changing process state.
            unsafe { libc::getsid(pid as libc::pid_t) } != session_id as libc::pid_t {
                // When: getsid no longer matches session_id, pid reuse makes signalling this process unsafe.
                return KillOutcome::SkippedRecheck;
            }
            let sent =
                // SAFETY: membership was rechecked immediately above; kill receives the member pid by value.
                unsafe { libc::kill(pid as libc::pid_t, libc::SIGKILL) };
            kill_outcome(sent)
        },
        || std::thread::sleep(Duration::from_millis(5)),
    )
}

/// The outcome of a SIGKILL, kept for the survivor report.
///
/// For a member it is the latest bounded pass that listed it, which overwrites
/// earlier passes: `Sent` means the kernel accepted SIGKILL, `Refused` that it
/// rejected it, `SkippedRecheck` that the pre-signal recheck skipped the pid, and
/// `Unlisted` that no pass listed it before the final scan. The group kill uses
/// only `Sent` and `Refused`.
#[cfg(any(unix, test))]
// Only macOS reads the outcome into the survivor report; other platforms keep the pid list.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum KillOutcome {
    /// The kernel accepted SIGKILL.
    Sent,
    /// `kill` failed with the errno read right after the call; `None` if the OS reported none.
    Refused(Option<i32>),
    /// The pre-signal membership recheck no longer matched, so the pid was not signalled.
    SkippedRecheck,
    /// The member was first listed by the final scan, after every signal pass.
    Unlisted,
}

/// Converts a `kill` return value into its outcome.
///
/// Call it right after `kill`, before another call can overwrite errno.
#[cfg(unix)]
fn kill_outcome(result: libc::c_int) -> KillOutcome {
    if result == 0 {
        // When: `result` is 0, the kernel accepted SIGKILL.
        return KillOutcome::Sent;
    }
    KillOutcome::Refused(std::io::Error::last_os_error().raw_os_error())
}

#[cfg(any(unix, test))]
fn terminate_session_members(
    group_kill: KillOutcome,
    mut members: impl FnMut() -> std::io::Result<Vec<u32>>,
    mut signal: impl FnMut(u32) -> KillOutcome,
    mut pause: impl FnMut(),
) -> std::io::Result<()> {
    // Each member's latest outcome, so a survivor shows whether its last signal was accepted.
    let mut last_signal = std::collections::HashMap::new();
    for _ in 0..8 {
        let remaining = members()?;
        if remaining.is_empty() {
            // When: remaining is empty, every descendant is gone and session cleanup is complete.
            return Ok(());
        }
        for pid in remaining {
            last_signal.insert(pid, signal(pid));
        }
        pause();
    }
    let remaining = members()?;
    if remaining.is_empty() {
        // When: remaining is empty after the last pause, the final signal completed within the existing attempt budget.
        return Ok(());
    }
    // State is re-read after the final scan, so on macOS a survivor that exits in between reads `state=gone`.
    // A pid no pass listed before the final scan reads `kill=unlisted`.
    let survivors = remaining
        .iter()
        .map(|pid| {
            let last = last_signal.get(pid).copied().unwrap_or(KillOutcome::Unlisted);
            describe_session_member(*pid, last)
        })
        .collect::<Vec<_>>();
    Err(std::io::Error::new(
        std::io::ErrorKind::WouldBlock,
        format!(
            "PTY session still has live descendants after termination attempts: [{}]{}",
            survivors.join(", "),
            describe_group_kill(group_kill)
        ),
    ))
}

#[cfg(test)]
#[path = "pty_session_tests.rs"]
mod pty_session_tests;
