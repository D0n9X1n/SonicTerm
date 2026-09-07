use super::*;
use std::cell::{Cell, RefCell};
use std::ffi::OsString;
use std::path::Path;

fn absolute_root() -> PathBuf {
    #[cfg(windows)]
    {
        PathBuf::from(r"C:\work\project")
    }
    #[cfg(not(windows))]
    {
        PathBuf::from("/work/project")
    }
}

fn parsed(args: impl IntoIterator<Item = OsString>) -> ParsedCli {
    crate::cli::parse_cli_from(args).unwrap()
}

#[test]
fn relative_open_script_is_resolved_and_routed_to_the_sink() {
    let parsed = parsed([
        OsString::from("sonicterm"),
        OsString::from("--open-script"),
        OsString::from("scripts/build.ps1"),
    ]);
    let routed = RefCell::new(Vec::new());

    assert!(queue_startup_open_script_with(
        &parsed,
        || Some(absolute_root()),
        |request| routed.borrow_mut().push(request),
    )
    .unwrap());

    let routed = routed.into_inner();
    assert_eq!(routed.len(), 1);
    assert_eq!(routed[0].original_path, Path::new("scripts/build.ps1"));
    assert_eq!(routed[0].launch_path, absolute_root().join("scripts/build.ps1"));
}

#[test]
fn absolute_open_script_never_queries_the_process_cwd() {
    let absolute = absolute_root().join("build.ps1");
    let parsed = parsed([
        OsString::from("sonicterm"),
        OsString::from("--open-script"),
        absolute.clone().into_os_string(),
    ]);
    let called = Cell::new(false);
    let routed = RefCell::new(None);

    assert!(queue_startup_open_script_with(
        &parsed,
        || {
            called.set(true);
            None
        },
        |request| *routed.borrow_mut() = Some(request),
    )
    .unwrap());

    assert!(!called.get());
    assert_eq!(routed.into_inner().unwrap().launch_path, absolute);
}

#[test]
fn relative_open_script_without_an_initial_cwd_is_a_contextual_error() {
    let parsed = parsed([
        OsString::from("sonicterm"),
        OsString::from("--open-script"),
        OsString::from("build.ps1"),
    ]);
    let sink_called = Cell::new(false);

    let error =
        queue_startup_open_script_with(&parsed, || None, |_| sink_called.set(true)).unwrap_err();
    let message = format!("{error:#}");
    assert!(message.contains("build.ps1"));
    assert!(message.contains("current directory"));
    assert!(!sink_called.get());
}

#[test]
fn no_open_script_routes_nothing() {
    let parsed = parsed([OsString::from("sonicterm")]);
    let sink_called = Cell::new(false);

    assert!(!queue_startup_open_script_with(
        &parsed,
        || panic!("cwd lookup must stay lazy"),
        |_| sink_called.set(true),
    )
    .unwrap());
    assert!(!sink_called.get());
}

#[test]
fn main_queues_the_request_before_constructing_the_windows_shell() {
    let source = include_str!("main.rs");
    let interactive = source
        .find("startup::queue_startup_open_script(&parsed_cli")
        .expect("interactive startup producer call");
    let queue = source[interactive..]
        .find("queue_startup_open_script")
        .map(|offset| interactive + offset)
        .expect("startup producer call");
    let shell = source[interactive..]
        .find("WindowsShell::new")
        .map(|offset| interactive + offset)
        .expect("interactive Windows shell construction");
    assert!(queue < shell);
}
