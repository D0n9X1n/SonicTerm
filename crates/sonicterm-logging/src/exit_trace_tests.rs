//! Fatal-signal behavior of the exit-trace handler, pinned in fresh processes.
//!
//! Each parent test re-runs this test binary with one scenario. The child
//! installs the exit trace, provokes a fatal signal, and dies; the parent checks
//! the terminating signal, the child's stderr, and the markers it logged.

#![cfg(unix)]

use std::{os::unix::process::ExitStatusExt, sync::atomic::AtomicUsize};

use super::*;

/// Scenario the re-run child performs; unset in the parent.
const SCENARIO: &str = "SONICTERM_FATAL_SIGNAL_SCENARIO";
/// Scratch log directory the child's exit trace writes its marker into.
const LOG_DIR: &str = "SONICTERM_FATAL_SIGNAL_LOG_DIR";

/// What a crashed child left behind.
#[derive(Debug)]
struct Crash {
    /// Signal that ended the child, if a signal did.
    signal: Option<i32>,
    /// Everything the child wrote to stderr, including Rust's overflow report.
    stderr: String,
    /// `FATAL:` lines the handler appended to the child's log.
    markers: Vec<String>,
}

/// Re-run this binary with `scenario` in a scratch directory and collect what
/// the crash left behind; the shared helper bounds the child with a deadline.
fn crash(scenario: &str) -> Crash {
    let directory =
        std::env::temp_dir().join(format!("sonicterm-fatal-{scenario}-{}", std::process::id()));
    std::fs::create_dir_all(&directory).unwrap();
    let output = crate::lib_tests::child_output(
        std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "exit_trace::exit_trace_tests::fatal_signal_child", "--nocapture"])
            .env(SCENARIO, scenario)
            .env(LOG_DIR, &directory)
            // A core file, should the host write one anyway, stays in the scratch directory.
            .current_dir(&directory),
    );
    let log = std::fs::read_to_string(exit_log_path(&directory)).unwrap_or_default();
    let _ = std::fs::remove_dir_all(&directory);
    Crash {
        signal: output.status.signal(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        markers: log
            .lines()
            .filter(|line| line.starts_with("FATAL: "))
            .map(str::to_owned)
            .collect(),
    }
}

/// The marker line the handler writes for `sig`.
fn marker_for(sig: i32) -> String {
    format!(
        "FATAL: {} - sonic terminating (handler async-signal-safe path)",
        String::from_utf8_lossy(signal_name(sig))
    )
}

/// Child half of the fatal-signal tests: a no-op unless a parent set `SCENARIO`.
/// It installs the exit trace against a scratch log directory, then provokes the
/// scenario's fatal signal and should never return.
#[test]
fn fatal_signal_child() {
    let Ok(scenario) = std::env::var(SCENARIO) else { return };
    let directory = PathBuf::from(std::env::var_os(LOG_DIR).unwrap());
    // The parent reads the terminating signal, not a core file, so each
    // deliberate crash skips writing one.
    let no_core = libc::rlimit { rlim_cur: 0, rlim_max: 0 };
    // SAFETY: setrlimit only reads `no_core` and affects only this process.
    unsafe {
        libc::setrlimit(libc::RLIMIT_CORE, &no_core);
    }
    match scenario.as_str() {
        "custom" => install_fault_recorder(),
        "ignored" => ignore(libc::SIGFPE),
        _ => {}
    }
    let _guard = install_exit_logging(&directory);
    match scenario.as_str() {
        "overflow" => overflow_on_named_thread(),
        "abort" => {
            // A second install is a no-op, so the handler cannot record itself
            // as the previous action.
            let _second = install_exit_logging(&directory);
            raise_signal(libc::SIGABRT);
        }
        "fault" | "custom" => touch_inaccessible_page(),
        "ignored" => raise_signal(libc::SIGFPE),
        "illegal" => raise_signal(libc::SIGILL),
        other => panic!("unknown fatal-signal scenario {other}"),
    }
}

/// Raise `sig` on this thread.
fn raise_signal(sig: libc::c_int) {
    // SAFETY: raise only delivers `sig` to this thread.
    unsafe {
        libc::raise(sig);
    }
}

/// Set `sig` to `SIG_IGN`, so the install records an ignored previous action.
fn ignore(sig: libc::c_int) {
    // SAFETY: SIG_IGN is a valid disposition for `sig`, and signal retains no pointer.
    unsafe {
        libc::signal(sig, libc::SIG_IGN);
    }
}

