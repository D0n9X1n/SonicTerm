//! Private, bounded observations of PTY termination; never signals or reaps.

// The library uses the recorder while the integration binary uses the reader from this shared module.
#![allow(dead_code)]

use std::{
    cell::RefCell,
    fs::{self, Metadata, OpenOptions},
    io::{self, Read, Write},
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::{Instant, SystemTime, UNIX_EPOCH},
};

pub(super) const MAX_BYTES: usize = 1 << 20;
pub(super) const MAX_RECORDS: usize = 4096;
pub(super) const MAX_IDENTITIES: usize = 256;
pub(super) const MAX_CHUNKS: usize = 256;
pub(super) const MAX_PAYLOAD: usize = 16 << 20;
pub(super) const ENV: &str = "SONICTERM_PTY_TERMINATION_PROBE_DIR";
static SEQUENCE: AtomicU64 = AtomicU64::new(0);

// The integration fixture aliases this lock so probe allocations cannot enter its heap measurement.
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

#[derive(Clone, Copy, Debug)]
pub(super) struct Record {
    pub(super) kind: u8,
    pub(super) elapsed: u128,
    pub(super) result: i32,
    pub(super) included: bool,
    pub(super) observation: Observation,
    pub(super) raw_sid: i32,
    pub(super) raw_sid_errno: i32,
    pub(super) active: i8,
    pub(super) active_errno: i32,
    pub(super) enumerated: u32,
    pub(super) skipped: u32,
}

fn wall_now() -> u128 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |time| time.as_nanos())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct DirectoryIdentity {
    dev: u64,
    ino: u64,
    mode: u32,
    uid: u32,
}

fn directory_identity(path: &Path) -> io::Result<DirectoryIdentity> {
    let metadata = fs::symlink_metadata(path)?;
    if !path.is_absolute() || !metadata.is_dir() || metadata.file_type().is_symlink() {
        // When: path is not an absolute real directory, so it cannot own this trace transport.
        return Err(invalid());
    }
    Ok(DirectoryIdentity {
        dev: metadata.dev(),
        ino: metadata.ino(),
        mode: metadata.mode(),
        uid: metadata.uid(),
    })
}

pub(super) struct Probe {
    directory: PathBuf,
    directory_identity: Option<DirectoryIdentity>,
    session: u32,
    sequence: u64,
    origin: Instant,
    wall_start: u128,
    group_before_wall: u128,
    group_before_elapsed: u128,
    group_after_wall: u128,
    group_after_elapsed: u128,
    records: Vec<Record>,
    seen: Vec<u32>,
    overflow: bool,
}

impl Probe {
    /// Allocate all pass-record storage before the original group signal.
    // Ordering: SEQUENCE only names exclusive trace files; Relaxed publishes no process state.
    pub(super) fn start(session: u32) -> Option<RefCell<Self>> {
        let directory = std::env::var_os(ENV).map(PathBuf::from)?;
        let identity = directory_identity(&directory).ok();
        let sequence = SEQUENCE.fetch_add(1, Ordering::Relaxed);
        Some(RefCell::new(Self {
            directory,
            directory_identity: identity,
            session,
            sequence,
            origin: Instant::now(),
            wall_start: wall_now(),
            group_before_wall: 0,
            group_before_elapsed: 0,
            group_after_wall: 0,
            group_after_elapsed: 0,
            records: Vec::with_capacity(MAX_RECORDS),
            seen: Vec::with_capacity(MAX_IDENTITIES),
            overflow: false,
        }))
    }

    pub(super) fn before_group(&mut self) {
        self.group_before_wall = wall_now();
        self.group_before_elapsed = self.origin.elapsed().as_nanos();
    }

    pub(super) fn after_group(&mut self) {
        self.group_after_elapsed = self.origin.elapsed().as_nanos();
        self.group_after_wall = wall_now();
    }

