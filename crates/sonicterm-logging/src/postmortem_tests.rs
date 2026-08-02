use super::*;

use std::sync::atomic::{AtomicU32, Ordering};

/// A scratch directory unique to one test.
///
/// Collection reads *every* marker and *every* artifact in the directories it
/// is given, so a shared scratch would make each test observe its siblings'
/// files. The failure that produces is order-dependent and reports a defect
/// that is not there.
fn scratch(label: &str) -> PathBuf {
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir()
        .join(format!("sonicterm-postmortem-{label}-{}-{unique}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir
}

/// Write a stale (armed, dead-process) marker for a named session.
fn write_stale_marker(log_dir: &Path, id: &str) {
    let dir = session_state::session_dir(log_dir);
    std::fs::create_dir_all(&dir).expect("session dir");
    // pid 0 is never a live process, so the liveness check cannot mask this.
    let marker = format!(
        "id={id}\npid=0\nversion=1.2.3\nplatform=macos\n\
         started_at=2026-01-01T00:00:00Z\nstate=armed\n"
    );
    std::fs::write(dir.join(format!("session-{id}.marker")), marker).expect("write marker");
}

/// Write a crash artifact as the panic hook would.
fn write_artifact(log_dir: &Path, name: &str, contents: &str) -> PathBuf {
    let dir = crate::path::crash_dir_in(log_dir);
    std::fs::create_dir_all(&dir).expect("crash dir");
    let path = dir.join(name);
    std::fs::write(&path, contents).expect("write artifact");
    path
}

/// A hard kill produces no dump, and the report says so in those words.
///
/// This is the acceptance criterion the whole module turns on. A `SIGKILL` or
/// `TerminateProcess` destroys the process before any handler runs, so there
/// is no dump to find — and a reader who is not told that will either keep
/// looking for one or conclude it was lost. Both waste the time this report
/// exists to save.
#[test]
fn a_hard_kill_reports_that_no_process_written_dump_exists() {
    let dir = scratch("hardkill");
    write_stale_marker(&dir, "killed-session");

    let reports = collect(&dir, None);
    assert_eq!(reports.len(), 1, "the stale session must be reported: {reports:?}");
    let report = &reports[0];

    assert!(report.is_unclean(), "a stale marker means the session never reached shutdown");
    assert!(
        !report.has_process_written_dump(),
        "no artifact was written, so the report must not claim one exists"
    );

    let rendered = report.to_string();
    assert!(
        rendered.contains("no process-written memory dump exists"),
        "the report must state plainly that no dump exists. Anything vaguer leaves a reader \
         hunting for a file that was never written: {rendered}"
    );
    assert!(
        rendered.contains("SIGKILL") && rendered.contains("TerminateProcess"),
        "the report must name the terminations it cannot capture, so the limitation is \
         attributable rather than mysterious: {rendered}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// A report never claims to have captured a dump for a hard kill.
///
/// The inverse of the test above, asserted on the wording rather than the
/// flag: a future change that reworded the message must not start implying
/// capture.
#[test]
fn a_hard_kill_report_never_claims_a_captured_dump() {
    let dir = scratch("noclaim");
    write_stale_marker(&dir, "killed-session");

    let rendered = collect(&dir, None)[0].to_string();
    for claim in ["dump captured", "captured a dump", "memory dump written", "core dumped"] {
        assert!(
            !rendered.contains(claim),
            "the report claimed {claim:?} for an uncatchable termination, which the operating \
             system does not permit: {rendered}"
        );
    }

    let _ = std::fs::remove_dir_all(&dir);
}

/// A panic leaves an artifact, and the report classifies it as one.
#[test]
fn a_panic_artifact_is_found_and_classified() {
    let dir = scratch("panic");
    write_stale_marker(&dir, "panicked-session");
    write_artifact(
        &dir,
        "crash-2026-01-01T00-00-00.000Z.log",
        "== sonic crash dump ==\nsession: panicked-session\nmessage: panic at src/main.rs\n",
    );

    let reports = collect(&dir, None);
    let report = &reports[0];

    assert!(report.has_process_written_dump(), "the artifact must be found");
    assert_eq!(report.artifacts[0].kind, ArtifactKind::Panic);
    assert_eq!(
        report.artifacts[0].session_id.as_deref(),
        Some("panicked-session"),
        "an artifact must be attributable to the session that wrote it"
    );
    assert!(
        !report.to_string().contains("no process-written memory dump exists"),
        "a session that did leave a dump must not be told none exists"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// A fatal signal is classified apart from a panic.
///
/// The two have different remedies — a panic points at Rust code, a signal at
/// memory or a native library — so collapsing them into one label would send a
/// reader to the wrong place.
#[test]
fn a_fatal_signal_artifact_is_classified_separately_from_a_panic() {
    let dir = scratch("signal");
    write_stale_marker(&dir, "signal-session");
    write_artifact(
        &dir,
        "crash-signal.log",
        "session: signal-session\nFATAL: SIGSEGV - sonic terminating\n",
    );

    let report = &collect(&dir, None)[0];
    assert_eq!(
        report.artifacts[0].kind,
        ArtifactKind::FatalSignal,
        "a signal artifact must not be reported as a panic; the remedies differ"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// An allocator failure is classified as itself.
#[test]
fn an_allocator_failure_is_classified() {
    let dir = scratch("alloc");
    write_stale_marker(&dir, "alloc-session");
    write_artifact(&dir, "crash-alloc.log", "session: alloc-session\nafter allocator failure\n");

    let report = &collect(&dir, None)[0];
    assert_eq!(report.artifacts[0].kind, ArtifactKind::AllocFailure);

    let _ = std::fs::remove_dir_all(&dir);
}

/// An artifact belonging to another session is not attached to this one.
///
/// Attributing a dump to the wrong session sends a reader to the wrong window
/// of the log, where they will find nothing and conclude the tooling is
/// broken.
#[test]
fn an_artifact_from_another_session_is_not_attached() {
    let dir = scratch("attribution");
    write_stale_marker(&dir, "session-a");
    write_artifact(&dir, "crash-other.log", "session: session-b\npanic in another session\n");

    let report = &collect(&dir, None)[0];
    assert!(
        report.artifacts.is_empty(),
        "an artifact tagged with a different session must not be claimed: {:?}",
        report.artifacts
    );
    assert!(
        report.to_string().contains("no process-written memory dump exists"),
        "with no artifact of its own, the session must be told none exists"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// A clean prior session is not reported as unclean and gets no dump claim.
#[test]
fn a_clean_prior_session_is_not_reported_as_a_failure() {
    let dir = scratch("cleanprior");
    let session = session_state::arm(&dir, "1.2.3").expect("arm");
    session.mark_clean().expect("mark clean");

    let reports = collect(&dir, None);
    assert!(
        !reports.iter().any(PostmortemReport::is_unclean),
        "a session that shut down on purpose must never be reported as a failure: {reports:?}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// A corrupt marker is reported as unclean rather than skipped.
#[test]
fn a_corrupt_marker_is_reported_as_unclean() {
    let dir = scratch("corrupt");
    let session_dir = session_state::session_dir(&dir);
    std::fs::create_dir_all(&session_dir).expect("session dir");
    std::fs::write(session_dir.join("session-broken.marker"), "id=broken\npi").expect("write");

    let reports = collect(&dir, None);
    assert!(
        reports[0].is_unclean(),
        "an unparseable marker is evidence of an interrupted session: {reports:?}"
    );
    assert!(
        reports[0].to_string().contains("no process-written memory dump exists"),
        "a corrupt marker with no artifact must still state that no dump exists"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// With no OS record present, the report says none was found.
///
/// Silence would be ambiguous — a reader cannot tell "looked and found
/// nothing" from "did not look".
#[test]
fn an_absent_os_record_is_stated_rather_than_left_silent() {
    let dir = scratch("noos");
    write_stale_marker(&dir, "no-os-session");

    let rendered = collect(&dir, None)[0].to_string();
    assert!(
        rendered.contains("no operating-system postmortem records found")
            || rendered.contains("may relate to this session"),
        "the report must say whether OS records were found: {rendered}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// OS-record discovery never claims certainty it does not have.
///
/// Matching is by filename convention, so the wording must stay hedged. A
/// report asserting a file *is* SonicTerm's would send someone reading a
/// stranger's crash log while looking for their own.
#[test]
fn os_evidence_is_reported_as_a_conventional_match_not_a_certainty() {
    let dir = scratch("hedge");
    write_stale_marker(&dir, "hedged-session");

    let report = &collect(&dir, None)[0];
    for evidence in &report.os_evidence {
        assert_eq!(
            evidence.attribution,
            Attribution::ByName,
            "filename matching is the only attribution available, and must be labelled as such"
        );
    }
    let rendered = report.to_string();
    if !report.os_evidence.is_empty() {
        assert!(
            rendered.contains("may relate to"),
            "an OS record matched by name must be hedged, not asserted: {rendered}"
        );
    }

    let _ = std::fs::remove_dir_all(&dir);
}

/// Reporting clears the marker so a session is reported once.
///
/// A finding repeated on every launch forever is one a user learns to ignore,
/// which makes it worthless on the launch after a real crash.
#[test]
fn a_reported_session_is_not_reported_again() {
    let dir = scratch("once");
    write_stale_marker(&dir, "once-session");

    assert_eq!(collect(&dir, None).len(), 1, "first launch finds it");
    report_prior_sessions(&dir, None);
    assert!(
        collect(&dir, None).is_empty(),
        "a reported session must not resurface on every subsequent launch"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// A fresh install reports nothing at all.
#[test]
fn a_fresh_install_produces_no_report() {
    let dir = scratch("fresh");
    assert!(collect(&dir, None).is_empty());
    // Must not panic against a directory with no sessions or crashes.
    report_prior_sessions(&dir, None);
    let _ = std::fs::remove_dir_all(&dir);
}

/// The current session is never reported to itself.
#[test]
fn the_current_session_is_excluded() {
    let dir = scratch("current");
    let session = session_state::arm(&dir, "1.2.3").expect("arm");

    let reports = collect(&dir, Some(session.id()));
    assert!(reports.is_empty(), "a launch must not report itself as a prior failure: {reports:?}");

    let _ = std::fs::remove_dir_all(&dir);
}

/// Classification reads dump contents, not the filename.
///
/// Artifact filenames are timestamps and carry no failure information, so a
/// classifier keyed on the name would report every artifact identically.
#[test]
fn classification_reads_contents_rather_than_the_file_name() {
    assert_eq!(ArtifactKind::classify("FATAL: SIGBUS - sonic"), ArtifactKind::FatalSignal);
    assert_eq!(
        ArtifactKind::classify("sonic exiting: after allocator failure"),
        ArtifactKind::AllocFailure
    );
    assert_eq!(ArtifactKind::classify("== sonic crash dump =="), ArtifactKind::Panic);
    assert_eq!(
        ArtifactKind::classify("something unrecognised"),
        ArtifactKind::Unknown,
        "an artifact naming no known failure must be Unknown rather than guessed into a class"
    );
}

/// An untagged artifact cannot be attributed to a particular session.
///
/// There is no backward-compatibility guess here: attaching an old or partial
/// dump to every stale marker is a false ownership claim, and becomes especially
/// misleading when several SonicTerm instances run concurrently.
#[test]
fn an_untagged_artifact_is_not_claimed_by_a_stale_session() {
    let dir = scratch("untagged");
    write_stale_marker(&dir, "target-session");
    write_artifact(&dir, "crash-untagged.log", "== sonic crash dump ==\nmessage: panic\n");

    let report = &collect(&dir, None)[0];
    assert!(
        report.artifacts.is_empty(),
        "a dump with no session identity cannot be attributed: {:?}",
        report.artifacts
    );
    assert!(report.to_string().contains("no process-written memory dump exists"));

    let _ = std::fs::remove_dir_all(&dir);
}

/// A stale report returns the breadcrumbs belonging to that exact session.
#[test]
fn a_stale_report_returns_only_its_own_breadcrumbs() {
    let dir = scratch("breadcrumbs");
    write_stale_marker(&dir, "target-session");
    let breadcrumbs = dir.join("breadcrumbs");
    std::fs::create_dir_all(&breadcrumbs).expect("breadcrumb dir");
    let matching = breadcrumbs.join("breadcrumbs-target-session.log");
    let unrelated = breadcrumbs.join("breadcrumbs-other-session.log");
    std::fs::write(&matching, "event=lifecycle lifecycle=ready\n").expect("matching breadcrumb");
    std::fs::write(&unrelated, "event=lifecycle lifecycle=ready\n").expect("unrelated breadcrumb");

    let report = &collect(&dir, None)[0];
    assert_eq!(
        report.breadcrumbs,
        vec![BreadcrumbEvidence { path: matching, session_id: "target-session".to_string() }]
    );
    assert!(
        report.to_string().contains("breadcrumb evidence"),
        "the rendered report must tell a reader that breadcrumbs survived: {report}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// macOS discovery accepts only conservatively named `.ips` records.
#[test]
fn macos_discovery_is_ips_only_and_conservative() {
    let dir = scratch("mac-evidence");
    let user = dir.join("user");
    let system = dir.join("system");
    std::fs::create_dir_all(&user).expect("user diagnostic dir");
    std::fs::create_dir_all(&system).expect("system diagnostic dir");

    let user_match = user.join("SonicTerm-2026-08-01-120000.ips");
    let system_match = system.join("sonicterm-mac_2026-08-01.ips");
    for path in [&user_match, &system_match] {
        std::fs::write(path, "candidate").expect("diagnostic candidate");
    }
    std::fs::write(user.join("MySonicTerm-2026.ips"), "unrelated")
        .expect("embedded-name candidate");
    std::fs::write(user.join("SonicTerminal-2026.ips"), "unrelated")
        .expect("longer-name candidate");
    std::fs::write(user.join("SonicTerm-2026.crash"), "legacy format")
        .expect("wrong-extension candidate");

    assert_eq!(
        discover_macos_evidence_at(&user, &system),
        vec![
            OsEvidence {
                path: user_match,
                attribution: Attribution::ByName,
                source: OsEvidenceSource::MacUserDiagnosticReports,
            },
            OsEvidence {
                path: system_match,
                attribution: Attribution::ByName,
                source: OsEvidenceSource::MacSystemDiagnosticReports,
            },
        ]
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// Windows filesystem discovery covers both WER stores and LocalDumps.
///
/// It also returns the limitation that filesystem inspection does not inspect
/// WER's registry configuration. The caveat belongs in the typed report, not
/// only in a source comment a user never sees.
#[test]
fn windows_discovery_returns_wer_localdumps_and_the_registry_caveat() {
    let local = scratch("windows-evidence");
    let queue = local.join("Microsoft/Windows/WER/ReportQueue/AppCrash_SonicTerm_abc");
    let archive = local.join("Microsoft/Windows/WER/ReportArchive/AppCrash_sonicterm-windows_xyz");
    let unrelated = local.join("Microsoft/Windows/WER/ReportQueue/AppCrash_OtherApp_abc");
    std::fs::create_dir_all(&queue).expect("WER queue entry");
    std::fs::create_dir_all(&archive).expect("WER archive entry");
    std::fs::create_dir_all(&unrelated).expect("unrelated WER entry");

    let dumps = local.join("CrashDumps");
    std::fs::create_dir_all(&dumps).expect("LocalDumps dir");
    let local_dump = dumps.join("SonicTerm.exe.1234.dmp");
    std::fs::write(&local_dump, "dump").expect("local dump");
    std::fs::write(dumps.join("Other.exe.1234.dmp"), "unrelated").expect("unrelated local dump");

    let discovered = discover_windows_evidence_at(&local);
    assert_eq!(discovered.notes, vec![PostmortemNote::WerRegistryConfigurationNotInspected]);
    assert!(discovered
        .notes
        .iter()
        .any(|note| note.to_string().contains("registry configuration was not inspected")));
    assert!(discovered
        .evidence
        .iter()
        .any(|entry| { entry.path == queue && entry.source == OsEvidenceSource::WindowsWerQueue }));
    assert!(discovered.evidence.iter().any(|entry| {
        entry.path == archive && entry.source == OsEvidenceSource::WindowsWerArchive
    }));
    assert!(discovered.evidence.iter().any(|entry| {
        entry.path == local_dump && entry.source == OsEvidenceSource::WindowsLocalDumps
    }));
    assert!(!discovered.evidence.iter().any(|entry| entry.path == unrelated));

    let _ = std::fs::remove_dir_all(&local);
}

/// Crash session association cannot inject additional line-oriented headers.
#[test]
fn crash_session_identity_accepts_only_the_marker_id_alphabet() {
    assert!(crate::crash::valid_session_id("20260801T120000.000Z-42-0"));
    for id in [
        "",
        "session\nclassification: fatal_signal",
        "../other-session",
        "TOKEN=secret",
        "session id",
    ] {
        assert!(!crate::crash::valid_session_id(id), "accepted unsafe session id {id:?}");
    }
}

/// The panic writer records a typed classification in dump contents.
#[test]
fn the_crash_writer_records_an_explicit_panic_classification() {
    let dir = scratch("crash-header");
    let path = crate::crash::__test_write_dump(&dir, "synthetic panic").expect("write dump");
    let written = std::fs::read_to_string(path).expect("read dump");
    assert!(written.contains("classification: panic"), "missing classification: {written}");
    assert!(written.contains("session:"), "missing session identity field: {written}");
    assert_eq!(ArtifactKind::classify(&written), ArtifactKind::Panic);

    let _ = std::fs::remove_dir_all(&dir);
}

/// Explicit artifact classifications take precedence over incidental words.
#[test]
fn explicit_classification_headers_are_typed() {
    assert_eq!(
        ArtifactKind::classify("classification: fatal_signal\nmessage: panic while handling"),
        ArtifactKind::FatalSignal
    );
    assert_eq!(
        ArtifactKind::classify("classification: alloc_failure\n== sonic crash dump =="),
        ArtifactKind::AllocFailure
    );
}
