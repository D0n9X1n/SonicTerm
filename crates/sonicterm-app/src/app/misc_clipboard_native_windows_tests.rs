//! Native Windows clipboard paste through the configured action into real PTYs, inside a private window station.

use super::*;
use crate::app::{
    mod_tests::{input_test_app, InputTestApp, PtySubmissions},
    pty_test_support::isolated,
};
use sonicterm_cfg::keymap::Action;
use sonicterm_ui::{copy_mode::CopyModeState, search::SearchState};
use windows::{
    core::{HSTRING, PCWSTR},
    Win32::{
        Foundation::{GlobalFree, GENERIC_ALL, HANDLE, HGLOBAL},
        System::{
            DataExchange::{
                CloseClipboard, CountClipboardFormats, OpenClipboard, SetClipboardData,
            },
            Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE},
            StationsAndDesktops::{
                CloseDesktop, CloseWindowStation, CreateDesktopW, CreateWindowStationW,
                GetProcessWindowStation, GetThreadDesktop, SetProcessWindowStation,
                SetThreadDesktop, DESKTOP_CONTROL_FLAGS, HDESK, HWINSTA,
            },
            Threading::GetCurrentThreadId,
        },
        UI::WindowsAndMessaging::CWF_CREATE_ONLY,
    },
};

const CF_UNICODETEXT: u32 = 13;
const MARKER: &str = "SONICTERM-CLIPBOARD-CONTENT-MARKER";

/// The process and this thread use a private window station and desktop, whose clipboard is not the user's.
struct PrivateStation {
    original_station: HWINSTA,
    original_desktop: HDESK,
    station: Option<HWINSTA>,
    desktop: Option<HDESK>,
}

impl PrivateStation {
    /// Enter a new, uniquely named station and desktop, panicking rather than falling back to the user's clipboard.
    fn enter() -> Self {
        let (original_station, original_desktop) =
            // SAFETY: both queries only return handles the system owns for this process and thread; nothing is closed.
            unsafe {
                (
                    GetProcessWindowStation().expect("current window station"),
                    GetThreadDesktop(GetCurrentThreadId()).expect("current thread desktop"),
                )
            };
        let mut entered = Self { original_station, original_desktop, station: None, desktop: None };
        let name = HSTRING::from(format!("SonicTermClipboardTest{}", std::process::id()));
        let station =
            // SAFETY: name is a live NUL-terminated string; CWF_CREATE_ONLY refuses to open an existing station.
            unsafe { CreateWindowStationW(&name, CWF_CREATE_ONLY, GENERIC_ALL.0, None) }
                .expect("create private window station");
        entered.station = Some(station);
        // SAFETY: station is the live handle created above; Drop restores the original before closing it.
        unsafe { SetProcessWindowStation(station) }.expect("assign private window station");
        let desktop_name = HSTRING::from("Default");
        let desktop =
            // SAFETY: the desktop is created on the process's private station; no device or mode is requested.
            unsafe {
                CreateDesktopW(
                    &desktop_name,
                    PCWSTR::null(),
                    None,
                    DESKTOP_CONTROL_FLAGS(0),
                    GENERIC_ALL.0,
                    None,
                )
            }
            .expect("create private desktop");
        entered.desktop = Some(desktop);
        // SAFETY: this fresh test thread owns no windows or hooks, so its desktop may change; Drop restores it.
        unsafe { SetThreadDesktop(desktop) }.expect("assign private desktop");
        entered.verify();
        entered
    }

    /// Prove the assignment took effect and that the private clipboard starts empty.
    fn verify(&self) {
        let (station, desktop, formats) =
            // SAFETY: the queries return system-owned handles that are compared, never closed.
            unsafe {
                (
                    GetProcessWindowStation().expect("current window station"),
                    GetThreadDesktop(GetCurrentThreadId()).expect("current thread desktop"),
                    CountClipboardFormats(),
                )
            };
        assert_eq!(Some(station), self.station, "process is not on the private window station");
        assert_eq!(Some(desktop), self.desktop, "thread is not on the private desktop");
        assert_eq!(formats, 0, "a new window station starts with an empty clipboard");
    }
}