    pub(super) fn seen(&self, pid: u32) -> bool {
        pid == self.session || self.seen.contains(&pid)
    }

    fn push(&mut self, record: Record) {
        if self.records.len() >= MAX_RECORDS {
            // When: records reached MAX_RECORDS, so latch overflow without allocating or affecting native termination.
            self.overflow = true;
            return;
        }
        let pid = record.observation.identity.pid;
        if !self.seen.contains(&pid) {
            // Retain first-seen PIDs for observations after session membership disappears.
            if self.seen.len() == MAX_IDENTITIES {
                // A full seen buffer makes subsequent membership evidence incomplete.
                self.overflow = true;
            } else {
                // When: seen has capacity for pid, retain it without growing the preallocated buffer.
                self.seen.push(pid);
            }
        }
        self.records.push(record);
    }

    fn record(
        &mut self,
        kind: u8,
        observation: Observation,
        result: i32,
        included: bool,
    ) -> Record {
        Record {
            kind,
            elapsed: self.origin.elapsed().as_nanos(),
            result,
            included,
            observation,
            raw_sid: -1,
            raw_sid_errno: 0,
            active: -1,
            active_errno: 0,
            enumerated: 0,
            skipped: 0,
        }
    }

    pub(super) fn note(&mut self, kind: u8, pid: u32, result: i32, included: bool) {
        if self.records.len() == MAX_RECORDS {
            // When: records is full, latch overflow before any additional native observation.
            self.overflow = true;
            return;
        }
        let record = self.record(kind, observe(pid), result, included);
        self.push(record);
    }

    pub(super) fn candidate(
        &mut self,
        pid: u32,
        raw_sid: i32,
        raw_errno: i32,
        active: i8,
        active_errno: i32,
        included: bool,
    ) {
        if self.records.len() == MAX_RECORDS {
            // When: records is full, latch overflow before any additional native observation.
            self.overflow = true;
            return;
        }
        let value = if active == -2 {
            // Preserve active_errno without another native query after the failed production check.
            Observation::absent(pid, 2, active_errno)
        } else {
            // When: active did not fail, observe pid to bind the recorded membership decision to its birth.
            observe(pid)
        };
        let mut record = self.record(3, value, 0, included);
        record.raw_sid = raw_sid;
        record.raw_sid_errno = raw_errno;
        record.active = active;
        record.active_errno = active_errno;
        self.push(record);
    }

    pub(super) fn scan_end(&mut self, enumerated: usize, skipped: usize, error: i32) {
        if self.records.len() == MAX_RECORDS {
            // When: records is full, latch overflow before any additional native observation.
            self.overflow = true;
            return;
        }
        let value = if error == 0 {
            // Observe the excluded leader only after a successful scan.
            observe(self.session)
        } else {
            // When: error ends the scan, retain unknown leader state without adding another native query.
            Observation::absent(self.session, 2, error)
        };
        let mut record = self.record(4, value, error, false);
        record.enumerated = u32::try_from(enumerated).unwrap_or(u32::MAX);
        record.skipped = u32::try_from(skipped).unwrap_or(u32::MAX);
        self.push(record);
    }

    pub(super) fn finish(self, result: i32) {
        if let Err(error) = self.write(result) {
            // When: write(result) failed, so report evidence failure after termination instead of claiming a valid trace.
            let _ = writeln!(
                io::stderr(),
                "PTY_TERMINATION_PROBE_WRITE_ERROR session={} sequence={} error={error:?}",
                self.session,
                self.sequence
            );
        }
    }

