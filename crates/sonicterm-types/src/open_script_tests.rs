use super::*;
use std::cell::Cell;

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

#[test]
fn resolves_nested_and_bare_relative_paths_against_initial_cwd() {
    let cwd = absolute_root();

    let nested = OpenScriptRequest::resolve(PathBuf::from("scripts/build.cmd"), &cwd).unwrap();
    assert_eq!(nested.original_path, Path::new("scripts/build.cmd"));
    assert_eq!(nested.launch_path, cwd.join("scripts/build.cmd"));
    assert_eq!(nested.pane_cwd(), Some(cwd.join("scripts").as_path()));

    let bare = OpenScriptRequest::resolve(PathBuf::from("build.cmd"), &cwd).unwrap();
    assert_eq!(bare.launch_path, cwd.join("build.cmd"));
    assert_eq!(bare.pane_cwd(), Some(cwd.as_path()));
}

#[test]
fn resolution_is_lexical_and_does_not_require_the_target_to_exist() {
    let request = OpenScriptRequest::resolve(
        PathBuf::from("./missing/../script with ünicode.sh"),
        &absolute_root(),
    )
    .unwrap();
    assert_eq!(request.launch_path, absolute_root().join("./missing/../script with ünicode.sh"));
}

#[test]
fn absolute_paths_do_not_consult_the_cwd_lookup() {
    let called = Cell::new(false);
    let request = OpenScriptRequest::resolve_with_cwd_lookup(
        absolute_root().join("already-absolute.sh"),
        || {
            called.set(true);
            None
        },
    )
    .unwrap();

    assert!(!called.get());
    assert_eq!(request.launch_path, absolute_root().join("already-absolute.sh"));
}

#[test]
fn relative_paths_require_an_available_absolute_initial_cwd() {
    assert_eq!(
        OpenScriptRequest::resolve_with_cwd_lookup(PathBuf::from("run.sh"), || None),
        Err(OpenScriptResolveError::InitialCwdUnavailable)
    );
    assert_eq!(
        OpenScriptRequest::resolve(PathBuf::from("run.sh"), Path::new("relative/cwd")),
        Err(OpenScriptResolveError::InitialCwdNotAbsolute)
    );
}
