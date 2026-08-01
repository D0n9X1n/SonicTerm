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