    fn write(&self, result: i32) -> io::Result<()> {
        if Some(directory_identity(&self.directory)?) != self.directory_identity {
            // When: directory_identity changed, trace custody no longer belongs to the captured scratch root.
            return Err(invalid());
        }
        let mut data = LimitedOutput::new(MAX_BYTES);
        writeln!(
            &mut data,
            "PTYTERM\t2\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
            std::process::id(),
            self.session,
            self.sequence,
            self.wall_start,
            self.group_before_wall,
            self.group_before_elapsed,
            self.group_after_wall,
            self.group_after_elapsed
        )?;
        for record in &self.records {
            encode_record(&mut data, record)?;
        }
        writeln!(
            &mut data,
            "END\t{}\t{}\t{}",
            self.records.len(),
            u8::from(self.overflow),
            result
        )?;
        parse(&data.bytes)?;
        let path = self.directory.join(format!(
            "ptyterm-{}-{}-{}.tsv",
            std::process::id(),
            self.session,
            self.sequence
        ));
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(path)?;
        file.write_all(&data.bytes)?;
        if Some(directory_identity(&self.directory)?) != self.directory_identity {
            // When: directory_identity changed, trace custody no longer belongs to the captured scratch root.
            return Err(invalid());
        }
        Ok(())
    }
}

struct LimitedOutput {
    bytes: Vec<u8>,
    limit: usize,
}
impl LimitedOutput {
    fn new(limit: usize) -> Self {
        Self { bytes: Vec::with_capacity(limit), limit }
    }
}
impl Write for LimitedOutput {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if !self.bytes.len().checked_add(bytes.len()).is_some_and(|len| len <= self.limit) {
            // When: bytes would exceed limit or overflow the length, so refuse before the buffer could allocate.
            return Err(invalid());
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn encode_record(output: &mut impl Write, r: &Record) -> io::Result<()> {
    let o = r.observation;
    write!(
        output,
        "R\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t",
        r.kind,
        r.elapsed,
        r.result,
        u8::from(r.included),
        o.state,
        o.identity.pid,
        o.identity.seconds,
        o.identity.micros,
        o.recheck.seconds,
        o.recheck.micros,
        o.ppid,
        o.pgid,
        o.sid,
        o.sid_errno,
        o.status,
        u8::from(o.in_exit),
        o.errno
    )?;
    for byte in o.command {
        write!(output, "{byte:02x}")?;
    }
    writeln!(
        output,
        "\t{}\t{}\t{}\t{}\t{}\t{}",
        r.raw_sid, r.raw_sid_errno, r.active, r.active_errno, r.enumerated, r.skipped
    )
}

pub(super) struct Trace {
    pub(super) process: u32,
    pub(super) session: u32,
    pub(super) sequence: u64,
    pub(super) wall_start: u128,
    pub(super) group_before_wall: u128,
    pub(super) group_before_elapsed: u128,
    pub(super) group_after_wall: u128,
    pub(super) group_after_elapsed: u128,
    pub(super) records: Vec<Record>,
    pub(super) overflow: bool,
    pub(super) result: i32,
}

fn invalid() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "invalid or incomplete PTY trace")
}
fn number<T: std::str::FromStr>(value: &str) -> io::Result<T> {
    value.parse().map_err(|_| invalid())
}
fn bit(value: &str) -> io::Result<bool> {
    match value {
        "0" => Ok(false),
        "1" => Ok(true),
        _ => Err(invalid()),
    }
}
fn decode_command(value: &str) -> io::Result<[u8; 16]> {
    if value.len() != 32
        || !value.bytes().all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        // When: value is not fixed-width lowercase hex, so it cannot represent the exact command bytes.
        return Err(invalid());
    }
    let mut command = [0; 16];
    for (index, byte) in command.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&value[index * 2..index * 2 + 2], 16).map_err(|_| invalid())?;
    }
    Ok(command)
}