impl PrivateStation {
    /// Restore the original process station, then the thread desktop that belongs to it, then close the private handles.
    fn restore(&mut self) -> windows::core::Result<()> {
        // SAFETY: original_station was valid at entry and is never closed; it is restored first so the original desktop is on it.
        unsafe { SetProcessWindowStation(self.original_station) }?;
        // SAFETY: original_desktop was valid at entry and is never closed by this process.
        unsafe { SetThreadDesktop(self.original_desktop) }?;
        if let Some(desktop) = self.desktop.take() {
            // SAFETY: desktop was created by enter() and is no longer assigned to this thread.
            unsafe { CloseDesktop(desktop) }?;
        }
        if let Some(station) = self.station.take() {
            // SAFETY: station was created by enter() and is no longer assigned to this process.
            unsafe { CloseWindowStation(station) }?;
        }
        Ok(())
    }

    /// Leave the private station on the success path, proving the originals are back and both private handles closed.
    fn leave(mut self) {
        self.restore().expect("restore original window station and desktop");
        let (station, desktop) =
            // SAFETY: the queries return system-owned handles that are compared, never closed.
            unsafe {
                (
                    GetProcessWindowStation().expect("current window station"),
                    GetThreadDesktop(GetCurrentThreadId()).expect("current thread desktop"),
                )
            };
        assert_eq!(station, self.original_station, "process station was not restored");
        assert_eq!(desktop, self.original_desktop, "thread desktop was not restored");
        assert!(self.desktop.is_none() && self.station.is_none(), "private handles stay open");
    }
}

// Lifecycle: PrivateStation restores the originals best-effort on unwind; it never panics, so an assertion failure is not doubled.
impl Drop for PrivateStation {
    fn drop(&mut self) {
        let _ = self.restore();
    }
}

/// An open clipboard closes on every exit, including assertion unwinding.
struct OpenedClipboard;

impl OpenedClipboard {
    fn open() -> Self {
        // SAFETY: a null owner opens the current station's clipboard for this thread until Drop closes it.
        unsafe { OpenClipboard(None) }.expect("open private clipboard");
        Self
    }
}

// Lifecycle: OpenedClipboard closes the clipboard it opened so later native reads can open it.
impl Drop for OpenedClipboard {
    fn drop(&mut self) {
        let _ =
            // SAFETY: this thread opened the clipboard in OpenedClipboard::open.
            unsafe { CloseClipboard() };
    }
}

/// Add Unicode text beside the clipboard's current formats without emptying it, so a file list stays offered.
fn add_unicode_text_keeping_files(text: &str) {
    let wide: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
    let byte_count = wide.len() * std::mem::size_of::<u16>();
    let _opened = OpenedClipboard::open();
    let memory: HGLOBAL =
        // SAFETY: a moveable allocation of byte_count bytes; ownership passes to the clipboard only on success.
        unsafe { GlobalAlloc(GMEM_MOVEABLE, byte_count) }.expect("allocate clipboard text");
    let base =
        // SAFETY: memory is a live moveable allocation, so GlobalLock returns its base or null.
        unsafe { GlobalLock(memory) }.cast::<u16>();
    assert!(!base.is_null(), "lock clipboard text");
    // SAFETY: base points at byte_count writable bytes, exactly wide.len() u16 units, not overlapping wide.
    unsafe { std::ptr::copy_nonoverlapping(wide.as_ptr(), base, wide.len()) };
    let _ =
        // SAFETY: memory is locked once above; an Err only reports the lock count reaching zero.
        unsafe { GlobalUnlock(memory) };
    let offered =
        // SAFETY: the clipboard is open on this thread; on success the system owns memory and frees it.
        unsafe { SetClipboardData(CF_UNICODETEXT, Some(HANDLE(memory.0))) };
    if let Err(error) = offered {
        let _ =
            // SAFETY: SetClipboardData failed, so this process still owns memory and must free it.
            unsafe { GlobalFree(Some(memory)) };
        panic!("offer Unicode text beside files: {error}");
    }
}

