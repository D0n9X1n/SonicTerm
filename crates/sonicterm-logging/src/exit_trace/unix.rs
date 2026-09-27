//! Unix fatal-signal handling for the exit trace: the chained `sigaction`
//! handler for SIGSEGV, SIGBUS, SIGILL, SIGABRT and SIGFPE, the action each
//! signal had before install, and the installing thread's alternate stack. The
//! coverage matrix and the chaining contract are documented in `exit_trace.rs`.

use super::*;

/// Fatal signals the exit-trace handler covers, in the slot order of
/// `PREVIOUS_ACTIONS`.
pub(super) const FATAL_SIGNALS: [libc::c_int; 5] =
    [libc::SIGSEGV, libc::SIGBUS, libc::SIGILL, libc::SIGABRT, libc::SIGFPE];

/// What one fatal signal did before `install_exit_logging` replaced it, and
/// whether the handler has called that action yet.
pub(super) struct Previous {
    /// The action `sigaction` reported at install, recorded once. The handler
    /// reads it with `OnceLock::get`, which never blocks or allocates. An empty
    /// slot means the signal arrived before its previous action was recorded.
    action: std::sync::OnceLock<libc::sigaction>,
    /// Set by the first entry that calls `action`, so the handler calls it at
    /// most once however many threads or repeated signals reach the chain.
    called: AtomicBool,
}

impl Previous {
    /// An empty slot: nothing recorded and nothing called yet.
    pub(super) const fn new() -> Self {
        Self { action: std::sync::OnceLock::new(), called: AtomicBool::new(false) }
    }

    /// The recorded action and how to call it, when it is a function.
    pub(super) fn recorded(&self) -> Option<(&libc::sigaction, Callable)> {
        self.action.get().and_then(|action| {
            classify(action.sa_sigaction, action.sa_flags).map(|callable| (action, callable))
        })
    }

    /// Whether the recorded action is `SIG_DFL`, `SIG_IGN`, or this handler. An
    /// empty slot is neither callable nor uncallable: it is left alone.
    pub(super) fn uncallable(&self) -> bool {
        self.action
            .get()
            .is_some_and(|action| classify(action.sa_sigaction, action.sa_flags).is_none())
    }

    /// Claim the single call of the recorded action; only the first claim succeeds.
    pub(super) fn claim(&self) -> bool {
        !self.called.swap(true, Ordering::SeqCst)
    }
}

/// The action each fatal signal had before `install_exit_logging` replaced it,
/// so the handler can chain to it: Rust's runtime owns the SIGSEGV/SIGBUS
/// overflow report, and an earlier crash reporter may own any of the five.
pub(super) static PREVIOUS_ACTIONS: [Previous; FATAL_SIGNALS.len()] =
    [const { Previous::new() }; FATAL_SIGNALS.len()];

/// Set by the first fatal signal to enter the handler, so the log receives one
/// marker however many fatal signals follow.
static FATAL_ENTERED: AtomicBool = AtomicBool::new(false);

pub(super) fn install_signal_handlers() {
    use std::mem::MaybeUninit;

    // An alternate stack for this thread, so a stack overflow here still has
    // room for the handler and the action it chains to. Threads spawned through
    // `std` get their own alternate stack from Rust's runtime.
    // SAFETY: `buf` is leaked, so the memory the kernel switches to stays valid
    // for the life of the process. `sigaltstack` only reads `alt_stack`, and a null
    // old-stack pointer means "do not report the previous stack".
    unsafe {
        const STK_SIZE: usize = 64 * 1024;
        let buf = Box::leak(vec![0u8; STK_SIZE].into_boxed_slice());
        let alt_stack =
            libc::stack_t { ss_sp: buf.as_mut_ptr() as *mut _, ss_flags: 0, ss_size: STK_SIZE };
        libc::sigaltstack(&alt_stack, std::ptr::null_mut());
    }

    for (sig, slot) in FATAL_SIGNALS.into_iter().zip(&PREVIOUS_ACTIONS) {
        // SAFETY: an all-zero `libc::sigaction` is a valid value for this plain
        // C struct, and every field it is read through is written before the
        // call. `sigaction` copies `act`, fills `previous`, and retains neither.
        unsafe {
            let mut act: libc::sigaction = MaybeUninit::zeroed().assume_init();
            act.sa_sigaction = own_handler_address();
            act.sa_flags = libc::SA_SIGINFO | libc::SA_ONSTACK | libc::SA_RESETHAND;
            libc::sigemptyset(&mut act.sa_mask);
            let mut previous: libc::sigaction = MaybeUninit::zeroed().assume_init();
            libc::sigaction(sig, &act, &mut previous);
            // `INSTALLED` admits one install per process, so every slot is still empty.
            let _ = slot.action.set(previous);
        }
    }
}

