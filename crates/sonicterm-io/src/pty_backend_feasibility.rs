//! Frozen PTY/native backend feasibility decision (`pty-backend-v1`).
//!
//! This module is the WP-PTY-FEAS decision artifact: it compares the current
//! `portable-pty` seam against the ownership and cancellation surface the
//! v1.2.0 resource/lifecycle architecture requires (`PtyTransportOwner`,
//! level-triggered cancellation, bounded reaper handoff, exact process-tree
//! ownership), records the chosen approach, and enumerates the exact Windows,
//! Unix, and SSH requirements that flow into the production WP-PTY package.
//!
//! It is a research/decision artifact only — no production backend is spawned
//! here. The frozen bytes are rendered deterministically by
//! [`render_canonical_evidence`] and pinned by [`FROZEN_EVIDENCE_SHA256`], so
//! the owner can record a stable hash in the coordination ledger. The flat
//! script `scripts/pty-backend-feasibility.sh` reproduces the same bytes and
//! hashes them with the system hasher (Python `hashlib`), keeping the freeze
//! reproducible without a crypto dependency in the crate. Every comparison row
//! is grounded in the observed `portable-pty` 0.9 trait surface (`MasterPty`
//! exposes no Windows handle accessor and no `HPCON`/job seam; `WinChild::kill`
//! terminates only the direct child), not in a fake.

/// Schema version for the frozen evidence document. Bump only with an
/// owner-approved decision; the pinned hash changes with it.
pub const EVIDENCE_SCHEMA_VERSION: &str = "pty-backend-v1";

/// Exact staging base SHA this decision was frozen against (WP-PTY-FEAS row in
/// the canonical ledger). Kept in the hashed body so the freeze is tied to a
/// concrete tree.
pub const FROZEN_BASE_SHA: &str = "ff8cb0b6";

/// The three approaches the feasibility decision must choose between.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BackendApproach {
    /// Extend `portable-pty` upstream and depend on the released crate.
    UpstreamExtension,
    /// Maintain a fork of `portable-pty` inside the workspace.
    MaintainedFork,
    /// Sonic-owned native transport behind `sonicterm-io`, no `portable-pty`
    /// on the local-PTY teardown path.
    SonicOwnedNative,
}

impl BackendApproach {
    /// Stable token used in the hashed evidence body.
    #[must_use]
    pub const fn token(self) -> &'static str {
        match self {
            Self::UpstreamExtension => "upstream-extension",
            Self::MaintainedFork => "maintained-fork",
            Self::SonicOwnedNative => "sonic-owned-native",
        }
    }
}

/// The frozen `pty-backend-v1` choice for both Windows and Unix local PTYs.
pub const DECISION: BackendApproach = BackendApproach::SonicOwnedNative;

/// One-line justification kept in the hashed body.
pub const DECISION_RATIONALE: &str = concat!(
    "portable-pty 0.9 MasterPty exposes no HPCON/handle accessor and no job-object seam; ",
    "WinChild::kill terminates only the direct child, and HPCON closes solely by private ",
    "drop order. The required PtyTransportOwner surface (own HPCON, close input/output/",
    "terminal independently, cancel synchronous IO before join, process-tree ownership) ",
    "cannot be met through the released seam. Upstream extension is out of Sonic's control ",
    "and WezTerm itself does not route ConPTY ownership through portable-pty; a fork ",
    "reintroduces a vendored dependency the workspace forbids. Sonic-owned native backends ",
    "are the only approach that satisfies every ownership/cancellation invariant.",
);

/// A capability the v1.2.0 `PtyTransportOwner`/lifecycle contract requires from
/// the local-PTY transport seam.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Capability {
    /// Own the ConPTY `HPCON` and sequence `ClosePseudoConsole` explicitly.
    WinOwnHpcon,
    /// Close the child input pipe end independently of output/terminal.
    WinCloseInputPipe,
    /// Drain or keep output open while closing the pseudoconsole (the
    /// pre-24H2 `ClosePseudoConsole` contract).
    WinDrainDuringClose,
    /// Own a job object so the whole process tree can be terminated.
    WinJobObjectTree,
    /// Cancel a thread's pending synchronous console IO before join.
    WinCancelSyncIo,
    /// Own the Unix master fd and close it to interrupt blocked reads/writes.
    UnixOwnMasterFd,
    /// Establish and own a session/process-group identity at spawn.
    UnixSessionIdentity,
    /// Signal descendants before the leader pid/pgid becomes reusable.
    UnixSignalGroupBeforeReuse,
    /// Reap the direct child without losing descendant-group cleanup.
    UnixReapWithoutLosingGroup,
    /// Provide an interruptible duplex transport for the mux seam.
    InterruptibleMuxTransport,
}

