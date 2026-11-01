//! Native macOS pasteboard paste through the configured action into a real `/bin/sh` PTY.
//!
//! The general pasteboard is shared with the signed-in user and with concurrent tests, so the test is
//! ignored: a serialized gate runs it by exact name, and it fails unless it runs on a
//! GitHub-hosted runner or `SONICTERM_NATIVE_CLIPBOARD_TEST=1` opts in locally.

use super::*;
use crate::app::{
    mod_tests::{input_test_app, InputTestApp, PtySubmissions},
    pty_test_support::{isolated_with_output, IsolationConfig},
    PtyInputSource,
};
use objc2::{rc::Retained, runtime::ProtocolObject};
use objc2_app_kit::{
    NSPasteboard, NSPasteboardItem, NSPasteboardTypeFileURL, NSPasteboardTypeString,
    NSPasteboardWriting,
};
use objc2_foundation::{NSArray, NSString, NSURL};
use sonicterm_cfg::keymap::Action;
use std::io::Write;

const MARKER: &str = "SONICTERM-CLIPBOARD-CONTENT-MARKER";
const OPT_IN: &str = "SONICTERM_NATIVE_CLIPBOARD_TEST";

/// Whether the shared pasteboard may be overwritten: a disposable GitHub-hosted runner, or an explicit local opt-in.
fn native_pasteboard_allowed() -> bool {
    let hosted = std::env::var("GITHUB_ACTIONS").as_deref() == Ok("true")
        && std::env::var("RUNNER_ENVIRONMENT").as_deref() == Ok("github-hosted");
    hosted || std::env::var(OPT_IN).as_deref() == Ok("1")
}

/// Write one marker line unbuffered past libtest capture, so CI logs show which process completed the native cases.
fn marker(line: &str) {
    let _ = std::io::stdout().write_all(format!("{line}\n").as_bytes());
}

/// Publish one pasteboard item per path carrying its file URL and a competing plain-text name, as Finder does.
fn publish_files_with_competing_text(paths: &[std::path::PathBuf], text: &str) {
    let (file_url_type, string_type) =
        // SAFETY: both are immutable AppKit string constants that live for the whole process.
        unsafe { (NSPasteboardTypeFileURL, NSPasteboardTypeString) };
    let competing = NSString::from_str(text);
    let items: Vec<Retained<ProtocolObject<dyn NSPasteboardWriting>>> = paths
        .iter()
        .map(|path| {
            let url = NSURL::fileURLWithPath(&NSString::from_str(path.to_str().unwrap()));
            let absolute = url.absoluteString().expect("file URL string");
            let item = NSPasteboardItem::new();
            assert!(item.setString_forType(&absolute, file_url_type), "file URL type");
            assert!(item.setString_forType(&competing, string_type), "competing text type");
            ProtocolObject::from_retained(item)
        })
        .collect();
    let pasteboard = NSPasteboard::generalPasteboard();
    pasteboard.clearContents();
    assert!(pasteboard.writeObjects(&NSArray::from_retained_slice(&items)), "write items");
}

/// Native file URLs and text reach a real POSIX shell PTY only as quoted paths or unchanged text through the configured paste action.
#[test]
#[ignore = "overwrites the general pasteboard; a serialized gate runs it by exact name"]
fn native_pasteboard_paste_action_reaches_real_pty() {
    // Fail closed before any child, App or pasteboard handle: an explicit run without admission must not pass.
    assert!(
        native_pasteboard_allowed(),
        "native pasteboard test overwrites the general pasteboard; set {OPT_IN}=1 to opt in locally \
         (GitHub-hosted runners are admitted by GITHUB_ACTIONS=true and RUNNER_ENVIRONMENT=github-hosted)"
    );
    // The exact-name child runs with --nocapture; on success its output, including its marker, is forwarded.
    if isolated_with_output(IsolationConfig::ORDINARY) {
        marker("NATIVE_PASTEBOARD PASS: exact-name child exercised files,mixed,text");
        return;
    }
    let action =
        Keymap::bundled_default().lookup("super+v").cloned().expect("configured paste binding");
    assert_eq!(action, Action::PasteFromClipboard);
    let fixtures = tempfile::Builder::new().prefix("sonicterm-clipboard-").tempdir().unwrap();
    let files = [fixtures.path().join("report one.txt"), fixtures.path().join("notes.md")];
    for file in &files {
        std::fs::write(file, MARKER).unwrap();
    }
    // arboard canonicalizes before writing file URLs, so /var temp paths come back as /private/var.
    let canonical: Vec<_> = files.iter().map(|file| std::fs::canonicalize(file).unwrap()).collect();
    let quoted = canonical
        .iter()
        .map(|path| {
            let display = path.to_str().unwrap();
            assert!(!display.contains('\''), "fixture path needs no POSIX quote escaping");
            format!("'{display}'")
        })
        .collect::<Vec<_>>()
        .join(" ");
    let mut seed = arboard::Clipboard::new().expect("native pasteboard");
    let (mut app, window, pane) = input_test_app();
    assert!(app.clipboard.is_some(), "App holds a native pasteboard handle");
    assert!(app.test_clipboard_text.is_none() && app.test_clipboard_files.is_none());
    app.__test_enable_pty_write_log();
    let paste = |app: &mut InputTestApp| {
        app.wait_for_input_queues();
        let submitted = PtySubmissions::start();
        assert!(app.run_action_for_window(&action, window));
        let sources = std::mem::take(&mut *app.test_pty_write_sources.borrow_mut());
        (submitted.take(), sources)
    };

    // Copied files: one paste of quoted POSIX paths, never contents, never Enter.
    seed.set().file_list(&files).unwrap();
    let (submitted, sources) = paste(&mut app);
    assert_eq!(submitted, [(pane, quoted.clone().into_bytes())]);
    assert_eq!(sources, [(pane, PtyInputSource::Paste)]);
    let bytes = &submitted[0].1;
    assert!(!bytes.contains(&b'\r') && !bytes.contains(&b'\n'), "paths are not submitted");
    assert!(!String::from_utf8_lossy(bytes).contains(MARKER), "file contents never paste");

    // File URLs with competing text in the same items: the native read sees both, and the paths win.
    publish_files_with_competing_text(&canonical, "competing name.txt");
    assert_eq!(seed.get().file_list().unwrap(), canonical);
    assert_eq!(seed.get_text().unwrap(), "competing name.txt");
    let (submitted, sources) = paste(&mut app);
    assert_eq!(submitted, [(pane, quoted.into_bytes())]);
    assert_eq!(sources, [(pane, PtyInputSource::Paste)]);

    // Text only: the pasteboard text reaches the PTY unchanged.
    seed.set_text("echo 你好é").unwrap();
    assert!(seed.get().file_list().is_err(), "text-only pasteboard offers no file URLs");
    let (submitted, sources) = paste(&mut app);
    assert_eq!(submitted, [(pane, "echo 你好é".as_bytes().to_vec())]);
    assert_eq!(sources, [(pane, PtyInputSource::Paste)]);
    marker("NATIVE_PASTEBOARD EXERCISED cases=files,mixed,text");
}