/// The handler's address as `sigaction` stores and reports it.
pub(super) fn own_handler_address() -> libc::sighandler_t {
    handle_signal as *const () as libc::sighandler_t
}

/// How to call a recorded action that is a function.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Callable {
    /// A one-argument `sa_handler`, called with the signal number.
    Handler(libc::sighandler_t),
    /// A three-argument `sa_sigaction`, called with the original `siginfo_t` and context.
    SigInfo(libc::sighandler_t),
}

/// Call shape of a recorded `SA_SIGINFO` action.
type SigInfoAction = extern "C" fn(libc::c_int, *mut libc::siginfo_t, *mut libc::c_void);

/// Call shape of a recorded one-argument action.
type HandlerAction = extern "C" fn(libc::c_int);

/// Classify a recorded action. `SIG_DFL` and `SIG_IGN` are sentinels rather
/// than functions whatever `flags` says, and the handler never chains to
/// itself: each yields `None`, so an ignored fatal signal still ends the
/// process, as a synchronous fault would only recur. Otherwise `SA_SIGINFO`
/// selects the three-argument call.
pub(super) fn classify(handler: libc::sighandler_t, flags: libc::c_int) -> Option<Callable> {
    if handler == libc::SIG_DFL || handler == libc::SIG_IGN || handler == own_handler_address() {
        // When: handler is SIG_DFL, SIG_IGN, or own_handler_address, so nothing
        // may be called and the default action ends the process.
        return None;
    }
    match flags & libc::SA_SIGINFO {
        0 => Some(Callable::Handler(handler)),
        _ => Some(Callable::SigInfo(handler)),
    }
}

/// Map signal number to a short static byte slice. Async-signal-safe.
pub(super) fn signal_name(sig: libc::c_int) -> &'static [u8] {
    match sig {
        libc::SIGSEGV => b"SIGSEGV",
        libc::SIGBUS => b"SIGBUS",
        libc::SIGILL => b"SIGILL",
        libc::SIGABRT => b"SIGABRT",
        libc::SIGFPE => b"SIGFPE",
        _ => b"SIG?",
    }
}

/// Fatal-signal handler: writes the marker for the first fatal signal, calls
/// the previous action at most once, then ends the process by the signal's
/// default action.
extern "C" fn handle_signal(sig: libc::c_int, info: *mut libc::siginfo_t, ctx: *mut libc::c_void) {
    // Async-signal-safe: ONLY write(2) and fsync on a pre-opened fd, sigaction,
    // sigaddset, pthread_sigmask, and raise, plus lock-free atomic and OnceLock
    // reads. No tracing macros, no alloc, no locks.
    if !FATAL_ENTERED.swap(true, Ordering::SeqCst) {
        // Only the first fatal signal writes a marker, so one crash logs one line;
        // it also gives the other fatal signals without a callable previous
        // action their default action.
        write_marker(sig);
        default_uncallable_others(sig);
    }
    chain_previous(sig, info, ctx);
    terminate_by_default(sig);
}

/// Append `FATAL: <signal> - …` to the pre-opened log descriptor, if there is one.
fn write_marker(sig: libc::c_int) {
    let fd = LOG_FD.load(Ordering::SeqCst);
    if fd >= 0 {
        // When: fd is a real descriptor, so the marker can be written from the
        // handler; a negative fd means the log was never opened for this path.
        let prefix: &[u8] = b"FATAL: ";
        let suffix: &[u8] = b" - sonic terminating (handler async-signal-safe path)\n";
        // SAFETY: async-signal-safe by construction — only `write` and `fsync`
        // on a descriptor opened before any signal could arrive, with pointers
        // and lengths taken from `'static` byte slices.
        unsafe {
            libc::write(fd, prefix.as_ptr() as *const _, prefix.len());
            let name = signal_name(sig);
            libc::write(fd, name.as_ptr() as *const _, name.len());
            libc::write(fd, suffix.as_ptr() as *const _, suffix.len());
            // Best-effort fsync — ignore failure.
            libc::fsync(fd);
        }
    }
}