impl Capability {
    /// Stable token for the hashed matrix.
    #[must_use]
    pub const fn token(self) -> &'static str {
        match self {
            Self::WinOwnHpcon => "win.own_hpcon",
            Self::WinCloseInputPipe => "win.close_input_pipe",
            Self::WinDrainDuringClose => "win.drain_during_close",
            Self::WinJobObjectTree => "win.job_object_tree",
            Self::WinCancelSyncIo => "win.cancel_sync_io",
            Self::UnixOwnMasterFd => "unix.own_master_fd",
            Self::UnixSessionIdentity => "unix.session_identity",
            Self::UnixSignalGroupBeforeReuse => "unix.signal_group_before_reuse",
            Self::UnixReapWithoutLosingGroup => "unix.reap_without_losing_group",
            Self::InterruptibleMuxTransport => "mux.interruptible_transport",
        }
    }
}

/// How well a seam supports a capability.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SeamSupport {
    /// The capability is reachable and owned through this seam today.
    Owned,
    /// Partially reachable: works but not with the ownership/ordering the
    /// contract requires (e.g. drop-order-only, direct child only).
    Partial,
    /// Not reachable through the public seam at all.
    Missing,
}

impl SeamSupport {
    /// Stable token for the hashed matrix.
    #[must_use]
    pub const fn token(self) -> &'static str {
        match self {
            Self::Owned => "owned",
            Self::Partial => "partial",
            Self::Missing => "missing",
        }
    }
}

/// One row comparing `portable-pty` against a Sonic-owned native backend for a
/// single required capability.
#[derive(Clone, Copy, Debug)]
pub struct CapabilityRow {
    /// The required capability.
    pub capability: Capability,
    /// Support offered by the current `portable-pty` 0.9 seam.
    pub portable_pty: SeamSupport,
    /// Support offered by a Sonic-owned native backend.
    pub sonic_native: SeamSupport,
    /// Grounded evidence note (observed trait surface, not a guess).
    pub evidence: &'static str,
}

/// The objective capability comparison. Every `portable_pty` verdict is
/// grounded in the observed 0.9 trait surface.
pub const CAPABILITY_MATRIX: &[CapabilityRow] = &[
    CapabilityRow {
        capability: Capability::WinOwnHpcon,
        portable_pty: SeamSupport::Missing,
        sonic_native: SeamSupport::Owned,
        evidence: "ConPtyMasterPty exposes no as_raw_handle/HPCON; HPCON lives in private Inner and closes only via PsuedoCon Drop.",
    },
    CapabilityRow {
        capability: Capability::WinCloseInputPipe,
        portable_pty: SeamSupport::Partial,
        sonic_native: SeamSupport::Owned,
        evidence: "take_writer yields a boxed Write whose drop closes input, but input/output/terminal cannot be closed independently in a defined order.",
    },
    CapabilityRow {
        capability: Capability::WinDrainDuringClose,
        portable_pty: SeamSupport::Partial,
        sonic_native: SeamSupport::Owned,
        evidence: "Sonic already spawns a drain thread around the boxed reader, but close timing is drop-order-bound because HPCON is not owned.",
    },
    CapabilityRow {
        capability: Capability::WinJobObjectTree,
        portable_pty: SeamSupport::Missing,
        sonic_native: SeamSupport::Owned,
        evidence: "WinChild::kill calls TerminateProcess on the direct child handle only; no job object is created, so descendants survive.",
    },
    CapabilityRow {
        capability: Capability::WinCancelSyncIo,
        portable_pty: SeamSupport::Owned,
        sonic_native: SeamSupport::Owned,
        evidence: "Cancellation targets the Sonic-owned reader/writer JoinHandle via CancelSynchronousIo and is independent of the backend.",
    },
    CapabilityRow {
        capability: Capability::UnixOwnMasterFd,
        portable_pty: SeamSupport::Partial,
        sonic_native: SeamSupport::Owned,
        evidence: "MasterPty::as_raw_fd borrows the fd but portable-pty owns its lifetime; closing to interrupt IO depends on dropping the master.",
    },
    CapabilityRow {
        capability: Capability::UnixSessionIdentity,
        portable_pty: SeamSupport::Partial,
        sonic_native: SeamSupport::Owned,
        evidence: "openpty child calls setsid; process_group_leader/process_id expose the sid, but Sonic must cache pid-as-sid rather than own a live handle.",
    },
    CapabilityRow {
        capability: Capability::UnixSignalGroupBeforeReuse,
        portable_pty: SeamSupport::Partial,
        sonic_native: SeamSupport::Owned,
        evidence: "Sonic wraps ChildState with waitid(WNOWAIT) to keep the pid reserved, working around Child::try_wait/kill reaping the leader early.",
    },
    CapabilityRow {
        capability: Capability::UnixReapWithoutLosingGroup,
        portable_pty: SeamSupport::Partial,
        sonic_native: SeamSupport::Owned,
        evidence: "Achievable only by not calling Child::wait and re-scanning the session; a native fd/pidfd owner removes the reap/identity race.",
    },
    CapabilityRow {
        capability: Capability::InterruptibleMuxTransport,
        portable_pty: SeamSupport::Missing,
        sonic_native: SeamSupport::Owned,
        evidence: "portable-pty has no mux/socket seam; the interruptible duplex transport is Sonic-owned regardless of the local-PTY backend.",
    },
];

