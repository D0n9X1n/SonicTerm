use super::*;
use std::path::{Path, PathBuf};

fn request(name: &str) -> OpenScriptRequest {
    let root = if cfg!(windows) { Path::new(r"C:\work") } else { Path::new("/work") };
    OpenScriptRequest::resolve(PathBuf::from(name), root).unwrap()
}

/// Requests queued before the proxy exists are kept in order and drained exactly once.
#[test]
fn requests_arriving_before_the_proxy_are_retained_in_order_exactly_once() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let _ = drain();
    let first = request("first.sh");
    let second = request("second.sh");

    assert!(!push_scripts(vec![first.clone(), second.clone()]));
    assert_eq!(drain(), vec![LaunchRequest::Script(first), LaunchRequest::Script(second)]);
    assert!(drain().is_empty());
}

/// Terminal launches and script drafts share one FIFO, so a cold start keeps their order.
#[test]
fn terminal_and_script_requests_share_one_queue() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let _ = drain();
    let terminal = LaunchRequest::Terminal(sonicterm_types::TerminalLaunch::default());
    let script = LaunchRequest::Script(request("draft.sh"));
    assert!(!push_requests(vec![terminal.clone(), script.clone()]));
    assert_eq!(drain(), vec![terminal, script]);
}
