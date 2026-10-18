use super::*;
use std::sync::{Arc, Mutex};

/// The hidden open-script entry the packages ship.
const SHIPPED_ENTRY: &str = include_str!("../resources/com.d0n9x1n.SonicTerm.open-script.desktop");

type Reported = Arc<Mutex<Vec<(usize, HandlerOutcome)>>>;

/// A sink that records each outcome in arrival order.
fn recording_sink() -> (HandlerResultSink, Reported) {
    let reported: Reported = Arc::default();
    let store = Arc::clone(&reported);
    let sink = HandlerResultSink::new(
        1,
        Arc::new(move |_, index, outcome| store.lock().unwrap().push((index, outcome))),
    );
    (sink, reported)
}

/// Write an executable shell script named `name` into `dir`.
///
/// The executable is created by a separate `cp` process, never by a write handle in this test
/// process. Tests run in parallel and spawn children; a child forked while this process held the
/// script open for writing would keep that handle until it execs, and Linux then refuses to run
/// the script with "executable file busy" (ETXTBSY). Once `cp` exits, no write handle to the
/// executable exists anywhere. The source file is written here but never executed.
fn write_script(dir: &Path, name: &str, body: &str) -> PathBuf {
    let path = dir.join(name);
    let source = dir.join(format!("{name}.source"));
    std::fs::write(&source, format!("#!/bin/sh\n{body}\n")).unwrap();
    let copied = std::process::Command::new("/bin/cp").arg(&source).arg(&path).status().unwrap();
    assert!(copied.success(), "cp {} failed: {copied}", source.display());
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

/// A fake `xdg-mime` that logs each argv line to `log`, exits `default_exit` for `default`,
/// and answers `query default` with `answer`.
fn fake_xdg_mime(dir: &Path, default_exit: i32, answer: &str) -> (PathBuf, PathBuf) {
    let log = dir.join("argv.log");
    let body = format!(
        "echo \"$*\" >> '{log}'\n\
         if [ \"$1\" = default ]; then exit {default_exit}; fi\n\
         echo '{answer}'",
        log = log.display()
    );
    (write_script(dir, "xdg-mime", &body), log)
}

fn shell_script_types() -> Vec<String> {
    vec!["application/x-shellscript".to_string(), "text/x-shellscript".to_string()]
}

/// An environment whose data home holds `entry` text (if any) and whose PATH is `path_dir`.
fn environment_with(root: &Path, entry: Option<&str>, path_dir: &Path) -> XdgEnvironment {
    let applications = root.join("data/applications");
    std::fs::create_dir_all(&applications).unwrap();
    if let Some(text) = entry {
        std::fs::write(applications.join(OPEN_SCRIPT_DESKTOP_ID), text).unwrap();
    }
    XdgEnvironment {
        data_home: Some(root.join("data").into_os_string()),
        data_dirs: Some(root.join("none").into_os_string()),
        home: None,
        path: Some(path_dir.as_os_str().to_owned()),
    }
}

/// Contract: the data home comes first, then each data dir; unset values take the XDG
/// defaults and relative entries are ignored.
#[test]
fn application_dirs_follow_the_xdg_order_and_defaults() {
    let explicit = XdgEnvironment {
        data_home: Some("/data/home".into()),
        data_dirs: Some("/one:relative:/two".into()),
        home: Some("/home/user".into()),
        path: None,
    };
    assert_eq!(
        application_dirs(&explicit),
        vec![
            PathBuf::from("/data/home/applications"),
            PathBuf::from("/one/applications"),
            PathBuf::from("/two/applications"),
        ]
    );
    let defaults = XdgEnvironment {
        data_home: Some(OsString::new()),
        home: Some("/home/user".into()),
        ..XdgEnvironment::default()
    };
    assert_eq!(
        application_dirs(&defaults),
        vec![
            PathBuf::from("/home/user/.local/share/applications"),
            PathBuf::from("/usr/local/share/applications"),
            PathBuf::from("/usr/share/applications"),
        ]
    );
}

/// Contract: the shipped entry's id and MIME types are exactly what the command lists, so the
/// confirmation page cannot drift from the resource file.
#[test]
fn shipped_entry_claims_the_shell_script_types() {
    assert_eq!(mime_types_from_entry(SHIPPED_ENTRY), shell_script_types());
    let text = "[Other]\nMimeType=text/plain;\n[Desktop Entry]\nName=x\n";
    assert!(mime_types_from_entry(text).is_empty(), "only the main group's MimeType counts");
}

/// Contract: the entry is found in the first directory that holds it.
#[test]
fn find_desktop_entry_uses_the_first_directory_holding_it() {
    let root = tempfile::tempdir().unwrap();
    let first = root.path().join("first");
    let second = root.path().join("second");
    std::fs::create_dir_all(&first).unwrap();
    std::fs::create_dir_all(&second).unwrap();
    assert_eq!(find_desktop_entry(&[first.clone(), second.clone()]), None);
    std::fs::write(second.join(OPEN_SCRIPT_DESKTOP_ID), SHIPPED_ENTRY).unwrap();
    assert_eq!(
        find_desktop_entry(&[first, second.clone()]),
        Some(second.join(OPEN_SCRIPT_DESKTOP_ID))
    );
}

/// Contract: `xdg-mime` resolves only to an absolute, executable file; relative PATH entries
/// and non-executable files are skipped.
#[test]
fn resolve_executable_requires_an_absolute_executable() {
    let root = tempfile::tempdir().unwrap();
    let plain = root.path().join("plain");
    let runnable = root.path().join("runnable");
    std::fs::create_dir_all(&plain).unwrap();
    std::fs::create_dir_all(&runnable).unwrap();
    std::fs::write(plain.join("xdg-mime"), "x").unwrap();
    let script = write_script(&runnable, "xdg-mime", "exit 0");
    let path = std::env::join_paths([PathBuf::from("relative"), plain, runnable]).unwrap();
    assert_eq!(resolve_executable("xdg-mime", Some(&path)), Some(script));
    assert_eq!(resolve_executable("xdg-mime", None), None);
}

/// Contract: the argv is exact: `default <entry> <types…>` and `query default <type>`.
#[test]
fn argv_is_exact() {
    assert_eq!(
        default_args(&shell_script_types()),
        vec![
            "default",
            "com.d0n9x1n.SonicTerm.open-script.desktop",
            "application/x-shellscript",
            "text/x-shellscript"
        ]
    );
    assert_eq!(query_args("text/x-shellscript"), vec!["query", "default", "text/x-shellscript"]);
}

/// Contract: a missing entry refuses and points to Packaging; it never creates the file.
#[test]
fn plan_reports_a_missing_entry() {
    let root = tempfile::tempdir().unwrap();
    let environment = environment_with(root.path(), None, root.path());
    let mut backend = LinuxDefaultHandlers::new(environment, COMMAND_TIMEOUT);
    let refusal = backend.plan().unwrap_err();
    assert!(refusal.contains("not installed"), "{refusal}");
    assert!(refusal.contains("Packaging"), "{refusal}");
    assert!(!root.path().join("data/applications").join(OPEN_SCRIPT_DESKTOP_ID).exists());
}

/// Contract: an entry with no MimeType, or a host without `xdg-mime`, refuses with the reason.
#[test]
fn plan_reports_an_empty_entry_and_a_missing_utility() {
    let root = tempfile::tempdir().unwrap();
    let empty_path = root.path().join("bin");
    std::fs::create_dir_all(&empty_path).unwrap();
    let mut empty = LinuxDefaultHandlers::new(
        environment_with(root.path(), Some("[Desktop Entry]\nName=x\n"), &empty_path),
        COMMAND_TIMEOUT,
    );
    assert!(empty.plan().unwrap_err().contains("claims no MIME type"));
    let mut no_utility = LinuxDefaultHandlers::new(
        environment_with(root.path(), Some(SHIPPED_ENTRY), &empty_path),
        COMMAND_TIMEOUT,
    );
    assert_eq!(no_utility.plan().unwrap_err(), "xdg-mime not found (install xdg-utils).");
}

/// Contract: a ready plan lists the entry's MIME types and the desktop entry, and its report
/// notes say this is not the terminal and only show how to choose one.
#[test]
fn plan_lists_mime_types_and_terminal_instructions() {
    let root = tempfile::tempdir().unwrap();
    let bin = root.path().join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    fake_xdg_mime(&bin, 0, OPEN_SCRIPT_DESKTOP_ID);
    let mut backend = LinuxDefaultHandlers::new(
        environment_with(root.path(), Some(SHIPPED_ENTRY), &bin),
        COMMAND_TIMEOUT,
    );
    let plan = backend.plan().unwrap();
    assert_eq!(backend.platform(), "linux");
    assert_eq!(plan.mode, PlanMode::Change);
    assert_eq!(
        plan.items.iter().map(|item| (item.kind, item.name.as_str())).collect::<Vec<_>>(),
        vec![
            (HandlerKind::MimeType, "application/x-shellscript"),
            (HandlerKind::MimeType, "text/x-shellscript"),
        ]
    );
    assert!(plan.confirm_notes[0].contains(OPEN_SCRIPT_DESKTOP_ID));
    let notes = plan.report_notes.join("\n");
    assert!(notes.contains("not your terminal"));
    assert!(notes.contains("update-alternatives"));
    assert!(notes.contains("xdg-terminals.list"));
    assert!(notes.contains("KDE"));
    assert!(!notes.to_lowercase().contains("terminal default"));
}

/// Contract: Apply runs `xdg-mime default` once with the exact argv, then reads each type back,
/// and reports changed only when the answer names the open-script entry; it runs off the
/// calling thread.
#[test]
fn apply_sets_then_reads_back_each_type() {
    let root = tempfile::tempdir().unwrap();
    let bin = root.path().join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let (_, log) = fake_xdg_mime(&bin, 0, OPEN_SCRIPT_DESKTOP_ID);
    let mut backend = LinuxDefaultHandlers::new(
        environment_with(root.path(), Some(SHIPPED_ENTRY), &bin),
        COMMAND_TIMEOUT,
    );
    let plan = backend.plan().unwrap();
    let (sink, reported) = recording_sink();
    backend.apply(&plan, sink);
    let deadline = Instant::now() + Duration::from_secs(10);
    while reported.lock().unwrap().len() < 2 {
        assert!(Instant::now() < deadline, "the worker never reported");
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(
        *reported.lock().unwrap(),
        vec![(0, HandlerOutcome::Changed), (1, HandlerOutcome::Changed)]
    );
    assert_eq!(
        std::fs::read_to_string(log).unwrap(),
        "default com.d0n9x1n.SonicTerm.open-script.desktop application/x-shellscript text/x-shellscript\n\
         query default application/x-shellscript\n\
         query default text/x-shellscript\n"
    );
}

/// Contract: a nonzero `xdg-mime default` fails every type with its exit status and runs no
/// read-back.
#[test]
fn a_nonzero_exit_fails_every_type() {
    let root = tempfile::tempdir().unwrap();
    let (xdg_mime, log) = fake_xdg_mime(root.path(), 3, OPEN_SCRIPT_DESKTOP_ID);
    let (sink, reported) = recording_sink();
    apply_mime_defaults(&xdg_mime, &shell_script_types(), COMMAND_TIMEOUT, &sink);
    let failure = HandlerOutcome::Failed("xdg-mime default: exit 3".to_string());
    assert_eq!(*reported.lock().unwrap(), vec![(0, failure.clone()), (1, failure)]);
    assert_eq!(std::fs::read_to_string(log).unwrap().lines().count(), 1);
}

/// Contract: a read-back naming another program reports that answer as a failure, not a change.
#[test]
fn a_disagreeing_read_back_is_a_failure() {
    let root = tempfile::tempdir().unwrap();
    let (xdg_mime, _) = fake_xdg_mime(root.path(), 0, "org.gnome.TextEditor.desktop");
    let (sink, reported) = recording_sink();
    apply_mime_defaults(&xdg_mime, &shell_script_types()[..1], COMMAND_TIMEOUT, &sink);
    assert_eq!(
        *reported.lock().unwrap(),
        vec![(0, HandlerOutcome::Failed("read back \"org.gnome.TextEditor.desktop\"".into()))]
    );
}

/// Contract: a read-back that itself fails reports its status.
#[test]
fn a_failing_read_back_reports_its_status() {
    let root = tempfile::tempdir().unwrap();
    let xdg_mime = write_script(root.path(), "xdg-mime", "[ \"$1\" = default ] && exit 0\nexit 5");
    let (sink, reported) = recording_sink();
    apply_mime_defaults(&xdg_mime, &shell_script_types()[..1], COMMAND_TIMEOUT, &sink);
    assert_eq!(
        *reported.lock().unwrap(),
        vec![(0, HandlerOutcome::Failed("read-back: exit 5".into()))]
    );
}

/// Whether process `pid` still exists (a zombie counts until it is reaped).
fn process_exists(pid: libc::pid_t) -> bool {
    // SAFETY: `kill` with signal 0 takes no pointer and only checks that the process exists.
    unsafe { libc::kill(pid, 0) == 0 }
}

/// Whether process `pid` is still running: it exists and is not a zombie. An orphaned helper is
/// reparented to PID 1, which in a CI container may never reap it, so a killed helper can stay a
/// zombie indefinitely; only its parent could reap it, and that is not this test.
fn process_running(pid: libc::pid_t) -> bool {
    if !process_exists(pid) {
        // When: `kill(pid, 0)` fails, the process is gone entirely.
        return false;
    }
    !process_is_zombie(pid)
}

/// Whether `pid` is a zombie, read from `/proc/<pid>/stat` (the state follows the last `)`).
#[cfg(target_os = "linux")]
fn process_is_zombie(pid: libc::pid_t) -> bool {
    std::fs::read_to_string(format!("/proc/{pid}/stat"))
        .ok()
        .and_then(|stat| stat.rsplit_once(')').map(|(_, rest)| rest.trim_start().starts_with('Z')))
        .unwrap_or(false)
}

/// Whether `pid` is a zombie. Outside Linux launchd reaps orphans at once, so an existing process
/// is treated as running.
#[cfg(not(target_os = "linux"))]
fn process_is_zombie(_pid: libc::pid_t) -> bool {
    false
}

/// Contract: a hung child is killed at the deadline together with a helper it started, the
/// child is reaped, and the run reports a timeout.
#[test]
fn a_hung_child_and_its_helper_are_killed_and_reaped() {
    let root = tempfile::tempdir().unwrap();
    let pids = root.path().join("pids");
    let body = format!("sleep 30 &\necho \"$$ $!\" > '{pids}'\nsleep 30", pids = pids.display());
    let hung = write_script(root.path(), "hung", &body);
    let started = Instant::now();
    assert_eq!(run_bounded(&hung, &[], Duration::from_millis(500)), RunOutcome::TimedOut);
    assert!(started.elapsed() < Duration::from_secs(5), "the run was not bounded");
    let text = std::fs::read_to_string(&pids).unwrap();
    let ids: Vec<libc::pid_t> = text.split_whitespace().map(|id| id.parse().unwrap()).collect();
    let (child, helper) = (ids[0], ids[1]);
    // The direct child must be reaped (gone, not a zombie); the helper is not ours to reap, so it
    // only has to stop running.
    let deadline = Instant::now() + Duration::from_secs(5);
    while process_exists(child) || process_running(helper) {
        assert!(
            Instant::now() < deadline,
            "a process from the hung run survived: child {child} exists={}, helper {helper} \
             running={}",
            process_exists(child),
            process_running(helper)
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    let (sink, reported) = recording_sink();
    apply_mime_defaults(&hung, &shell_script_types()[..1], Duration::from_millis(200), &sink);
    assert_eq!(
        *reported.lock().unwrap(),
        vec![(0, HandlerOutcome::Failed("xdg-mime default: timed out".into()))]
    );
}

/// Contract: a program that cannot start, or one a signal ends, is described without a value.
#[test]
fn spawn_failures_and_signals_are_described() {
    let root = tempfile::tempdir().unwrap();
    let missing = run_bounded(&root.path().join("absent"), &[], COMMAND_TIMEOUT);
    assert_eq!(missing.describe(), "could not start (entity not found)");
    let killed = write_script(root.path(), "killed", "kill -9 $$");
    let outcome = run_bounded(&killed, &[], COMMAND_TIMEOUT);
    assert_eq!(outcome.describe(), "killed by a signal");
}

/// Contract: Apply without a successful plan runs nothing and fails every item.
#[test]
fn apply_without_a_plan_fails_every_item() {
    let mut backend = LinuxDefaultHandlers::new(XdgEnvironment::default(), COMMAND_TIMEOUT);
    let plan = HandlerPlan {
        mode: PlanMode::Change,
        items: vec![HandlerItem::new(HandlerKind::MimeType, "text/x-shellscript")],
        confirm_notes: Vec::new(),
        report_notes: Vec::new(),
    };
    let (sink, reported) = recording_sink();
    backend.apply(&plan, sink);
    assert_eq!(
        *reported.lock().unwrap(),
        vec![(0, HandlerOutcome::Failed("xdg-mime was not resolved".into()))]
    );
}

/// Contract: the process backend reads the live environment's PATH.
#[test]
fn the_process_backend_reads_the_live_environment() {
    let backend = LinuxDefaultHandlers::from_process_env();
    assert_eq!(backend.environment.path, std::env::var_os("PATH"));
    assert_eq!(backend.timeout, COMMAND_TIMEOUT);
}