/// A Win32 `windows`-crate feature the native ConPTY backend requires.
#[derive(Clone, Copy, Debug)]
pub struct WinFeatureRequirement {
    /// `windows` crate feature name.
    pub feature: &'static str,
    /// Whether it is already enabled in the workspace `windows` dependency at
    /// the frozen base SHA.
    pub already_enabled: bool,
    /// Why the native backend needs it.
    pub reason: &'static str,
}

/// Exact `windows`-crate features the Sonic-owned ConPTY backend needs. The
/// `already_enabled` flags reflect the workspace manifest at the frozen base
/// SHA (Console, Threading, IO, Foundation are present; Pipes, JobObjects,
/// Security must be added by the integrator during WP-PTY).
pub const WIN_FEATURE_REQUIREMENTS: &[WinFeatureRequirement] = &[
    WinFeatureRequirement {
        feature: "Win32_System_Console",
        already_enabled: true,
        reason: "CreatePseudoConsole/ResizePseudoConsole/ClosePseudoConsole and console modes.",
    },
    WinFeatureRequirement {
        feature: "Win32_System_Threading",
        already_enabled: true,
        reason: "CreateProcessW, InitializeProcThreadAttributeList, PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE.",
    },
    WinFeatureRequirement {
        feature: "Win32_System_IO",
        already_enabled: true,
        reason: "CancelSynchronousIo / CancelIoEx on the owned reader/writer threads.",
    },
    WinFeatureRequirement {
        feature: "Win32_Foundation",
        already_enabled: true,
        reason: "HANDLE, CloseHandle, INVALID_HANDLE_VALUE, WAIT_* return codes.",
    },
    WinFeatureRequirement {
        feature: "Win32_System_Pipes",
        already_enabled: false,
        reason: "CreatePipe for the owned ConPTY input/output pipe ends.",
    },
    WinFeatureRequirement {
        feature: "Win32_System_JobObjects",
        already_enabled: false,
        reason: "CreateJobObjectW/AssignProcessToJobObject/SetInformationJobObject for process-tree ownership and kill-on-close.",
    },
    WinFeatureRequirement {
        feature: "Win32_Security",
        already_enabled: false,
        reason: "SECURITY_ATTRIBUTES + SetHandleInformation to control pipe-handle inheritance.",
    },
];

/// Exact Windows handles the native ConPTY transport owner must hold and close
/// in a defined order (not by opaque field-drop order).
pub const WIN_OWNED_HANDLES: &[&str] = &[
    "HPCON pseudoconsole",
    "child stdin read end (given to child)",
    "input write end (Sonic writer)",
    "output read end (Sonic reader)",
    "child stdout write end (given to child)",
    "PROCESS_INFORMATION.hProcess",
    "PROCESS_INFORMATION.hThread",
    "job object handle",
    "reader thread JoinHandle",
    "writer thread JoinHandle",
];

