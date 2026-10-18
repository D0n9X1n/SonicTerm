//! Measurement-only probe for Windows' Default terminal application handoff.
//!
//! The probe is an out-of-process COM server for a test CLSID that implements
//! `ITerminalHandoff3`. When conhost delegates a console launch to it, the probe
//! records what the handoff carried, creates the IO pipes, and drives a scripted
//! session (Unicode echo, resize, Ctrl+C, `exit`) through them without a window.
//! `scripts/defterm-handoff-lab.ps1` registers it, routes `%%Startup` to it, and
//! restores the user's console delegation afterwards. It never ships.

// GUI subsystem: a console-subsystem COM server would get its own console, whose
// delegation would activate this same CLSID while the probe is still starting.
#![cfg_attr(windows, windows_subsystem = "windows")]

#[cfg(not(windows))]
fn main() {}

#[cfg(windows)]
fn main() -> windows_core::Result<()> {
    probe::run()
}

#[cfg(windows)]
// The COM method name and arity are fixed by the `ITerminalHandoff3` IDL, and the
// interface macro rejects lint attributes on the trait, so the allow sits here.
#[allow(non_snake_case, clippy::too_many_arguments)]
mod probe {
    use std::fs::OpenOptions;
    use std::io::Write as _;
    use std::path::PathBuf;
    use std::sync::mpsc::{channel, Sender};
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

    use windows::core::{IUnknown, Interface, Ref, GUID, HRESULT, PWSTR};
    use windows::Win32::Foundation::{
        CloseHandle, DuplicateHandle, DUPLICATE_SAME_ACCESS, E_FAIL, HANDLE, WAIT_OBJECT_0,
    };
    use windows::Win32::Storage::FileSystem::{ReadFile, WriteFile};
    use windows::Win32::System::Com::{
        CoInitializeEx, CoRegisterClassObject, CoRevokeClassObject, IClassFactory,
        IClassFactory_Impl, CLSCTX_LOCAL_SERVER, COINIT_MULTITHREADED, REGCLS_MULTIPLEUSE,
    };
    use windows::Win32::System::Pipes::CreatePipe;
    use windows::Win32::System::Threading::{
        GetCurrentProcess, GetCurrentProcessId, GetExitCodeProcess, GetProcessId,
        QueryFullProcessImageNameW, WaitForSingleObject, PROCESS_NAME_WIN32,
    };
    // The `interface` macro names the parent vtable by its bare identifier.
    #[allow(unused_imports)]
    use windows::core::IUnknown_Vtbl;
    use windows_core::{implement, interface, BOOL};

    /// Test CLSID the lab script registers under HKCU; never a shipping identity.
    const PROBE_CLSID: GUID = GUID::from_u128(0xa3d9e8f1_6b2c_4e7a_9c15_2f8b7d4e0a61);

    /// ConPTY signal-pipe opcode for a resize packet (`PTY_SIGNAL_RESIZE_WINDOW`).
    const PTY_SIGNAL_RESIZE_WINDOW: u16 = 8;

    /// Pipe buffer size Windows Terminal also uses for handoff sessions.
    const PIPE_BUFFER_BYTES: u32 = 128 * 1024;

    /// Width and height the probe resizes the session to, in character cells.
    const RESIZED_COLUMNS: u16 = 100;
    const RESIZED_ROWS: u16 = 30;

    /// Text echoed through the session to check UTF-8 input and output.
    const UNICODE_MARKER: &str = "SONIC_MARK_A 日本語 ✓";

    /// `TERMINAL_STARTUP_INFO` from microsoft/terminal `ITerminalHandoff.idl`.
    #[repr(C)]
    pub struct TerminalStartupInfo {
        title_bstr: *const u16,
        icon_path_bstr: *const u16,
        icon_index: i32,
        pos_x: u32,
        pos_y: u32,
        size_x: u32,
        size_y: u32,
        count_chars_x: u32,
        count_chars_y: u32,
        fill_attribute: u32,
        flags: u32,
        show_window: u16,
    }