/// Native CF_HDROP and CF_UNICODETEXT reach real cmd.exe PTYs only as quoted paths or unchanged text through the configured paste action.
#[test]
fn native_clipboard_paste_action_reaches_real_pty() {
    if isolated() {
        return;
    }
    // Every native clipboard and window call below runs inside this station, never the interactive one.
    let station = PrivateStation::enter();
    let action = Keymap::bundled_default()
        .lookup("ctrl+shift+v")
        .cloned()
        .expect("configured paste binding");
    assert_eq!(action, Action::PasteFromClipboard);
    let fixtures = tempfile::Builder::new().prefix("sonicterm-clipboard-").tempdir().unwrap();
    let files = [fixtures.path().join("report one.txt"), fixtures.path().join("notes.md")];
    for file in &files {
        std::fs::write(file, MARKER).unwrap();
    }
    // The native file list carries final DOS paths; canonicalize and strip the verbatim prefix to match it.
    let quoted = files
        .iter()
        .map(|file| {
            let canonical = std::fs::canonicalize(file).unwrap();
            let display = canonical.to_str().unwrap().trim_start_matches(r"\\?\").to_owned();
            format!("\"{display}\"")
        })
        .collect::<Vec<_>>()
        .join(" ");
    let mut seed = arboard::Clipboard::new().unwrap();
    let (mut app, window, pane) = input_test_app();
    assert!(app.test_clipboard_text.is_none() && app.test_clipboard_files.is_none());
    app.__test_enable_pty_write_log();
    let paste = |app: &mut InputTestApp| {
        app.wait_for_input_queues();
        let submitted = PtySubmissions::start();
        assert!(app.run_action_for_window(&action, window));
        let sources = std::mem::take(&mut *app.test_pty_write_sources.borrow_mut());
        (submitted.take(), sources)
    };

    // Files only, with no Unicode text offered: one paste of quoted paths, never contents, never Enter.
    seed.clear().unwrap();
    seed.set().file_list(&files).unwrap();
    assert!(seed.get_text().is_err(), "files-only clipboard offers no Unicode text");
    let (submitted, sources) = paste(&mut app);
    assert_eq!(submitted, [(pane, quoted.clone().into_bytes())]);
    assert_eq!(sources, [(pane, crate::app::PtyInputSource::Paste)]);
    let bytes = &submitted[0].1;
    assert!(!bytes.contains(&b'\r') && !bytes.contains(&b'\n'), "paths are not submitted");
    assert!(!String::from_utf8_lossy(bytes).contains(MARKER), "file contents never paste");

    // Files with competing Unicode text: the exact file list wins over the bare text form.
    seed.clear().unwrap();
    seed.set().file_list(&files).unwrap();
    add_unicode_text_keeping_files("report one.txt");
    assert_eq!(seed.get().file_list().unwrap().len(), files.len());
    assert_eq!(seed.get_text().unwrap(), "report one.txt");
    let (submitted, _) = paste(&mut app);
    assert_eq!(submitted, [(pane, quoted.clone().into_bytes())]);

    // Text only: the clipboard text reaches the PTY unchanged.
    seed.set_text("echo 你好é").unwrap();
    assert!(seed.get().file_list().is_err(), "text-only clipboard offers no file list");
    let (submitted, sources) = paste(&mut app);
    assert_eq!(submitted, [(pane, "echo 你好é".as_bytes().to_vec())]);
    assert_eq!(sources, [(pane, crate::app::PtyInputSource::Paste)]);

    // READONLY consumes the action before any native read reaches the PTY.
    seed.clear().unwrap();
    seed.set().file_list(&files).unwrap();
    app.windows.get_mut(&window).unwrap().copy_mode = Some(CopyModeState::read_only_at((0, 0)));
    let (submitted, sources) = paste(&mut app);
    assert!(submitted.is_empty() && sources.is_empty(), "READONLY pasted {submitted:?}");
    app.windows.get_mut(&window).unwrap().copy_mode = None;

    // An open search takes the native text only; the co-offered files reach neither its query nor the PTY.
    add_unicode_text_keeping_files("needle");
    app.windows.get_mut(&window).unwrap().tab_states[0].search = Some(SearchState::new());
    let (submitted, sources) = paste(&mut app);
    assert!(submitted.is_empty() && sources.is_empty(), "search pasted {submitted:?}");
    assert_eq!(app.windows[&window].tab_states[0].search.as_ref().unwrap().query, "needle");

    // Release the PTYs and clipboard handle before checking that the original station and desktop come back.
    drop(app);
    drop(seed);
    station.leave();
}
