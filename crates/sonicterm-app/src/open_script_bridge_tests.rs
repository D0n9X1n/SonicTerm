use super::*;
use std::path::{Path, PathBuf};

fn request(name: &str) -> OpenScriptRequest {
    let root = if cfg!(windows) { Path::new(r"C:\work") } else { Path::new("/work") };
    OpenScriptRequest::resolve(PathBuf::from(name), root).unwrap()
}

#[test]
fn requests_arriving_before_the_proxy_are_retained_in_order_exactly_once() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let _ = drain();
    let first = request("first.sh");
    let second = request("second.sh");

    assert!(!push_requests(vec![first.clone(), second.clone()]));
    assert_eq!(drain(), vec![first, second]);
    assert!(drain().is_empty());
}