/// Recurse on a named thread with a small stack until it reaches the guard page.
fn overflow_on_named_thread() {
    let overflowing = std::thread::Builder::new()
        .name("exit-trace-overflow".into())
        .stack_size(256 * 1024)
        .spawn(|| recurse(&[0; 512], 0))
        .unwrap();
    let _ = overflowing.join();
}

/// Each frame keeps a 512-byte buffer that its callee reads, so every frame
/// stays live and the recursion cannot become a loop; it ends in the guard page.
fn recurse(parent: &[u8; 512], depth: u64) -> u64 {
    let mut frame = [0u8; 512];
    frame[0] = parent[0].wrapping_add(1);
    if std::hint::black_box(depth) == u64::MAX {
        return u64::from(frame[0]);
    }
    recurse(std::hint::black_box(&frame), depth + 1) + u64::from(frame[1])
}

/// Address of the page `touch_inaccessible_page` faults on.
static FAULT_ADDRESS: AtomicUsize = AtomicUsize::new(0);

/// Read a fresh `PROT_NONE` page: a genuine fault outside any stack guard.
fn touch_inaccessible_page() {
    // SAFETY: mmap of one anonymous PROT_NONE page returns a valid mapping or
    // MAP_FAILED, and the volatile read deliberately faults on that mapping.
    unsafe {
        let page = libc::mmap(
            std::ptr::null_mut(),
            4096,
            libc::PROT_NONE,
            libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
            -1,
            0,
        );
        assert_ne!(page, libc::MAP_FAILED);
        FAULT_ADDRESS.store(page as usize, Ordering::SeqCst);
        let _ = std::ptr::read_volatile(page.cast::<u8>());
    }
}

/// A custom previous SIGSEGV/SIGBUS action: reports on stderr whether it
/// received the original fault address, then returns.
extern "C" fn record_fault(_: libc::c_int, info: *mut libc::siginfo_t, _: *mut libc::c_void) {
    let report: &[u8] = if fault_address(info) == FAULT_ADDRESS.load(Ordering::SeqCst) {
        b"previous handler saw the fault address\n"
    } else {
        b"previous handler saw a different address\n"
    };
    // SAFETY: write is async-signal-safe, and `report` is a 'static byte slice.
    unsafe {
        libc::write(2, report.as_ptr().cast(), report.len());
    }
}

/// Fault address in the SIGSEGV/SIGBUS `siginfo_t` that `info` points to.
#[cfg(target_vendor = "apple")]
fn fault_address(info: *const libc::siginfo_t) -> usize {
    // SAFETY: the kernel passes a valid siginfo_t to an SA_SIGINFO action, and
    // the exit-trace handler forwards that pointer unchanged.
    unsafe { (*info).si_addr as usize }
}

/// Fault address in the SIGSEGV/SIGBUS `siginfo_t` that `info` points to.
#[cfg(not(target_vendor = "apple"))]
fn fault_address(info: *const libc::siginfo_t) -> usize {
    // SAFETY: the kernel passes a valid siginfo_t to an SA_SIGINFO action and
    // the exit-trace handler forwards it unchanged; si_addr reads the member
    // the kernel fills for SIGSEGV and SIGBUS.
    unsafe { (*info).si_addr() as usize }
}

/// Install `record_fault` for SIGSEGV and SIGBUS before the exit trace, so it
/// is the previous action the exit trace chains to.
fn install_fault_recorder() {
    for sig in [libc::SIGSEGV, libc::SIGBUS] {
        // SAFETY: an all-zero sigaction is valid, every field read is written
        // first, and sigaction copies the struct without retaining it.
        unsafe {
            let mut act: libc::sigaction = std::mem::MaybeUninit::zeroed().assume_init();
            act.sa_sigaction = record_fault as *const () as libc::sighandler_t;
            act.sa_flags = libc::SA_SIGINFO;
            libc::sigemptyset(&mut act.sa_mask);
            libc::sigaction(sig, &act, std::ptr::null_mut());
        }
    }
}

// A guard-page overflow on a named thread keeps both the log marker and Rust's
// overflow report, which needs the original fault address. The report's abort()
// then ends the process through SIGABRT's previous, default action.
#[test]
fn a_stack_overflow_keeps_the_marker_and_rusts_overflow_report() {
    let crash = crash("overflow");
    assert!(
        crash.stderr.contains("thread 'exit-trace-overflow'")
            && crash.stderr.contains("has overflowed its stack"),
        "{crash:?}"
    );
    assert_eq!(crash.signal, Some(libc::SIGABRT), "{crash:?}");
    assert_eq!(crash.markers.len(), 1, "{crash:?}");
    assert!(
        [libc::SIGSEGV, libc::SIGBUS].into_iter().any(|sig| crash.markers[0] == marker_for(sig)),
        "{crash:?}"
    );
}