/// Accept only complete bounded records with clock brackets and fixed-width command encoding.
pub(super) fn parse(data: &[u8]) -> io::Result<Trace> {
    if data.len() > MAX_BYTES || !data.ends_with(b"\n") || data.contains(&b'\r') {
        // When: data violates the byte bound or line framing, so no partial record may establish settlement.
        return Err(invalid());
    }
    let text = std::str::from_utf8(data).map_err(|_| invalid())?;
    let mut lines = text.lines();
    let header: Vec<_> = lines.next().ok_or_else(invalid)?.split('\t').take(11).collect();
    if header.len() != 10 || header[..2] != ["PTYTERM", "2"] {
        // When: header is not the exact v2 schema, so its identity and clock fields cannot be interpreted safely.
        return Err(invalid());
    }
    let mut trace = Trace {
        process: number(header[2])?,
        session: number(header[3])?,
        sequence: number(header[4])?,
        wall_start: number(header[5])?,
        group_before_wall: number(header[6])?,
        group_before_elapsed: number(header[7])?,
        group_after_wall: number(header[8])?,
        group_after_elapsed: number(header[9])?,
        records: Vec::with_capacity(MAX_RECORDS),
        overflow: false,
        result: 0,
    };
    if trace.process == 0
        || trace.session == 0
        || trace.wall_start == 0
        || trace.wall_start > trace.group_before_wall
        || trace.group_before_wall > trace.group_after_wall
        || trace.group_before_elapsed > trace.group_after_elapsed
    {
        // When: trace has invalid process identity or reversed clock brackets, so the native call window is unproven.
        return Err(invalid());
    }
    let mut last = 0;
    while let Some(line) = lines.next() {
        let f: Vec<_> = line.split('\t').take(26).collect();
        if f.first() == Some(&"END") {
            // When: f.first() is END, validate its count and terminal framing before accepting the trace.
            if f.len() != 4
                || number::<usize>(f[1])? != trace.records.len()
                || lines.next().is_some()
            {
                // When: END has an invalid count, field count or trailing lines, making the trace incomplete or ambiguous.
                return Err(invalid());
            }
            trace.overflow = bit(f[2])?;
            trace.result = number(f[3])?;
            if !(0..=2).contains(&trace.result) || trace.records.len() < 2 {
                // When: trace.result is unknown or leader records are missing, so the signal boundary cannot be established.
                return Err(invalid());
            }
            let before = &trace.records[0];
            let after = &trace.records[1];
            if before.kind != 0
                || after.kind != 1
                || before.observation.identity.pid != trace.session
                || after.observation.identity.pid != trace.session
                || before.elapsed > trace.group_before_elapsed
                || after.elapsed < trace.group_after_elapsed
            {
                // When: before and after do not bracket the same session signal, so their observations cannot bind this call.
                return Err(invalid());
            }
            return Ok(trace);
        }
        if f.len() != 25 || f[0] != "R" || trace.records.len() == MAX_RECORDS {
            // When: f violates the record schema or record capacity, so refuse before decoding or allocating another entry.
            return Err(invalid());
        }
        let kind = number(f[1])?;
        let elapsed = number(f[2])?;
        let state = number(f[5])?;
        let pid = number(f[6])?;
        if kind > 6
            || (kind < 2 && trace.records.len() != kind as usize)
            || state > 3
            || elapsed < last
            || pid == 0
        {
            // When: kind, state, elapsed or pid breaks record identity/order, so the observation sequence is not trustworthy.
            return Err(invalid());
        }
        last = elapsed;
        let observation = Observation {
            state,
            identity: Identity { pid, seconds: number(f[7])?, micros: number(f[8])? },
            recheck: Identity { pid, seconds: number(f[9])?, micros: number(f[10])? },
            ppid: number(f[11])?,
            pgid: number(f[12])?,
            sid: number(f[13])?,
            sid_errno: number(f[14])?,
            status: number(f[15])?,
            in_exit: bit(f[16])?,
            errno: number(f[17])?,
            command: decode_command(f[18])?,
        };
        let active: i8 = number(f[21])?;
        let enumerated = number(f[23])?;
        let skipped = number(f[24])?;
        if observation.identity.micros >= 1_000_000
            || observation.recheck.micros >= 1_000_000
            || !(-2..=1).contains(&active)
            || skipped > enumerated
        {
            // When: birth microseconds, active or skipped counts are out of range, so reject the malformed observation.
            return Err(invalid());
        }
        trace.records.push(Record {
            kind,
            elapsed,
            result: number(f[3])?,
            included: bit(f[4])?,
            observation,
            raw_sid: number(f[19])?,
            raw_sid_errno: number(f[20])?,
            active,
            active_errno: number(f[22])?,
            enumerated,
            skipped,
        });
    }
    Err(invalid())
}

