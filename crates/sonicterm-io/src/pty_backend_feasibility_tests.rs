use super::*;

// ---- Frozen decision ----------------------------------------------------

#[test]
fn decision_is_sonic_owned_native_with_grounded_rationale() {
    assert_eq!(DECISION, BackendApproach::SonicOwnedNative);
    assert_eq!(DECISION.token(), "sonic-owned-native");
    // The rationale must name the concrete blocker, not a vibe.
    assert!(DECISION_RATIONALE.contains("HPCON"));
    assert!(DECISION_RATIONALE.contains("direct child"));
    assert!(DECISION_RATIONALE.len() > 100);
}

#[test]
fn every_backend_approach_has_a_stable_token() {
    assert_eq!(BackendApproach::UpstreamExtension.token(), "upstream-extension");
    assert_eq!(BackendApproach::MaintainedFork.token(), "maintained-fork");
    assert_eq!(BackendApproach::SonicOwnedNative.token(), "sonic-owned-native");
}

// ---- Capability matrix (objective comparison) ---------------------------

const ALL_CAPABILITIES: [Capability; 10] = [
    Capability::WinOwnHpcon,
    Capability::WinCloseInputPipe,
    Capability::WinDrainDuringClose,
    Capability::WinJobObjectTree,
    Capability::WinCancelSyncIo,
    Capability::UnixOwnMasterFd,
    Capability::UnixSessionIdentity,
    Capability::UnixSignalGroupBeforeReuse,
    Capability::UnixReapWithoutLosingGroup,
    Capability::InterruptibleMuxTransport,
];

fn row_for(capability: Capability) -> &'static CapabilityRow {
    CAPABILITY_MATRIX
        .iter()
        .find(|row| row.capability == capability)
        .expect("every capability has exactly one matrix row")
}

#[test]
fn capability_matrix_covers_each_capability_exactly_once() {
    assert_eq!(CAPABILITY_MATRIX.len(), ALL_CAPABILITIES.len());
    for capability in ALL_CAPABILITIES {
        let count = CAPABILITY_MATRIX.iter().filter(|row| row.capability == capability).count();
        assert_eq!(count, 1, "{capability:?} must appear exactly once");
    }
}

#[test]
fn matrix_proves_portable_pty_cannot_meet_the_contract() {
    // The two hard blockers are unreachable through the released seam.
    assert_eq!(row_for(Capability::WinOwnHpcon).portable_pty, SeamSupport::Missing);
    assert_eq!(row_for(Capability::WinJobObjectTree).portable_pty, SeamSupport::Missing);
    // The mux transport is also absent from portable-pty entirely.
    assert_eq!(row_for(Capability::InterruptibleMuxTransport).portable_pty, SeamSupport::Missing);
    // A Sonic-owned native backend owns every required capability.
    assert!(CAPABILITY_MATRIX.iter().all(|row| row.sonic_native == SeamSupport::Owned));
    // portable-pty is never fully Owned across the whole matrix — the choice is decisive.
    assert!(CAPABILITY_MATRIX.iter().any(|row| row.portable_pty != SeamSupport::Owned));
    // Cancellation is the one capability portable-pty already satisfies, because it
    // targets the Sonic-owned IO threads, not the backend.
    assert_eq!(row_for(Capability::WinCancelSyncIo).portable_pty, SeamSupport::Owned);
}

#[test]
fn every_matrix_row_carries_nonempty_evidence() {
    assert!(CAPABILITY_MATRIX.iter().all(|row| !row.evidence.is_empty()));
}

#[test]
fn capability_and_support_tokens_are_unique_and_stable() {
    let mut tokens: Vec<&str> =
        CAPABILITY_MATRIX.iter().map(|row| row.capability.token()).collect();
    let total = tokens.len();
    tokens.sort_unstable();
    tokens.dedup();
    assert_eq!(tokens.len(), total, "capability tokens must be unique for a stable hash");

    assert_eq!(SeamSupport::Owned.token(), "owned");
    assert_eq!(SeamSupport::Partial.token(), "partial");
    assert_eq!(SeamSupport::Missing.token(), "missing");
}

// ---- Injectable pre-24H2 ClosePseudoConsole semantics -------------------

struct FakeClose {
    drainer_active: bool,
    output_pending: bool,
    semantics: ConPtyCloseSemantics,
}

impl ConPtyCloseOps for FakeClose {
    fn drainer_active(&self) -> bool {
        self.drainer_active
    }
    fn output_pending(&self) -> bool {
        self.output_pending
    }
    fn semantics(&self) -> ConPtyCloseSemantics {
        self.semantics
    }
}