/// Exact Unix session/FD requirements for the native backend.
pub const UNIX_REQUIREMENTS: &[&str] = &[
    "own the master fd for its full lifetime; close it to interrupt blocked read/write",
    "child setsid() + TIOCSCTTY to become session leader with the pty as controlling terminal",
    "capture session/process-group identity at spawn; treat cached pid/pgid as metadata only",
    "signal the process group (negative pgid) before the leader pid/pgid can be reused",
    "reap the direct child (waitpid) without blocking descendant cleanup; re-scan the session",
    "prefer a live OS identity (pidfd on Linux) over a cached numeric leader where available",
];

/// SSH inclusion/exclusion disposition for `RemoteInput`/`RemoteOutput`.
pub const SSH_DISPOSITION: &str = concat!(
    "INCLUDED but independent of the local-PTY backend choice. The russh backend (feature=\"ssh\") ",
    "is already Sonic-owned pure Rust; it needs no HPCON/ConPTY/job/session work. WP-PTY accounts ",
    "RemoteInput/RemoteOutput on the same byte-bounded queues and must replace the current unbounded ",
    "crossbeam channels and the accept-all host-key check. If a platform build ships without the ssh ",
    "feature, that is the tested explicit exclusion for that build.",
);

/// Cargo/CI/helper changes the production WP-PTY package will need.
pub const CARGO_CI_REQUIREMENTS: &[&str] = &[
    "sonicterm-io: add windows features Win32_System_Pipes, Win32_System_JobObjects, Win32_Security (integrator-applied)",
    "sonicterm-io: drop portable-pty from the local-PTY teardown path once the native backend lands (WP-PTY, not WP-PTY-FEAS)",
    "CI: add a real pre-24H2 Windows job (windows-2022 / Server 2019 image) alongside the current windows-latest runner",
    "CI: keep the injectable pre-24H2 close test in the default cross-platform gate so 24H2-only CI still exercises it",
    "no separate helper binary required for v1; the bounded reaper lives in-process per WP-LIFECYCLE",
];

/// A required piece of platform evidence and whether this artifact can capture
/// it on the host running the WP-PTY-FEAS gate.
#[derive(Clone, Copy, Debug)]
pub struct EvidenceRow {
    /// Host class the row must be captured on.
    pub host_class: &'static str,
    /// What must be observed.
    pub observation: &'static str,
    /// `true` when a sibling probe test captures it on that host class in this
    /// package; `false` when a real runner is still required (WP-PLATFORM).
    pub capturable_here: bool,
}

/// The real-OS evidence matrix. `capturable_here == false` rows are the
/// outstanding real-platform runs the owner still needs before accepting.
pub const EVIDENCE_MATRIX: &[EvidenceRow] = &[
    EvidenceRow {
        host_class: "macos",
        observation: "portable-pty master exposes as_raw_fd and process_group_leader for a live child (Partial ownership confirmed).",
        capturable_here: cfg!(target_os = "macos"),
    },
    EvidenceRow {
        host_class: "linux",
        observation: "same Unix probe plus pidfd_open availability for live-identity supervision.",
        capturable_here: cfg!(target_os = "linux"),
    },
    EvidenceRow {
        host_class: "windows-24h2-plus",
        observation: "ConPTY/CancelSynchronousIo symbols compile against the workspace windows features; native drain/close runs green.",
        capturable_here: false,
    },
    EvidenceRow {
        host_class: "windows-pre-24h2",
        observation: "real ClosePseudoConsole blocks until output drains; Sonic drain-before-close ordering settles the child.",
        capturable_here: false,
    },
];

/// Injectable model of the platform `ClosePseudoConsole` contract so the
/// pre-24H2 close semantics can be exercised on any host (24H2+ CI included).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ConPtyCloseSemantics {
    /// Windows < 11 24H2: `ClosePseudoConsole` blocks until buffered output is
    /// drained; closing without a live drainer deadlocks.
    Pre24H2Blocking,
    /// Windows >= 11 24H2: `ClosePseudoConsole` returns without requiring a
    /// concurrent drainer.
    Post24H2NonBlocking,
}

/// Outcome of driving the teardown close sequence against injected semantics.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CloseOutcome {
    /// Close completed and the pseudoconsole is closed.
    Completed,
    /// Close would block forever (pre-24H2 with no concurrent drainer).
    WouldDeadlock,
}

