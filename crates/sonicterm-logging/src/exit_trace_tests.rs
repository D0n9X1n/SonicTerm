//! Fatal-signal behavior of the exit-trace handler, pinned in fresh processes.
//!
//! Each parent test re-runs this test binary with one scenario. The child
//! installs the exit trace, provokes a fatal signal or drives the chain
//! directly, and reports on stderr; the parent checks how the child ended, its
//! stderr, and the markers it logged.

#![cfg(unix)]

use std::{io::Read, os::unix::process::ExitStatusExt, sync::atomic::AtomicUsize};

use super::*;

/// Scenario the re-run child performs; unset in the parent.
const SCENARIO: &str = "SONICTERM_FATAL_SIGNAL_SCENARIO";
/// Scratch log directory the child's exit trace writes its marker into.
const LOG_DIR: &str = "SONICTERM_FATAL_SIGNAL_LOG_DIR";
/// Most of a child's log the parent reads, so a runaway child cannot exhaust it.
const READ_LIMIT: u64 = 64 * 1024;

/// What a child left behind.
#[derive(Debug)]
struct Crash {
    /// Signal that ended the child, if a signal did.
    signal: Option<i32>,
    /// Exit code, when the child exited instead.
    code: Option<i32>,
    /// Everything the child wrote to stderr, including Rust's overflow report.
    stderr: String,
    /// `FATAL:` lines the handler appended to the child's log.
    markers: Vec<String>,
}

/// A child's scratch directory, removed when the guard drops, including after a
/// failed or timed-out run.
struct Scratch(PathBuf);