#[test]
fn pre_24h2_close_deadlocks_without_a_live_drainer() {
    let ops = FakeClose {
        drainer_active: false,
        output_pending: true,
        semantics: ConPtyCloseSemantics::Pre24H2Blocking,
    };
    assert_eq!(drive_conpty_close(&ops), CloseOutcome::WouldDeadlock);
}

#[test]
fn pre_24h2_close_settles_when_output_is_drained_concurrently() {
    // The Sonic teardown policy runs a drain thread during close.
    let draining = FakeClose {
        drainer_active: true,
        output_pending: true,
        semantics: ConPtyCloseSemantics::Pre24H2Blocking,
    };
    assert_eq!(drive_conpty_close(&draining), CloseOutcome::Completed);
    // Nothing pending is also fine.
    let idle = FakeClose {
        drainer_active: false,
        output_pending: false,
        semantics: ConPtyCloseSemantics::Pre24H2Blocking,
    };
    assert_eq!(drive_conpty_close(&idle), CloseOutcome::Completed);
}

#[test]
fn post_24h2_close_always_completes() {
    for (drainer_active, output_pending) in [(false, false), (false, true), (true, true)] {
        let ops = FakeClose {
            drainer_active,
            output_pending,
            semantics: ConPtyCloseSemantics::Post24H2NonBlocking,
        };
        assert_eq!(drive_conpty_close(&ops), CloseOutcome::Completed);
    }
}

// ---- Requirement tables & pending real-OS evidence ----------------------

#[test]
fn windows_owned_handles_include_hpcon_pipes_and_job() {
    assert!(WIN_OWNED_HANDLES.iter().any(|h| h.contains("HPCON")));
    assert!(WIN_OWNED_HANDLES.iter().any(|h| h.contains("job object")));
    assert!(WIN_OWNED_HANDLES.iter().any(|h| h.contains("write end")));
    assert!(WIN_OWNED_HANDLES.len() >= 8);
}

#[test]
fn unix_requirements_name_session_and_group_identity() {
    assert!(UNIX_REQUIREMENTS.iter().any(|r| r.contains("setsid")));
    assert!(UNIX_REQUIREMENTS.iter().any(|r| r.contains("process group")));
    assert!(UNIX_REQUIREMENTS.iter().any(|r| r.contains("master fd")));
}

#[test]
fn ssh_is_included_and_decoupled_from_the_local_backend() {
    assert!(SSH_DISPOSITION.contains("INCLUDED"));
    assert!(SSH_DISPOSITION.contains("RemoteInput"));
    assert!(SSH_DISPOSITION.contains("RemoteOutput"));
    assert!(SSH_DISPOSITION.contains("russh"));
}

#[test]
fn evidence_matrix_is_scoped_to_supported_platforms() {
    // SonicTerm supports macOS (Intel and Apple Silicon) and current Windows.
    // Linux is out of scope, and pre-24H2 Windows is not a supported target,
    // so neither may reappear as an evidence row.
    let classes: Vec<&str> = EVIDENCE_MATRIX.iter().map(|row| row.host_class).collect();
    assert_eq!(classes, ["macos", "windows-24h2-plus"]);
}

#[test]
fn every_evidence_row_is_captured_by_a_ci_host() {
    // Each row must be capturable on exactly one host class, and the CI matrix
    // (macos-14 + windows-latest) must contain a job that captures it. A row no
    // runner can reach would leave the decision resting on an uncaptured claim.
    for row in EVIDENCE_MATRIX {
        let captured_on_this_host = match row.host_class {
            "macos" => cfg!(target_os = "macos"),
            "windows-24h2-plus" => cfg!(windows),
            other => panic!("unknown host class {other} has no CI job"),
        };
        assert_eq!(
            row.capturable_here, captured_on_this_host,
            "{} must be capturable exactly on its own host class",
            row.host_class
        );
    }
}

#[test]
fn windows_feature_flags_match_the_workspace_manifest() {
    // Ground each `already_enabled` flag in the real workspace manifest, so the
    // enumeration cannot silently drift from what the `windows` dep enables.
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let workspace_toml =
        std::path::Path::new(manifest_dir).join("..").join("..").join("Cargo.toml");
    let text = std::fs::read_to_string(&workspace_toml).expect("read workspace Cargo.toml");
    for req in WIN_FEATURE_REQUIREMENTS {
        let present = text.contains(&format!("\"{}\"", req.feature));
        assert_eq!(
            present, req.already_enabled,
            "windows feature {} present-in-manifest={present} but enumerated already_enabled={}",
            req.feature, req.already_enabled
        );
    }
    // The three to-add features are exactly the process-tree / pipe / security gaps.
    let to_add: Vec<&str> =
        WIN_FEATURE_REQUIREMENTS.iter().filter(|r| !r.already_enabled).map(|r| r.feature).collect();
    assert_eq!(to_add, ["Win32_System_Pipes", "Win32_System_JobObjects", "Win32_Security"]);
}