    /// `ITerminalHandoff3`: the terminal creates the IO pipes and returns the
    /// console-side ends; the signal, reference and process handles are borrowed.
    #[interface("6F23DA90-15C5-4203-9DB0-64E73F1B1B00")]
    unsafe trait ITerminalHandoff3: IUnknown {
        fn EstablishPtyHandoff(
            &self,
            input: *mut HANDLE,
            output: *mut HANDLE,
            signal: HANDLE,
            reference: HANDLE,
            server: HANDLE,
            client: HANDLE,
            startup_info: *const TerminalStartupInfo,
        ) -> HRESULT;
    }

    /// What the probe does when conhost calls `EstablishPtyHandoff`.
    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    enum Mode {
        /// Accept the handoff and drive the scripted session.
        Success,
        /// Return `E_FAIL` without creating pipes.
        Fail,
        /// Block inside the call for the timeout, then return `E_FAIL`.
        Hang,
        /// Accept, wait for the first prompt, then abort the probe process.
        CrashAfter,
        /// Accept, wait for the first prompt, then close the session like a closed window.
        CloseAfter,
        /// Exit before registering the class object (activation failure).
        ExitEarly,
    }

    /// Settings shared by the class factory and every handoff object.
    struct ProbeState {
        log_path: PathBuf,
        mode: Mode,
        timeout: Duration,
        session_done: Mutex<Option<Sender<()>>>,
    }