impl Scratch {
    /// A fresh directory unique to `scenario`, this process, and this moment.
    fn new(scenario: &str) -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let directory = std::env::temp_dir()
            .join(format!("sonicterm-fatal-{scenario}-{}-{nanos}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        Self(directory)
    }
}

// Lifecycle: dropping Scratch removes the child's directory once the parent has
// read its log, whether the run passed, failed, or timed out.
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// `FATAL:` lines in the first `READ_LIMIT` bytes of the log under `directory`.
fn logged_markers(directory: &Path) -> Vec<String> {
    let mut log = Vec::new();
    if let Ok(file) = std::fs::File::open(exit_log_path(directory)) {
        let _ = file.take(READ_LIMIT).read_to_end(&mut log);
    }
    String::from_utf8_lossy(&log)
        .lines()
        .filter(|line| line.starts_with("FATAL: "))
        .map(str::to_owned)
        .collect()
}

/// Re-run this binary with `scenario` in a scratch directory and collect what the
/// child left behind. The shared helper bounds the child with a deadline; when it
/// panics, the scenario and its logged markers are printed before the panic
/// resumes, and the guard still removes the directory.
fn crash(scenario: &str) -> Crash {
    let scratch = Scratch::new(scenario);
    let run = std::panic::catch_unwind(|| {
        crate::lib_tests::child_output(
            std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "exit_trace::exit_trace_tests::fatal_signal_child",
                    "--nocapture",
                ])
                .env(SCENARIO, scenario)
                .env(LOG_DIR, &scratch.0)
                // A core file, should the host write one anyway, stays in the scratch directory.
                .current_dir(&scratch.0),
        )
    });
    let markers = logged_markers(&scratch.0);
    let output = run.unwrap_or_else(|panic| {
        eprintln!("fatal-signal scenario {scenario:?} failed; logged markers: {markers:?}");
        std::panic::resume_unwind(panic)
    });
    Crash {
        signal: output.status.signal(),
        code: output.status.code(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        markers,
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
/// It contains crash reports, records the scenario's previous actions, installs
/// the exit trace against a scratch log directory, then runs the scenario. Every
/// scenario except `once` ends by a signal and never returns.
#[test]
fn fatal_signal_child() {
    let Ok(scenario) = std::env::var(SCENARIO) else { return };
    let directory = PathBuf::from(std::env::var_os(LOG_DIR).unwrap());
    contain_crash_reports();
    match scenario.as_str() {
        "custom" => install_fault_recorder(),
        "ignored" => ignore(libc::SIGFPE),
        "rearm" => {
            install_previous(
                libc::SIGSEGV,
                inspect_dispositions as *const (),
                libc::SA_SIGINFO,
                &[],
            );
            install_previous(libc::SIGBUS, count_call as *const (), libc::SA_RESETHAND, &[]);
        }
        "once" => install_previous(libc::SIGABRT, count_call as *const (), 0, &[]),
        "plain" => install_previous(libc::SIGABRT, report_mask as *const (), 0, &[libc::SIGUSR1]),
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
        "rearm" => raise_signal(libc::SIGSEGV),
        "plain" => raise_signal(libc::SIGABRT),
        "once" => {
            // Two entries reach the chain for the same signal; only the first may
            // call the recorded action.
            chain_previous(libc::SIGABRT, std::ptr::null_mut(), std::ptr::null_mut());
            chain_previous(libc::SIGABRT, std::ptr::null_mut(), std::ptr::null_mut());
            eprintln!("previous action calls: {}", CALLS.load(Ordering::SeqCst));
        }
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

/// Install `action` for `sig` with `flags` and `blocked` in its mask, before the
/// exit trace, so it is the previous action the exit trace records.
fn install_previous(
    sig: libc::c_int,
    action: *const (),
    flags: libc::c_int,
    blocked: &[libc::c_int],
) {
    // SAFETY: an all-zero sigaction is valid, every field read is written first,
    // `action` is an extern "C" function whose arity matches `flags`, and
    // sigaction copies the struct without retaining it.
    unsafe {
        let mut act: libc::sigaction = std::mem::MaybeUninit::zeroed().assume_init();
        act.sa_sigaction = action as libc::sighandler_t;
        act.sa_flags = flags;
        libc::sigemptyset(&mut act.sa_mask);
        for &masked in blocked {
            libc::sigaddset(&mut act.sa_mask, masked);
        }
        libc::sigaction(sig, &act, std::ptr::null_mut());
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
        install_previous(sig, record_fault as *const (), libc::SA_SIGINFO, &[]);
    }
}

/// Write `bytes` to stderr with one async-signal-safe `write(2)`.
fn report(bytes: &[u8]) {
    // SAFETY: write is async-signal-safe, and `bytes` is a valid slice for the call.
    unsafe {
        libc::write(2, bytes.as_ptr().cast(), bytes.len());
    }
}

/// Calls of `count_call`, a stand-in previous action.
static CALLS: AtomicUsize = AtomicUsize::new(0);

/// A one-argument previous action that only counts its calls.
extern "C" fn count_call(_: libc::c_int) {
    CALLS.fetch_add(1, Ordering::SeqCst);
}

/// A previous SIGSEGV action that reports, while that signal's chain runs,
/// whether SIGBUS still has the exit-trace handler and SIGABRT its default.
extern "C" fn inspect_dispositions(_: libc::c_int, _: *mut libc::siginfo_t, _: *mut libc::c_void) {
    let bus = disposition(libc::SIGBUS);
    let bus_report: &[u8] = if bus == own_handler_address() {
        b"SIGBUS keeps the exit-trace handler\n"
    } else if bus == count_call as *const () as libc::sighandler_t {
        b"SIGBUS is back on its previous action\n"
    } else {
        b"SIGBUS has another action\n"
    };
    report(bus_report);
    let abort_report: &[u8] = if disposition(libc::SIGABRT) == libc::SIG_DFL {
        b"SIGABRT is at its default action\n"
    } else {
        b"SIGABRT is not at its default action\n"
    };
    report(abort_report);
}

/// The handler address currently installed for `sig`.
fn disposition(sig: libc::c_int) -> libc::sighandler_t {
    // SAFETY: an all-zero sigaction is valid, and a null new action makes
    // sigaction only report the current one into `current`.
    unsafe {
        let mut current: libc::sigaction = std::mem::MaybeUninit::zeroed().assume_init();
        libc::sigaction(sig, std::ptr::null(), &mut current);
        current.sa_sigaction
    }
}

/// A one-argument previous SIGABRT action that reports whether its saved mask
/// and its own signal are blocked while it runs.
extern "C" fn report_mask(sig: libc::c_int) {
    let mask_report: &[u8] = match (blocked(libc::SIGUSR1), blocked(sig)) {
        (true, true) => b"saved mask applied and signal blocked\n",
        (true, false) => b"saved mask applied but signal unblocked\n",
        (false, true) => b"saved mask missing and signal blocked\n",
        (false, false) => b"saved mask missing and signal unblocked\n",
    };
    report(mask_report);
}

/// Whether `sig` is in this thread's blocked set.
fn blocked(sig: libc::c_int) -> bool {
    // SAFETY: an all-zero sigset_t is valid, a null new set makes pthread_sigmask
    // only report the current mask into `mask`, and sigismember only reads it.
    unsafe {
        let mut mask: libc::sigset_t = std::mem::MaybeUninit::zeroed().assume_init();
        libc::pthread_sigmask(libc::SIG_BLOCK, std::ptr::null(), &mut mask);
        libc::sigismember(&mask, sig) == 1
    }
}

/// Keep a deliberate crash from leaving host artifacts: no core file, and no
/// report from the platform crash reporter. The parent only reads how the child
/// ended.
fn contain_crash_reports() {
    let no_core = libc::rlimit { rlim_cur: 0, rlim_max: 0 };
    // SAFETY: setrlimit only reads `no_core` and affects only this process.
    unsafe {
        libc::setrlimit(libc::RLIMIT_CORE, &no_core);
    }
    assert_eq!(disable_crash_reporter(), 0, "the host crash reporter must be disabled");
}

/// Mark the process non-dumpable, so a piped core handler such as apport or
/// systemd-coredump never receives a deliberate crash.
#[cfg(target_os = "linux")]
fn disable_crash_reporter() -> libc::c_int {
    let not_dumpable: libc::c_ulong = 0;
    // SAFETY: PR_SET_DUMPABLE takes one integer argument and changes only this
    // process's dumpable flag.
    unsafe { libc::prctl(libc::PR_SET_DUMPABLE, not_dumpable) }
}

/// Clear this task's crash exception ports, so ReportCrash never writes a report
/// for a deliberate crash; the child still ends by its signal.
#[cfg(target_os = "macos")]
fn disable_crash_reporter() -> libc::c_int {
    // SAFETY: `TASK_SELF` is the task port the runtime sets before `main`, and
    // replacing this task's own crash exception ports with the null port affects
    // only this process.
    unsafe {
        task_set_exception_ports(
            TASK_SELF,
            EXC_MASK_CRASH | EXC_MASK_CORPSE_NOTIFY,
            MACH_PORT_NULL,
            EXCEPTION_STATE_IDENTITY_CODES,
            THREAD_STATE_NONE,
        )
    }
}

/// Other Unix hosts rely on the zero core-file limit alone.
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn disable_crash_reporter() -> libc::c_int {
    0
}

/// `EXC_MASK_CRASH` from `<mach/exception_types.h>`: `1 << EXC_CRASH`.
#[cfg(target_os = "macos")]
const EXC_MASK_CRASH: libc::c_uint = 1 << 10;
/// `EXC_MASK_CORPSE_NOTIFY` from `<mach/exception_types.h>`: `1 << EXC_CORPSE_NOTIFY`.
#[cfg(target_os = "macos")]
const EXC_MASK_CORPSE_NOTIFY: libc::c_uint = 1 << 13;
/// `EXCEPTION_STATE_IDENTITY | MACH_EXCEPTION_CODES`, that is 3 | 0x8000_0000.
#[cfg(target_os = "macos")]
const EXCEPTION_STATE_IDENTITY_CODES: libc::c_int = libc::c_int::MIN | 3;
/// The null Mach port.
#[cfg(target_os = "macos")]
const MACH_PORT_NULL: libc::c_uint = 0;
/// `THREAD_STATE_NONE` from `<mach/arm/thread_status.h>`.
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const THREAD_STATE_NONE: libc::c_int = 5;
/// `THREAD_STATE_NONE` from `<mach/i386/thread_status.h>`.
#[cfg(all(target_os = "macos", target_arch = "x86_64"))]
const THREAD_STATE_NONE: libc::c_int = 13;

#[cfg(target_os = "macos")]
extern "C" {
    /// This task's port, which the runtime initializes before `main`.
    #[link_name = "mach_task_self_"]
    static TASK_SELF: libc::c_uint;
    /// `task_set_exception_ports` from `<mach/task.h>`.
    fn task_set_exception_ports(
        task: libc::c_uint,
        exception_mask: libc::c_uint,
        new_port: libc::c_uint,
        behavior: libc::c_int,
        new_flavor: libc::c_int,
    ) -> libc::c_int;
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

// While one fatal signal's chain runs, a signal whose previous action is a
// function keeps the exit-trace handler: reinstalling that action could re-arm a
// one-shot action that another thread already consumed. A signal whose previous
// action is the default gets its default action.
#[test]
fn a_callable_previous_action_is_never_reinstalled() {
    let crash = crash("rearm");
    assert!(crash.stderr.contains("SIGBUS keeps the exit-trace handler\n"), "{crash:?}");
    assert!(crash.stderr.contains("SIGABRT is at its default action\n"), "{crash:?}");
    assert_eq!(crash.signal, Some(libc::SIGSEGV), "{crash:?}");
    assert_eq!(crash.markers, [marker_for(libc::SIGSEGV)], "{crash:?}");
}

// However many entries reach the chain for one signal, its previous action runs
// at most once. The child drives the chain directly and exits normally.
#[test]
fn a_previous_action_is_called_at_most_once() {
    let crash = crash("once");
    assert!(crash.stderr.contains("previous action calls: 1\n"), "{crash:?}");
    assert_eq!(crash.code, Some(0), "{crash:?}");
    assert!(crash.markers.is_empty(), "{crash:?}");
}

// A one-argument previous action runs with its saved mask and its own signal
// blocked, and the default action still ends the process afterwards.
#[test]
fn a_one_argument_previous_action_runs_with_its_saved_mask() {
    let crash = crash("plain");
    assert!(crash.stderr.contains("saved mask applied and signal blocked\n"), "{crash:?}");
    assert_eq!(crash.signal, Some(libc::SIGABRT), "{crash:?}");
    assert_eq!(crash.markers, [marker_for(libc::SIGABRT)], "{crash:?}");
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
        assert!(classify(libc::SIG_DFL, flags).is_none());
        assert!(classify(libc::SIG_IGN, flags).is_none());
        assert!(classify(own_handler_address(), flags).is_none());
    }
}

// SA_SIGINFO selects the three-argument call, and its absence the one-argument call.
#[test]
fn sa_siginfo_selects_the_call_shape() {
    let siginfo = sample_siginfo_action as *const () as libc::sighandler_t;
    let handler = sample_handler_action as *const () as libc::sighandler_t;
    assert_eq!(classify(siginfo, libc::SA_SIGINFO), Some(Callable::SigInfo(siginfo)));
    assert_eq!(classify(handler, 0), Some(Callable::Handler(handler)));
}

// Only the first claim of a slot succeeds, so no later entry, from any signal or
// thread, calls a previous action a second time.
#[test]
fn a_previous_action_is_claimed_once() {
    let slot = Previous::new();
    assert!(slot.claim());
    assert!(!slot.claim());
    assert!(!slot.claim());
}

// A slot the install has not written has nothing to call and is not treated as
// uncallable, so a signal that arrives mid-install keeps its disposition.
#[test]
fn an_unrecorded_slot_is_left_alone() {
    let slot = Previous::new();
    assert!(slot.recorded().is_none());
    assert!(!slot.uncallable());
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