/// Injectable native-close operations, mirroring the real teardown seam so the
/// pre-24H2 contract is testable without a Windows 10 runner.
pub trait ConPtyCloseOps {
    /// Whether a drain thread is actively consuming output during close.
    fn drainer_active(&self) -> bool;
    /// Whether the output buffer still holds undrained bytes.
    fn output_pending(&self) -> bool;
    /// The platform semantics to model.
    fn semantics(&self) -> ConPtyCloseSemantics;
}

/// Drive the Sonic teardown policy — start a drainer, then close — against the
/// injected semantics and report whether close settles. This is the property a
/// native backend must preserve across both Windows generations.
#[must_use]
pub fn drive_conpty_close<O: ConPtyCloseOps>(ops: &O) -> CloseOutcome {
    match ops.semantics() {
        ConPtyCloseSemantics::Post24H2NonBlocking => CloseOutcome::Completed,
        ConPtyCloseSemantics::Pre24H2Blocking => {
            if ops.output_pending() && !ops.drainer_active() {
                CloseOutcome::WouldDeadlock
            } else {
                CloseOutcome::Completed
            }
        }
    }
}

/// Render the frozen decision as a deterministic, line-oriented document. The
/// exact bytes are hashed into [`FROZEN_EVIDENCE_SHA256`]; the flat script
/// reproduces the same bytes and hash. No host/live data appears here.
#[must_use]
pub fn render_canonical_evidence() -> String {
    let mut out = String::new();
    out.push_str("# pty-backend feasibility — frozen decision\n");
    out.push_str("schema_version: ");
    out.push_str(EVIDENCE_SCHEMA_VERSION);
    out.push('\n');
    out.push_str("base_sha: ");
    out.push_str(FROZEN_BASE_SHA);
    out.push('\n');
    out.push_str("decision: ");
    out.push_str(DECISION.token());
    out.push('\n');
    out.push_str("rationale: ");
    out.push_str(DECISION_RATIONALE);
    out.push('\n');

    out.push_str("\n## capability_matrix\n");
    out.push_str("capability|portable_pty|sonic_native|evidence\n");
    for row in CAPABILITY_MATRIX {
        out.push_str(row.capability.token());
        out.push('|');
        out.push_str(row.portable_pty.token());
        out.push('|');
        out.push_str(row.sonic_native.token());
        out.push('|');
        out.push_str(row.evidence);
        out.push('\n');
    }

    out.push_str("\n## windows_feature_requirements\n");
    for req in WIN_FEATURE_REQUIREMENTS {
        out.push_str(req.feature);
        out.push('|');
        out.push_str(if req.already_enabled { "enabled" } else { "to-add" });
        out.push('|');
        out.push_str(req.reason);
        out.push('\n');
    }

    out.push_str("\n## windows_owned_handles\n");
    for handle in WIN_OWNED_HANDLES {
        out.push_str(handle);
        out.push('\n');
    }

    out.push_str("\n## unix_requirements\n");
    for req in UNIX_REQUIREMENTS {
        out.push_str(req);
        out.push('\n');
    }

    out.push_str("\n## ssh_disposition\n");
    out.push_str(SSH_DISPOSITION);
    out.push('\n');

    out.push_str("\n## cargo_ci_requirements\n");
    for req in CARGO_CI_REQUIREMENTS {
        out.push_str(req);
        out.push('\n');
    }

    out.push_str("\n## evidence_matrix\n");
    out.push_str("host_class|observation\n");
    for row in EVIDENCE_MATRIX {
        out.push_str(row.host_class);
        out.push('|');
        out.push_str(row.observation);
        out.push('\n');
    }

    out
}

/// SHA-256 (lowercase hex) of the exact bytes produced by
/// [`render_canonical_evidence`].
///
/// Frozen so the decision is tamper-evident: any change to the matrix,
/// requirements, or rationale changes these bytes, and
/// `scripts/pty-backend-feasibility.sh --check` fails until the freeze is
/// re-approved and this constant is updated. The script is the authoritative
/// hasher (system Python `hashlib`); the sibling test only asserts this
/// constant is a well-formed, non-placeholder digest so the crate stays
/// crypto-dependency-free.
pub const FROZEN_EVIDENCE_SHA256: &str =
    "7e367c3f573c5d0eaff41eb732059c67b26b5a1e379ab18d5fde0013b2cf699b";

#[cfg(windows)]
#[path = "pty_backend_feasibility_win_probe.rs"]
mod win_probe;

#[cfg(test)]
#[path = "pty_backend_feasibility_tests.rs"]
mod pty_backend_feasibility_tests;