    impl ProbeState {
        /// Appends one `key=value` line, prefixed with a wall-clock millisecond stamp.
        fn log(&self, event: &str, fields: &str) {
            let stamp_ms = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|elapsed| elapsed.as_millis())
                .unwrap_or_default();
            // When: logging fails the probe still answers conhost; the lab reads gaps as missing events.
            if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(&self.log_path)
            {
                let _ = writeln!(file, "{stamp_ms} event={event} {fields}");
            }
        }
    }

    /// A handle the probe owns and closes exactly once.
    struct OwnedHandle(HANDLE);

    // SAFETY: kernel handles are process-wide values usable from any thread.
    unsafe impl Send for OwnedHandle {}
    // SAFETY: the session only performs thread-safe kernel calls through a shared handle.
    unsafe impl Sync for OwnedHandle {}

    impl OwnedHandle {
        /// Duplicates a borrowed handle so it outlives the COM call that lent it.
        fn duplicate(borrowed: HANDLE) -> windows_core::Result<Self> {
            let mut owned = HANDLE::default();
            // SAFETY: `borrowed` is valid for the duration of the COM call; the copy is ours.
            unsafe {
                DuplicateHandle(
                    GetCurrentProcess(),
                    borrowed,
                    GetCurrentProcess(),
                    &mut owned,
                    0,
                    false,
                    DUPLICATE_SAME_ACCESS,
                )?;
            }
            Ok(Self(owned))
        }
    }

    impl Drop for OwnedHandle {
        // Lifecycle: every duplicated or created handle closes once when its owner drops.
        fn drop(&mut self) {
            // SAFETY: the handle was duplicated or created by this process and is closed only here.
            unsafe {
                let _ = CloseHandle(self.0);
            }
        }
    }

    #[implement(IClassFactory)]
    struct ProbeFactory {
        state: Arc<ProbeState>,
    }

    impl IClassFactory_Impl for ProbeFactory_Impl {
        fn CreateInstance(
            &self,
            outer: Ref<IUnknown>,
            iid: *const GUID,
            object: *mut *mut core::ffi::c_void,
        ) -> windows_core::Result<()> {
            // SAFETY: COM passes a valid IID pointer to CreateInstance.
            let requested = unsafe { *iid };
            self.state.log("create_instance", &format!("iid={requested:?}"));
            // When: aggregation is not part of the handoff protocol, so refuse it as COM requires.
            if outer.is_some() {
                return Err(windows::Win32::Foundation::CLASS_E_NOAGGREGATION.into());
            }
            let handoff: IUnknown = ProbeHandoff { state: Arc::clone(&self.state) }.into();
            // SAFETY: `object` is the caller's out pointer for the requested interface.
            unsafe { handoff.query(iid, object).ok() }
        }

        fn LockServer(&self, _lock: BOOL) -> windows_core::Result<()> {
            Ok(())
        }
    }

    #[implement(ITerminalHandoff3)]
    struct ProbeHandoff {
        state: Arc<ProbeState>,
    }

    impl ITerminalHandoff3_Impl for ProbeHandoff_Impl {
        unsafe fn EstablishPtyHandoff(
            &self,
            input: *mut HANDLE,
            output: *mut HANDLE,
            signal: HANDLE,
            reference: HANDLE,
            server: HANDLE,
            client: HANDLE,
            startup_info: *const TerminalStartupInfo,
        ) -> HRESULT {
            let state = &self.state;
            // SAFETY: conhost passes valid process handles and a startup-info pointer for this call.
            let (client_pid, server_pid, info) =
                unsafe { (GetProcessId(client), GetProcessId(server), &*startup_info) };
            state.log(
                "handoff",
                &format!(
                    "probe_pid={} client_pid={client_pid} server_pid={server_pid} client_image={:?} \
                     server_image={:?} title={:?} flags=0x{:x} show_window={} size_chars={}x{}",
                    // SAFETY: GetCurrentProcessId has no preconditions.
                    unsafe { GetCurrentProcessId() },
                    process_image(client),
                    process_image(server),
                    // SAFETY: the BSTR fields are valid or null for the duration of this call.
                    unsafe { bstr_to_string(info.title_bstr) },
                    info.flags,
                    info.show_window,
                    info.count_chars_x,
                    info.count_chars_y,
                ),
            );
            match state.mode {
                Mode::Fail => return E_FAIL,
                Mode::Hang => {
                    std::thread::sleep(state.timeout);
                    return E_FAIL;
                }
                Mode::Success | Mode::CrashAfter | Mode::CloseAfter | Mode::ExitEarly => {}
            }
            match start_session(state, signal, reference, server, client) {
                Ok((console_input, console_output)) => {
                    // SAFETY: the out pointers are valid; COM marshals the handles and then closes our copies.
                    unsafe {
                        *input = console_input;
                        *output = console_output;
                    }
                    HRESULT(0)
                }
                Err(error) => {
                    state.log("handoff_error", &format!("error={error:?}"));
                    error.code()
                }
            }
        }
    }

    /// Owned handles one handoff session drives.
    struct Session {
        state: Arc<ProbeState>,
        input_write: OwnedHandle,
        output_read: OwnedHandle,
        signal: OwnedHandle,
        reference: OwnedHandle,
        server: OwnedHandle,
        client: OwnedHandle,
    }

    /// Duplicates the borrowed handles, creates the IO pipes, and starts the
    /// session thread. Returns the console-side pipe ends for conhost.
    fn start_session(
        state: &Arc<ProbeState>,
        signal: HANDLE,
        reference: HANDLE,
        server: HANDLE,
        client: HANDLE,
    ) -> windows_core::Result<(HANDLE, HANDLE)> {
        let signal = OwnedHandle::duplicate(signal)?;
        let reference = OwnedHandle::duplicate(reference)?;
        let server = OwnedHandle::duplicate(server)?;
        let client = OwnedHandle::duplicate(client)?;
        let mut input_read = HANDLE::default();
        let mut input_write = HANDLE::default();
        let mut output_read = HANDLE::default();
        let mut output_write = HANDLE::default();
        // SAFETY: the out pointers are valid locals; ownership is split below.
        unsafe {
            CreatePipe(&mut input_read, &mut input_write, None, PIPE_BUFFER_BYTES)?;
            CreatePipe(&mut output_read, &mut output_write, None, PIPE_BUFFER_BYTES)?;
        }
        let session = Session {
            state: Arc::clone(state),
            input_write: OwnedHandle(input_write),
            output_read: OwnedHandle(output_read),
            signal,
            reference,
            server,
            client,
        };
        std::thread::spawn(move || drive_session(session));
        Ok((input_read, output_write))
    }

    /// Reads the image path of a process handle for the log.
    fn process_image(process: HANDLE) -> String {
        let mut buffer = [0u16; 1024];
        let mut length = buffer.len() as u32;
        // SAFETY: `buffer` holds `length` UTF-16 units and outlives the call.
        let queried = unsafe {
            QueryFullProcessImageNameW(
                process,
                PROCESS_NAME_WIN32,
                PWSTR(buffer.as_mut_ptr()),
                &mut length,
            )
        };
        match queried {
            Ok(()) => String::from_utf16_lossy(&buffer[..length as usize]),
            Err(error) => format!("<{:?}>", error.code()),
        }
    }

    /// Copies a borrowed BSTR, whose byte length precedes its first character.
    ///
    /// # Safety
    /// `bstr` must be null or point at a live BSTR.
    unsafe fn bstr_to_string(bstr: *const u16) -> String {
        if bstr.is_null() {
            return String::new();
        }
        // SAFETY: a BSTR stores its byte length in the u32 before the character data.
        let byte_len = unsafe { *(bstr as *const u32).sub(1) } as usize;
        // SAFETY: the BSTR holds `byte_len / 2` UTF-16 units starting at `bstr`.
        let units = unsafe { std::slice::from_raw_parts(bstr, byte_len / 2) };
        String::from_utf16_lossy(units)
    }

    /// Writes bytes to a pipe; failures are logged by the caller's later checks.
    fn write_all(handle: &OwnedHandle, bytes: &[u8]) -> bool {
        let mut written = 0u32;
        // SAFETY: the handle is an open pipe write end and `bytes` outlives the call.
        unsafe { WriteFile(handle.0, Some(bytes), Some(&mut written), None).is_ok() }
    }

    /// Shared output transcript plus the offset already scanned for terminal queries.
    struct Transcript {
        bytes: Vec<u8>,
        scanned: usize,
    }

    /// Counts non-overlapping occurrences of `needle` in `haystack`.
    fn count_occurrences(haystack: &[u8], needle: &[u8]) -> usize {
        haystack.windows(needle.len()).filter(|window| *window == needle).count()
    }

    /// Runs one handoff session: output reader, scripted input, exit checks.
    fn drive_session(session: Session) {
        let Session { state, input_write, output_read, signal, reference, server, client } =
            session;
        let input_write = Arc::new(input_write);
        let transcript = Arc::new(Mutex::new(Transcript { bytes: Vec::new(), scanned: 0 }));
        let reader = {
            let transcript = Arc::clone(&transcript);
            let responder = Arc::clone(&input_write);
            std::thread::spawn(move || read_output(&output_read, &transcript, &responder))
        };
        let started = Instant::now();
        let wait_for = |needle: &str, count: usize, limit: Duration| -> bool {
            let deadline = Instant::now() + limit;
            while Instant::now() < deadline {
                let seen =
                    transcript.lock().map(|log| count_occurrences(&log.bytes, needle.as_bytes()));
                // When: the needle reached the wanted count, so the scripted step is observed.
                if seen.unwrap_or_default() >= count {
                    return true;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            false
        };

        let prompt_seen = wait_for(">", 1, Duration::from_secs(10));
        state.log(
            "prompt",
            &format!("seen={prompt_seen} after_ms={}", started.elapsed().as_millis()),
        );
        // When: crash-after mode tests what conhost and the client do when the terminal dies mid-session.
        if state.mode == Mode::CrashAfter {
            state.log("crash", "abort=true");
            std::process::abort();
        }
        // When: close-after mode closes the session the way a terminal closes a window,
        // by releasing the signal and reference handles, then records how the client ends.
        if state.mode == Mode::CloseAfter {
            drop(signal);
            drop(reference);
            let closed_at = Instant::now();
            // SAFETY: `client` is an owned process handle.
            let client_wait = unsafe { WaitForSingleObject(client.0, 10_000) };
            let mut exit_code = u32::MAX;
            // SAFETY: `client` is an owned process handle with query rights.
            let _ = unsafe { GetExitCodeProcess(client.0, &mut exit_code) };
            // SAFETY: `server` is an owned process handle.
            let server_wait = unsafe { WaitForSingleObject(server.0, 10_000) };
            state.log(
                "close_done",
                &format!(
                    "client_exited={} exit_code={exit_code} server_exited={} after_ms={}",
                    client_wait == WAIT_OBJECT_0,
                    server_wait == WAIT_OBJECT_0,
                    closed_at.elapsed().as_millis()
                ),
            );
            let done = state.session_done.lock().ok().and_then(|mut sender| sender.take());
            if let Some(done) = done {
                let _ = done.send(());
            }
            return;
        }

        write_all(&input_write, b"mode con\r");
        let size_before = wait_for("Columns:", 1, Duration::from_secs(5));

        write_all(&input_write, format!("echo {UNICODE_MARKER}\r").as_bytes());
        let unicode_ok = wait_for(UNICODE_MARKER, 2, Duration::from_secs(5));
        state.log("unicode", &format!("echoed={unicode_ok}"));

        let mut packet = Vec::with_capacity(6);
        for word in [PTY_SIGNAL_RESIZE_WINDOW, RESIZED_COLUMNS, RESIZED_ROWS] {
            packet.extend_from_slice(&word.to_le_bytes());
        }
        let signal_written = write_all(&signal, &packet);
        write_all(&input_write, b"mode con\r");
        let resized_text = format!("Columns:        {RESIZED_COLUMNS}");
        let resize_ok = wait_for(&resized_text, 1, Duration::from_secs(5));
        state.log(
            "resize",
            &format!("size_before_seen={size_before} signal_written={signal_written} columns_{RESIZED_COLUMNS}_seen={resize_ok}"),
        );

        write_all(&input_write, b"ping -n 30 127.0.0.1\r");
        let ping_started = wait_for("Reply from", 1, Duration::from_secs(10));
        write_all(&input_write, b"\x03");
        let interrupted_at = Instant::now();
        write_all(&input_write, b"echo SONIC_MARK_C\r");
        let ctrl_c_ok = wait_for("SONIC_MARK_C", 2, Duration::from_secs(8));
        state.log(
            "ctrl_c",
            &format!(
                "ping_started={ping_started} prompt_back={ctrl_c_ok} after_ms={}",
                interrupted_at.elapsed().as_millis()
            ),
        );

        // Record the errorlevel `exit` inherits, so the client's exit code can be read.
        write_all(&input_write, b"echo SONIC_ERRORLEVEL=%ERRORLEVEL%\r");
        wait_for("SONIC_ERRORLEVEL=", 2, Duration::from_secs(5));
        write_all(&input_write, b"exit\r");
        // SAFETY: `client` and `server` are owned process handles.
        let client_wait = unsafe { WaitForSingleObject(client.0, 10_000) };
        let mut exit_code = u32::MAX;
        // SAFETY: `client` is an owned process handle with query rights.
        let _ = unsafe { GetExitCodeProcess(client.0, &mut exit_code) };
        state.log(
            "client_exit",
            &format!("exited={} exit_code={exit_code}", client_wait == WAIT_OBJECT_0),
        );

        drop(signal);
        drop(reference);
        // SAFETY: `server` is an owned process handle.
        let server_wait = unsafe { WaitForSingleObject(server.0, 10_000) };
        state.log("server_exit", &format!("exited={}", server_wait == WAIT_OBJECT_0));
        drop(input_write);
        let _ = reader.join();

        let transcript_path = state.log_path.with_extension("transcript.txt");
        // When: the transcript is evidence only; the summary line below still reports the session.
        if let Ok(log) = transcript.lock() {
            let _ = std::fs::write(&transcript_path, &log.bytes);
        }
        state.log(
            "session_done",
            &format!("elapsed_ms={} transcript={transcript_path:?}", started.elapsed().as_millis()),
        );
        // When: the server thread is waiting for this session; a missing receiver means it already timed out.
        let done = state.session_done.lock().ok().and_then(|mut sender| sender.take());
        if let Some(done) = done {
            let _ = done.send(());
        }
    }

    /// Copies console output into the transcript and answers the cursor-position
    /// and device-attribute queries ConPTY sends at startup.
    fn read_output(output: &OwnedHandle, transcript: &Mutex<Transcript>, responder: &OwnedHandle) {
        let mut buffer = vec![0u8; 16 * 1024];
        loop {
            let mut read = 0u32;
            // SAFETY: `buffer` outlives the call and the handle is an open pipe read end.
            let ok = unsafe { ReadFile(output.0, Some(&mut buffer), Some(&mut read), None) };
            // When: a failed or empty read means conhost closed its end of the output pipe.
            if ok.is_err() || read == 0 {
                return;
            }
            let Ok(mut log) = transcript.lock() else { return };
            log.bytes.extend_from_slice(&buffer[..read as usize]);
            let pending = log.bytes[log.scanned..].to_vec();
            log.scanned = log.bytes.len();
            drop(log);
            for _ in 0..count_occurrences(&pending, b"\x1b[6n") {
                write_all(responder, b"\x1b[1;1R");
            }
            for _ in 0..count_occurrences(&pending, b"\x1b[c") {
                write_all(responder, b"\x1b[?61;6;7;22;23;24;28;32;42c");
            }
        }
    }

    /// Parses `--mode`, `--log`, `--timeout-s` and `--clsid`; COM appends `-Embedding`.
    fn parse_args() -> (Mode, PathBuf, Duration, GUID) {
        let mut clsid = PROBE_CLSID;
        let mut mode = Mode::Success;
        let mut log_path = std::env::temp_dir().join("sonicterm-defterm-probe.log");
        let mut timeout = Duration::from_secs(60);
        let mut args = std::env::args().skip(1);
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--mode" => {
                    mode = match args.next().as_deref() {
                        Some("fail") => Mode::Fail,
                        Some("hang") => Mode::Hang,
                        Some("crash-after") => Mode::CrashAfter,
                        Some("close-after") => Mode::CloseAfter,
                        Some("exit-early") => Mode::ExitEarly,
                        _ => Mode::Success,
                    }
                }
                "--log" => {
                    if let Some(path) = args.next() {
                        log_path = PathBuf::from(path);
                    }
                }
                "--timeout-s" => {
                    if let Some(seconds) = args.next().and_then(|value| value.parse().ok()) {
                        timeout = Duration::from_secs(seconds);
                    }
                }
                "--clsid" => {
                    // When: a packaged test identity serves its own class, distinct from the lab's HKCU class.
                    if let Some(parsed) = args
                        .next()
                        .and_then(|value| GUID::try_from(value.trim_matches(['{', '}'])).ok())
                    {
                        clsid = parsed;
                    }
                }
                _ => {}
            }
        }
        (mode, log_path, timeout, clsid)
    }

    /// Registers the probe class object and serves one handoff or the timeout.
    pub fn run() -> windows_core::Result<()> {
        let (mode, log_path, timeout, clsid) = parse_args();
        let (done_sender, done_receiver) = channel();
        let state = Arc::new(ProbeState {
            log_path,
            mode,
            timeout,
            session_done: Mutex::new(Some(done_sender)),
        });
        state.log(
            "server_started",
            &format!(
                "pid={} mode={mode:?} args={:?}",
                std::process::id(),
                std::env::args().skip(1).collect::<Vec<_>>()
            ),
        );
        // When: exit-early mode measures conhost when the activated server never registers its class.
        if mode == Mode::ExitEarly {
            state.log("server_exit_early", "");
            return Ok(());
        }
        state.log("com_init", "");
        // SAFETY: called once on this thread before any other COM use.
        let initialized = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED).ok() };
        // When: a COM setup failure would otherwise end the probe without any log line.
        if let Err(error) = initialized {
            state.log("com_init_error", &format!("error={error:?}"));
            return Err(error);
        }
        let factory: IClassFactory = ProbeFactory { state: Arc::clone(&state) }.into();
        let factory_unknown: IUnknown = factory.cast()?;
        state.log("registering", &format!("clsid={clsid:?}"));
        // SAFETY: the factory stays alive until the registration is revoked below.
        let registered = unsafe {
            CoRegisterClassObject(&clsid, &factory_unknown, CLSCTX_LOCAL_SERVER, REGCLS_MULTIPLEUSE)
        };
        let cookie = match registered {
            Ok(cookie) => cookie,
            Err(error) => {
                state.log("register_error", &format!("error={error:?}"));
                return Err(error);
            }
        };
        state.log("registered", &format!("cookie={cookie} clsid={clsid:?}"));
        // The hang mode blocks inside the call, so the server waits a little beyond it.
        let served = done_receiver.recv_timeout(timeout + Duration::from_secs(30)).is_ok();
        state.log("server_stopping", &format!("session_completed={served}"));
        // SAFETY: `cookie` came from the successful registration above.
        unsafe { CoRevokeClassObject(cookie)? };
        Ok(())
    }
}
