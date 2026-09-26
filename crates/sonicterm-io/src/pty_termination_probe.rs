//! Post-return PTY observations for an isolated diagnostic; never signals or reaps.

use std::io;

pub(super) const MAX_BYTES: usize = 1 << 20;
pub(super) const MAX_RECORDS: usize = 4096;
pub(super) const MAX_IDENTITIES: usize = 256;
pub(super) const MAX_CHUNKS: usize = 256;
pub(super) const MAX_PAYLOAD: usize = 16 << 20;

// All tests in the counting-allocator process share this lock before allocating fixture state.
#[cfg(test)]
pub(super) static TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Identity {
    pub(super) pid: u32,
    pub(super) seconds: u64,
    pub(super) micros: u64,
}

impl Identity {
    fn known(self) -> bool {
        self.pid > 0 && self.seconds > 0 && self.micros < 1_000_000
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Observation {
    pub(super) state: u8,
    pub(super) identity: Identity,
    pub(super) recheck: Identity,
    pub(super) ppid: u32,
    pub(super) pgid: u32,
    pub(super) sid: i32,
    pub(super) sid_errno: i32,
    pub(super) status: u32,
    pub(super) in_exit: bool,
    pub(super) errno: i32,
    pub(super) command: [u8; 16],
}

impl Observation {
    fn absent(pid: u32, state: u8, errno: i32) -> Self {
        let identity = Identity { pid, seconds: 0, micros: 0 };
        Self {
            state,
            identity,
            recheck: identity,
            ppid: 0,
            pgid: 0,
            sid: -1,
            sid_errno: 0,
            status: 0,
            in_exit: false,
            errno,
            command: [0; 16],
        }
    }
}

pub(super) fn unknown_observation(pid: u32) -> Observation {
    Observation::absent(pid, 2, libc::ETIMEDOUT)
}

fn native_info(pid: u32) -> Result<libc::proc_bsdinfo, Observation> {
    if pid == 0 || pid > i32::MAX as u32 {
        // When: pid cannot name a positive native process, so preserve unknown identity without querying the kernel.
        return Err(Observation::absent(pid, 2, libc::EINVAL));
    }
    let mut info = std::mem::MaybeUninit::<libc::proc_bsdinfo>::zeroed();
    let size = std::mem::size_of::<libc::proc_bsdinfo>();
    let written =
        // SAFETY: info is aligned writable storage of size bytes; argument 1 includes unreaped zombies.
        unsafe {
        libc::proc_pidinfo(
            pid as i32,
            libc::PROC_PIDTBSDINFO,
            1,
            info.as_mut_ptr().cast(),
            size as i32,
        )
    };
    if written == size as i32 {
        // When: written covers the complete proc_bsdinfo, allowing field reads rather than treating partial bytes as identity.
        let info =
            // SAFETY: written equals size, so the native call initialized the complete info record.
            unsafe { info.assume_init() };
        if info.pbi_pid != pid {
            // When: pbi_pid differs from pid, so the returned record cannot establish the requested identity.
            return Err(Observation::absent(pid, 3, 0));
        }
        return Ok(info);
    }
    let errno = if written > 0 {
        // A short positive written count has no meaningful errno.
        0
    } else {
        // When: written reports native failure, retain its errno before another call can replace it.
        io::Error::last_os_error().raw_os_error().unwrap_or(0)
    };
    Err(Observation::absent(pid, if errno == libc::ESRCH { 0 } else { 2 }, errno))
}

fn from_native(pid: u32, info: &libc::proc_bsdinfo) -> Observation {
    let mut command = [0; 16];
    for (out, byte) in command.iter_mut().zip(info.pbi_comm.iter()) {
        *out = *byte as u8;
    }
    let identity = Identity { pid, seconds: info.pbi_start_tvsec, micros: info.pbi_start_tvusec };
    Observation {
        state: 1,
        identity,
        recheck: identity,
        ppid: info.pbi_ppid,
        pgid: info.pbi_pgid,
        sid: -1,
        sid_errno: 0,
        status: info.pbi_status,
        in_exit: info.pbi_flags & 4 != 0,
        errno: 0,
        command,
    }
}

/// Bracket the session lookup with birth reads; disappearance never erases an earlier known birth.
pub(super) fn observe(pid: u32) -> Observation {
    let mut first = match native_info(pid) {
        Ok(info) => from_native(pid, &info),
        Err(value) => {
            // When: native_info returns Err(value), retain unknown or gone without another native query.
            return value;
        }
    };
    first.sid =
        // SAFETY: native_info validated pid; getsid only reads its session and never consumes wait status.
        unsafe { libc::getsid(pid as i32) };
    first.sid_errno = if first.sid < 0 {
        // Capture the failed first.sid errno before the birth recheck overwrites it.
        io::Error::last_os_error().raw_os_error().unwrap_or(0)
    } else {
        // When: first.sid is valid, no session-query error accompanies this observation.
        0
    };
    match native_info(pid) {
        Ok(info) => {
            // When: native_info returned a second record, so compare births before combining its state with first.sid.
            let mut last = from_native(pid, &info);
            if first.identity != last.identity {
                // When: first.identity differs from last.identity, making the intervening session read ambiguous through PID reuse.
                first.state = 3;
                first.recheck = last.identity;
                return first;
            }
            last.sid = first.sid;
            last.sid_errno = first.sid_errno;
            last
        }
        Err(value) => {
            // Preserve first.identity when a later read cannot provide a current birth.
            first.state = value.state;
            first.errno = value.errno;
            first.recheck = value.identity;
            first
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum Verdict {
    SettledLate,
    Persistent,
    NoEof,
    Unknown,
}

/// Eventual settlement does not establish signal delivery, ancestry, or a causal fix.
pub(super) fn classify(
    expected: &[Identity],
    current: &[Observation],
    disconnected: bool,
    unknown: bool,
) -> Verdict {
    if unknown
        || expected.is_empty()
        || expected.len() > MAX_IDENTITIES
        || expected.len() != current.len()
        || expected.iter().any(|id| !id.known())
    {
        // When: expected or current is incomplete, oversized or unknown, so no partial population may yield settlement.
        return Verdict::Unknown;
    }
    if expected
        .iter()
        .enumerate()
        .any(|(index, id)| expected[..index].iter().any(|prior| prior.pid == id.pid))
    {
        // When: expected repeats a PID, so distinct births cannot be paired unambiguously with current observations.
        return Verdict::Unknown;
    }
    let mut live = false;
    for (id, value) in expected.iter().zip(current) {
        if value.identity.pid != id.pid {
            // When: value belongs to another PID, so pairing it with id could manufacture a terminal observation.
            return Verdict::Unknown;
        }
        match value.state {
            0 if value.errno == libc::ESRCH
                && (!value.identity.known() || value.identity == *id) =>
            {
                // When: value has ESRCH without a conflicting birth, id no longer has a native record.
            }
            1 if value.identity == *id && value.recheck == *id => {
                // When: value and its recheck match id, allowing terminal-state classification for that exact birth.
                if value.status != libc::SZOMB {
                    // When: value is not a zombie, so an in-exit member still counts as live rather than settled.
                    if value.sid < 0 && !(value.in_exit && value.sid_errno == libc::ESRCH) {
                        // When: value.sid failed outside the known in-exit ESRCH case, so readable process state cannot resolve session loss.
                        return Verdict::Unknown;
                    }
                    live = true;
                }
            }
            _ => {
                // When: value.state or birth cannot match id, fail closed instead of guessing whether it exited.
                return Verdict::Unknown;
            }
        }
    }
    if live {
        // Disconnected output alone cannot establish settlement while a member remains live.
        Verdict::Persistent
    } else if disconnected {
        // When: disconnected confirms EOF after every retained birth became terminal or gone.
        Verdict::SettledLate
    } else {
        // When: disconnected is false, terminal process observations still do not prove the output population is frozen.
        Verdict::NoEof
    }
}

pub(super) fn admits_chunk(len: usize, capacity: usize, bytes: usize, next: usize) -> bool {
    len < capacity.min(MAX_CHUNKS)
        && bytes.checked_add(next).is_some_and(|total| total <= MAX_PAYLOAD)
}

/// Parse only the bounded final survivor list, never PID-shaped text elsewhere in an error.
pub(super) fn failed_member_pids(error: &str) -> Result<Vec<u32>, &'static str> {
    if error.len() > MAX_BYTES || error.contains(['\n', '\r']) {
        // When: error exceeds the framing bounds, no partial survivor list can identify the observed population.
        return Err("unbounded or multiline failure");
    }
    let body = error
        .strip_prefix("PTY session still has live descendants after termination attempts: [")
        .ok_or("unrecognized termination failure")?;
    let (members, group) = body.rsplit_once("]; group_kill=").ok_or("missing group outcome")?;
    if !matches!(group, "ok" | "EPERM" | "ESRCH" | "EINVAL" | "refused") {
        // When: matches! rejects the group outcome, so uncertain error framing cannot nominate a survivor PID.
        return Err("unrecognized group outcome");
    }
    let mut pids = Vec::with_capacity(MAX_IDENTITIES);
    for member in members.split(", ") {
        let (pid, rest) = member
            .strip_prefix("pid=")
            .and_then(|value| value.split_once(' '))
            .ok_or("incomplete survivor identity")?;
        if pid.is_empty()
            || !pid.bytes().all(|byte| byte.is_ascii_digit())
            || !rest.contains("state=")
            || !rest.contains(" kill=")
        {
            // When: pid or rest lacks the required fields, the error cannot bind a native survivor observation.
            return Err("invalid survivor fields");
        }
        let pid: u32 = pid.parse().map_err(|_| "invalid survivor pid")?;
        if pid == 0
            || pid > i32::MAX as u32
            || pids.contains(&pid)
            || pids.len() >= MAX_IDENTITIES - 1
        {
            // When: pid is invalid or pids cannot retain it uniquely, refuse before losing population identity.
            return Err("duplicate, invalid or excessive survivors");
        }
        pids.push(pid);
    }
    Ok(pids)
}

pub(super) struct PostFailure {
    pub(super) named: Vec<u32>,
    pub(super) expected: Vec<Identity>,
    pub(super) initial: Vec<Observation>,
    pub(super) unknown: bool,
}

/// Bind only births observable after refusal; these observations cannot recover pre-signal identity.
pub(super) fn capture_after_failure(
    session: u32,
    error: &str,
    mut sample: impl FnMut(u32) -> Observation,
) -> Result<PostFailure, &'static str> {
    let members = failed_member_pids(error)?;
    if session == 0 || session > i32::MAX as u32 || members.contains(&session) {
        // When: session cannot be the distinct retained leader, the post-return membership boundary is unproven.
        return Err("invalid retained session identity");
    }
    let mut named = Vec::with_capacity(MAX_IDENTITIES);
    named.push(session);
    named.extend(members);
    let mut captured = PostFailure {
        named,
        expected: Vec::with_capacity(MAX_IDENTITIES),
        initial: Vec::with_capacity(MAX_IDENTITIES),
        unknown: false,
    };
    for &pid in &captured.named {
        let observation = sample(pid);
        let valid_birth = observation.state == 1
            && observation.identity.pid == pid
            && observation.identity.known()
            && observation.identity == observation.recheck;
        let same_session = observation.sid == session as i32
            || (pid == session
                && observation.pgid == session
                && (observation.status == libc::SZOMB
                    || (observation.in_exit && observation.sid_errno == libc::ESRCH)));
        captured.unknown |= !valid_birth || !same_session;
        if valid_birth {
            captured.expected.push(observation.identity);
        }
        captured.initial.push(observation);
    }
    Ok(captured)
}

#[cfg(test)]
#[path = "pty_termination_probe_tests.rs"]
mod pty_termination_probe_tests;
