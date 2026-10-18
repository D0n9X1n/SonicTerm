use super::*;
use std::path::Path;

fn root() -> PathBuf {
    if cfg!(windows) {
        PathBuf::from(r"C:\work")
    } else {
        PathBuf::from("/work")
    }
}

#[test]
fn pure_path_resolution_preserves_order_and_rejects_unresolvable_relative_paths() {
    let cwd = root();
    let absolute = cwd.join("first.command");
    let resolved = resolve_paths(
        [absolute.clone(), PathBuf::from("second.sh"), PathBuf::from("missing.sh")],
        Some(&cwd),
    );
    assert_eq!(
        resolved.iter().map(|request| request.launch_path.clone()).collect::<Vec<_>>(),
        [absolute, cwd.join("second.sh"), cwd.join("missing.sh")]
    );

    assert!(resolve_paths([PathBuf::from("relative.sh")], None).is_empty());
}

#[test]
fn apple_event_file_urls_decode_to_ordered_open_requests() {
    let first = root().join("first.command");
    let second = root().join("second.tool");
    let requests = requests_from_paths_for_test(&[first.clone(), second.clone()]);

    assert_eq!(
        requests.iter().map(|request| request.launch_path.clone()).collect::<Vec<_>>(),
        [first, second]
    );
}

#[test]
fn absolute_paths_do_not_need_an_initial_cwd() {
    let absolute = root().join("script.sh");
    assert_eq!(resolve_paths([absolute.clone()], None)[0].launch_path, absolute);
    assert!(
        Path::new(&resolve_paths([root().join("script.sh")], None)[0].launch_path).is_absolute()
    );
}

/// A verified executable becomes a held command launch in its parent with its identity attached.
#[test]
fn a_verified_executable_becomes_a_held_command_launch() {
    let identity = sonicterm_types::FileIdentity { device: 7, inode: 9 };
    let requests = launch_requests_for(
        Disposition::Run {
            program: PathBuf::from("/work/bin/tool"),
            cwd: PathBuf::from("/work/bin"),
            identity,
        },
        None,
    );
    let [LaunchRequest::Terminal(launch)] = requests.as_slice() else {
        panic!("expected one terminal launch, got {requests:?}");
    };
    let command = launch.command.as_ref().expect("command");
    assert_eq!(command.program, "/work/bin/tool");
    assert!(command.args.is_empty());
    assert_eq!(launch.cwd.as_deref(), Some(Path::new("/work/bin")));
    assert_eq!(launch.cwd_source, Some(CwdSource::Parent));
    assert!(launch.hold, "output stays visible after the program exits");
    assert_eq!(launch.origin, LaunchOrigin::Executable { identity });
    assert_eq!(launch.title, None, "a document open never renames the window");
}

/// A refusal opens a plain shell beside the file (or in home) and queues its notice; nothing runs.
#[test]
fn a_refusal_opens_a_plain_shell_and_a_notice() {
    use crate::executable_classify::Refusal;
    let requests = launch_requests_for(
        Disposition::Refuse {
            refusal: Refusal::Quarantined,
            cwd: Some(PathBuf::from("/work/downloads")),
            file_name: "tool".to_string(),
        },
        Some(PathBuf::from("/Users/someone")),
    );
    let [LaunchRequest::Terminal(shell), LaunchRequest::Notice(message)] = requests.as_slice()
    else {
        panic!("expected a shell and a notice, got {requests:?}");
    };
    assert!(shell.command.is_none(), "a refusal never runs anything");
    assert_eq!(shell.cwd.as_deref(), Some(Path::new("/work/downloads")));
    assert_eq!(message, "Not run: tool is quarantined (downloaded). Run it yourself after review.");

    let unresolved = launch_requests_for(
        Disposition::Refuse { refusal: Refusal::Unresolvable, cwd: None, file_name: "x".into() },
        Some(PathBuf::from("/Users/someone")),
    );
    let LaunchRequest::Terminal(shell) = &unresolved[0] else { panic!("expected a shell") };
    assert_eq!(shell.cwd.as_deref(), Some(Path::new("/Users/someone")), "fails closed in home");
    assert_eq!(shell.cwd_source, Some(CwdSource::Home));
}

/// A script disposition is today's draft request, unchanged.
#[test]
fn a_script_disposition_stays_a_draft() {
    let request = resolve_paths([root().join("build.sh")], None).remove(0);
    assert_eq!(
        launch_requests_for(Disposition::Draft(request.clone()), None),
        vec![LaunchRequest::Script(request)]
    );
}

/// A refusal logs only its reason; the file's name and directory never reach any log level.
#[test]
fn refusal_logs_carry_the_reason_only() {
    use crate::executable_classify::Refusal;
    let log = crate::test_log::capture_all_levels(|| {
        let _ = launch_requests_for(
            Disposition::Refuse {
                refusal: Refusal::Quarantined,
                cwd: Some(PathBuf::from("/work/secret-dir-AAA")),
                file_name: "secret-name-BBB".to_string(),
            },
            None,
        );
    });
    assert!(log.contains("quarantined"), "{log}");
    for secret in ["secret-dir-AAA", "secret-name-BBB"] {
        assert!(!log.contains(secret), "{secret} leaked into the log: {log}");
    }
}