/// Retain the union of relevant known births, including a birth recovered before a later ESRCH read.
pub(super) fn identities(trace: &Trace) -> (Vec<Identity>, bool) {
    let mut identities: Vec<Identity> = Vec::with_capacity(MAX_IDENTITIES);
    let mut unknown = trace.overflow;
    for record in &trace.records {
        let value = record.observation;
        for id in [value.identity, value.recheck] {
            if !id.known() {
                // When: id has no complete birth identity, so it cannot enter the retained identity union.
                continue;
            }
            if identities.iter().any(|old| old.pid == id.pid && *old != id) {
                // Conflicting births cannot prove settlement of one retained process.
                unknown = true;
            }
            if !identities.contains(&id) {
                // Retain births even when later session queries omit their PIDs.
                if identities.len() == MAX_IDENTITIES {
                    // A full identities buffer cannot represent complete membership evidence.
                    unknown = true;
                } else {
                    // When: identities has spare capacity, record id without growing its reserved allocation.
                    identities.push(id);
                }
            }
        }
        unknown |= matches!(value.state, 2 | 3)
            || (record.kind == 4 && record.result != 0)
            || record.active == -2;
        if value.state == 0 {
            // Gone needs ESRCH and an already known birth before it can support absence.
            unknown |= value.errno != libc::ESRCH
                || !identities.iter().any(|id| id.pid == value.identity.pid);
        } else {
            // When: value is not Gone, require a known birth and consistent recheck for a readable observation.
            unknown |=
                !value.identity.known() || (value.state == 1 && value.identity != value.recheck);
        }
        if value.state == 1
            && value.status != libc::SZOMB
            && value.sid < 0
            && !(value.in_exit && value.sid_errno == libc::ESRCH)
        {
            // Unexplained live-session loss remains unknown rather than proof of absence.
            unknown = true;
        }
    }
    (identities, unknown)
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

#[derive(PartialEq, Eq)]
struct FileIdentity {
    dev: u64,
    ino: u64,
    mode: u32,
    links: u64,
    len: u64,
    modified: (i64, i64),
    changed: (i64, i64),
}
fn regular(metadata: &Metadata) -> io::Result<FileIdentity> {
    if !metadata.is_file() || metadata.nlink() != 1 || metadata.len() > MAX_BYTES as u64 {
        // When: metadata is not bounded single-link regular storage, so opening it could block or follow shared evidence.
        return Err(invalid());
    }
    Ok(FileIdentity {
        dev: metadata.dev(),
        ino: metadata.ino(),
        mode: metadata.mode(),
        links: metadata.nlink(),
        len: metadata.len(),
        modified: (metadata.mtime(), metadata.mtime_nsec()),
        changed: (metadata.ctime(), metadata.ctime_nsec()),
    })
}

pub(super) struct Ticket {
    directory: PathBuf,
    root_identity: DirectoryIdentity,
    session: u32,
    leader: Observation,
    before: Vec<PathBuf>,
    captured_wall: u128,
}

fn files(directory: &Path, session: u32) -> io::Result<Vec<PathBuf>> {
    let mut result = Vec::with_capacity(MAX_IDENTITIES);
    let prefix = format!("ptyterm-{}-{session}-", std::process::id());
    directory_identity(directory)?;
    for (index, entry) in fs::read_dir(directory)?.enumerate() {
        if index == MAX_RECORDS {
            // When: directory enumeration reached MAX_RECORDS, so an incomplete filename census cannot prove one fresh trace.
            return Err(invalid());
        }
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_str().ok_or_else(invalid)?;
        if name.starts_with(&prefix) {
            // When: name matches this process/session prefix, so validate its sequence and file custody before matching the call.
            let sequence = name
                .strip_prefix(&prefix)
                .and_then(|value| value.strip_suffix(".tsv"))
                .ok_or_else(invalid)?;
            let _: u64 = number(sequence)?;
            regular(&fs::symlink_metadata(entry.path())?)?;
            if result.len() == MAX_IDENTITIES {
                // When: result reached MAX_IDENTITIES, so further matching paths make this bounded census incomplete.
                return Err(invalid());
            }
            result.push(entry.path());
        }
    }
    Ok(result)
}

impl Ticket {
    /// Bind one subsequent trace to an unchanged scratch root, retained leader birth and call window.
    pub(super) fn capture(session: u32) -> io::Result<Option<Self>> {
        let Some(directory) = std::env::var_os(ENV).map(PathBuf::from) else {
            // When: ENV is absent, no diagnostic transport is requested and native teardown remains unobserved.
            return Ok(None);
        };
        let root_identity = directory_identity(&directory)?;
        let before = files(&directory, session)?;
        let leader = observe(session);
        if leader.state != 1 || !leader.identity.known() || leader.identity != leader.recheck {
            // When: leader lacks one stable readable birth, so a later file cannot be bound to the retained child.
            return Err(invalid());
        }
        Ok(Some(Self {
            directory,
            root_identity,
            session,
            leader,
            before,
            captured_wall: wall_now(),
        }))
    }

    pub(super) fn load(&self) -> io::Result<Trace> {
        if directory_identity(&self.directory)? != self.root_identity {
            // When: directory_identity no longer matches root_identity, so the ticket cannot trust paths under that root.
            return Err(invalid());
        }
        let added: Vec<_> = files(&self.directory, self.session)?
            .into_iter()
            .filter(|path| !self.before.contains(path))
            .collect();
        if added.len() != 1 {
            // When: added has no unique fresh file, so choosing a trace would confuse separate termination calls.
            return Err(invalid());
        }
        let identity = regular(&fs::symlink_metadata(&added[0])?)?;
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(&added[0])?;
        if regular(&file.metadata()?)? != identity {
            // When: the opened file differs from identity, so path replacement invalidates transport custody.
            return Err(invalid());
        }
        let mut data = Vec::with_capacity(MAX_BYTES + 1);
        let mut reader = (&file).take((MAX_BYTES + 1) as u64);
        reader.read_to_end(&mut data)?;
        if regular(&file.metadata()?)? != identity
            || regular(&fs::symlink_metadata(&added[0])?)? != identity
            || data.len() as u64 != identity.len
            || directory_identity(&self.directory)? != self.root_identity
        {
            // When: file, path, length or root changed during the read, so those bytes cannot authenticate this ticket.
            return Err(invalid());
        }
        let trace = parse(&data)?;
        let first = trace.records.first().ok_or_else(invalid)?;
        let name = format!("ptyterm-{}-{}-{}.tsv", trace.process, trace.session, trace.sequence);
        if added[0].file_name().and_then(|value| value.to_str()) != Some(name.as_str())
            || trace.process != std::process::id()
            || trace.session != self.session
            || trace.result != 1
            || first.observation.state != 1
            || first.observation.identity != self.leader.identity
            || first.observation.recheck != self.leader.identity
            || self.captured_wall == 0
            || trace.wall_start < self.captured_wall
            || trace.group_after_wall > wall_now()
        {
            // When: trace identity, result or call window disagrees with the ticket, so it is not evidence for this failed kill.
            return Err(invalid());
        }
        Ok(trace)
    }
}

#[cfg(test)]
#[path = "pty_termination_probe_tests.rs"]
mod pty_termination_probe_tests;