/// Give every other fatal signal whose previous action is `SIG_DFL`, `SIG_IGN`,
/// or this handler its default action. A fatal signal raised while the chain
/// runs, such as the `abort()` that follows Rust's overflow report, then ends
/// the process directly instead of stacking a second handler frame on an
/// alternate stack sized for one. A signal whose previous action is a function
/// keeps this handler, which calls that function at most once and never
/// reinstalls it, so a consumed one-shot action cannot be re-armed. Unrecorded
/// slots, and `sig` itself, keep their disposition.
fn default_uncallable_others(sig: libc::c_int) {
    let uncallable = FATAL_SIGNALS
        .into_iter()
        .zip(&PREVIOUS_ACTIONS)
        .filter(|&(other, previous)| other != sig && previous.uncallable());
    for (other, _) in uncallable {
        set_default(other);
    }
}

/// Install `SIG_DFL` for `sig`.
fn set_default(sig: libc::c_int) {
    // SAFETY: an all-zero `libc::sigaction` is `SIG_DFL` with an empty mask and
    // no flags, and `sigaction` copies it without retaining a pointer.
    unsafe {
        let default: libc::sigaction = std::mem::MaybeUninit::zeroed().assume_init();
        libc::sigaction(sig, &default, std::ptr::null_mut());
    }
}

/// Call the action `sig` had before install, at most once per process: with its
/// `sa_mask` and `sig` blocked and, for an `SA_SIGINFO` action, the original
/// `info` and `ctx`, which carry the fault address. This is not a replay of
/// kernel delivery: `SA_NODEFER` is not honoured, an `SA_RESETHAND` action is
/// not reinstalled, and a later fatal signal that finds the call already
/// claimed does not wait for it to return.
pub(super) fn chain_previous(sig: libc::c_int, info: *mut libc::siginfo_t, ctx: *mut libc::c_void) {
    let Some(previous) = slot_for(sig) else {
        // When: slot_for(sig) finds no slot, so sig is not a covered fatal signal
        // and there is no recorded action to call.
        return;
    };
    let Some((action, callable)) = previous.recorded() else {
        // When: previous.recorded() holds no function, so the recorded action is
        // missing, SIG_DFL, SIG_IGN, or this handler, and nothing is called.
        return;
    };
    if !previous.claim() {
        // When: previous.claim() fails because an earlier entry already called the
        // action, so a one-shot action is never entered twice.
        return;
    }
    block(action.sa_mask, sig);
    // SAFETY: `classify` yields `SigInfo` only for a function address recorded
    // with `SA_SIGINFO`, which takes `(sig, info, ctx)`, and `Handler` only for a
    // function address that takes the signal number alone.
    unsafe {
        match callable {
            Callable::SigInfo(handler) => {
                std::mem::transmute::<libc::sighandler_t, SigInfoAction>(handler)(sig, info, ctx)
            }
            Callable::Handler(handler) => {
                std::mem::transmute::<libc::sighandler_t, HandlerAction>(handler)(sig)
            }
        }
    }
}

/// Add `mask` and `sig` to this thread's blocked set. `sig` is added explicitly
/// because POSIX lets an `SA_RESETHAND` action be delivered with it unblocked.
fn block(mut mask: libc::sigset_t, sig: libc::c_int) {
    // SAFETY: `sigaddset` and `pthread_sigmask` are async-signal-safe and only
    // touch the local `mask`; a null old-set pointer means the previous mask is
    // not reported.
    unsafe {
        libc::sigaddset(&mut mask, sig);
        libc::pthread_sigmask(libc::SIG_BLOCK, &mask, std::ptr::null_mut());
    }
}

/// The `PREVIOUS_ACTIONS` slot for `sig`, when it is a covered fatal signal.
pub(super) fn slot_for(sig: libc::c_int) -> Option<&'static Previous> {
    FATAL_SIGNALS
        .iter()
        .position(|&fatal| fatal == sig)
        .and_then(|index| PREVIOUS_ACTIONS.get(index))
}

/// End the process by `sig`'s default action. The default is installed
/// explicitly because macOS keeps a SIGILL handler despite `SA_RESETHAND`, so
/// a bare re-raise would re-enter this handler forever. When `sig` is blocked
/// the raised signal is delivered once the handler returns; otherwise it ends
/// the process at once. Either way the recorded signal information describes
/// this raise, not the original fault.
fn terminate_by_default(sig: libc::c_int) {
    set_default(sig);
    // SAFETY: `raise` only delivers `sig` to this thread, whose disposition is
    // now `SIG_DFL`.
    unsafe {
        libc::raise(sig);
    }
}