// ---- Canonical render + frozen hash -------------------------------------

#[test]
fn canonical_evidence_is_byte_deterministic_and_grounded() {
    let first = render_canonical_evidence();
    let second = render_canonical_evidence();
    assert_eq!(first, second, "render must be byte-for-byte deterministic for hashing");
    assert!(first.starts_with("# pty-backend feasibility"));
    assert!(first.contains("decision: sonic-owned-native"));
    assert!(first.contains("base_sha: ff8cb0b6"));
    for row in CAPABILITY_MATRIX {
        assert!(
            first.contains(row.capability.token()),
            "matrix must render {}",
            row.capability.token()
        );
    }
    assert!(first.contains("INCLUDED"));
}

#[test]
fn frozen_hash_is_a_wellformed_nonplaceholder_digest() {
    assert_eq!(FROZEN_EVIDENCE_SHA256.len(), 64, "SHA-256 hex is 64 chars");
    assert!(
        FROZEN_EVIDENCE_SHA256.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
        "digest must be lowercase hex"
    );
    assert_ne!(
        FROZEN_EVIDENCE_SHA256,
        "0".repeat(64),
        "placeholder hash — run scripts/pty-backend-feasibility.sh --write to freeze"
    );
}

#[test]
fn evidence_names_no_transport_owner_type_that_does_not_exist() {
    // This artifact specifies the surface a future native backend must provide.
    // Naming it as though it were an existing trait invites readers to check
    // the code against a contract, find nothing, and treat the decision as
    // blocked on a missing implementor. The requirement is described by what it
    // must do, so no reader can mistake it for a type to look up.
    let rendered = render_canonical_evidence();
    assert!(
        !rendered.contains("PtyTransportOwner"),
        "evidence must not name a transport-owner type; no such type is defined in the workspace"
    );
    // The capability requirements it stands for must still be stated.
    assert!(rendered.contains("own HPCON"));
    assert!(rendered.contains("win.own_hpcon"));
    assert!(rendered.contains("win.job_object_tree"));
}

// ---- Real-OS probe (host-gated) -----------------------------------------

#[cfg(unix)]
#[test]
fn unix_real_pty_probe_confirms_partial_fd_ownership() {
    use portable_pty::{native_pty_system, CommandBuilder, MasterPty, PtySize, PtySystem};

    let pty_system: Box<dyn PtySystem + Send> = native_pty_system();
    let pair = pty_system
        .openpty(PtySize { rows: 24, cols: 80, pixel_width: 0, pixel_height: 0 })
        .expect("open a real pty on this host");

    // The matrix marks unix.own_master_fd as Partial for portable-pty: the fd
    // is borrowable, but portable-pty owns its lifetime. Confirm the fd really
    // is exposed on this host so the Partial verdict is grounded, not asserted.
    let master: &(dyn MasterPty + Send) = pair.master.as_ref();
    assert!(
        master.as_raw_fd().is_some(),
        "portable-pty master must expose a borrowable fd on unix"
    );

    // Spawning a child confirms observable session identity (also Partial):
    // process_id is Some, but it is a cached number, not an owned handle.
    let mut cmd = CommandBuilder::new("/bin/sh");
    cmd.arg("-c");
    cmd.arg("exit 0");
    let mut child = pair.slave.spawn_command(cmd).expect("spawn child in real pty");
    assert!(child.process_id().is_some(), "child pid should be observable through the seam");
    let _ = child.wait();

    // Tie the live observation back to the frozen matrix claims.
    assert_eq!(row_for(Capability::UnixOwnMasterFd).portable_pty, SeamSupport::Partial);
    assert_eq!(row_for(Capability::UnixOwnMasterFd).sonic_native, SeamSupport::Owned);
    assert_eq!(row_for(Capability::UnixSessionIdentity).portable_pty, SeamSupport::Partial);

    // The host row for this OS is marked capturable in the evidence matrix.
    #[cfg(target_os = "macos")]
    {
        let row =
            EVIDENCE_MATRIX.iter().find(|r| r.host_class == "macos").expect("host row present");
        assert!(row.capturable_here, "this host class must be capturable here");
    }

    drop(pair.master);
}