// An ordinary SIGABRT after a repeated install logs exactly one marker and ends
// the process by SIGABRT: a default previous action still terminates.
#[test]
fn an_abort_logs_one_marker_and_ends_by_sigabrt() {
    let crash = crash("abort");
    assert_eq!(crash.signal, Some(libc::SIGABRT), "{crash:?}");
    assert_eq!(crash.markers, [marker_for(libc::SIGABRT)], "{crash:?}");
}

// A genuine fault outside any stack guard is logged, is not reported as an
// overflow, and ends the process by the faulting signal.
#[test]
fn a_fault_outside_a_stack_guard_ends_by_its_signal() {
    let crash = crash("fault");
    let signal = crash.signal.unwrap_or_else(|| panic!("no terminating signal: {crash:?}"));
    assert!([libc::SIGSEGV, libc::SIGBUS].contains(&signal), "{crash:?}");
    assert_eq!(crash.markers, [marker_for(signal)], "{crash:?}");
    assert!(!crash.stderr.contains("overflowed its stack"), "{crash:?}");
}

// A custom previous SA_SIGINFO action receives the original fault address, and
// after it returns the default action still ends the process.
#[test]
fn a_custom_previous_handler_sees_the_original_fault() {
    let crash = crash("custom");
    assert!(crash.stderr.contains("previous handler saw the fault address"), "{crash:?}");
    let signal = crash.signal.unwrap_or_else(|| panic!("no terminating signal: {crash:?}"));
    assert!([libc::SIGSEGV, libc::SIGBUS].contains(&signal), "{crash:?}");
    assert_eq!(crash.markers, [marker_for(signal)], "{crash:?}");
}

// An ignored previous disposition is not honoured for a fatal signal: the signal
// is logged and still ends the process.
#[test]
fn an_ignored_fatal_signal_still_ends_the_process() {
    let crash = crash("ignored");
    assert_eq!(crash.signal, Some(libc::SIGFPE), "{crash:?}");
    assert_eq!(crash.markers, [marker_for(libc::SIGFPE)], "{crash:?}");
}

// A raised SIGILL ends the process after one marker. macOS keeps a SIGILL handler
// despite SA_RESETHAND, so without an explicit default the handler re-enters.
#[test]
fn a_raised_sigill_ends_the_process_after_one_marker() {
    let crash = crash("illegal");
    assert_eq!(crash.signal, Some(libc::SIGILL), "{crash:?}");
    assert_eq!(crash.markers, [marker_for(libc::SIGILL)], "{crash:?}");
}

/// Stand-in SA_SIGINFO action whose address the classification tests use.
extern "C" fn sample_siginfo_action(_: libc::c_int, _: *mut libc::siginfo_t, _: *mut libc::c_void) {
}

/// Stand-in one-argument action whose address the classification tests use.
extern "C" fn sample_handler_action(_: libc::c_int) {}

// SIG_DFL and SIG_IGN are sentinels that are never called, whatever sa_flags
// says, and the handler never chains to itself.
#[test]
fn sentinels_and_the_handler_itself_are_never_called() {
    for flags in [0, libc::SA_SIGINFO] {
        assert_eq!(chain_for(libc::SIG_DFL, flags), Chain::Default);
        assert_eq!(chain_for(libc::SIG_IGN, flags), Chain::Default);
        assert_eq!(chain_for(own_handler_address(), flags), Chain::Default);
    }
}

// SA_SIGINFO selects the three-argument call, and its absence the one-argument call.
#[test]
fn sa_siginfo_selects_the_call_shape() {
    let siginfo = sample_siginfo_action as *const () as libc::sighandler_t;
    let handler = sample_handler_action as *const () as libc::sighandler_t;
    assert_eq!(chain_for(siginfo, libc::SA_SIGINFO), Chain::SigInfo(siginfo));
    assert_eq!(chain_for(handler, 0), Chain::Handler(handler));
}

// The handler reads the slot the install wrote for the same signal, so no signal
// chains to another signal's previous action; uncovered signals have no slot.
#[test]
fn each_fatal_signal_reads_the_slot_install_wrote() {
    for (sig, slot) in FATAL_SIGNALS.into_iter().zip(&PREVIOUS_ACTIONS) {
        assert!(slot_for(sig).is_some_and(|found| std::ptr::eq(found, slot)), "signal {sig}");
    }
    assert!(slot_for(libc::SIGTERM).is_none());
}
