use super::*;
use crate::test_log::capture_all_levels;

/// The requests a link yields with every client present and a fixed home.
fn requests(link: &str) -> Vec<LaunchRequest> {
    requests_for_link(link, |_| true, Some(PathBuf::from("/Users/someone")))
}

/// The one terminal launch a link yields.
fn terminal(link: &str) -> TerminalLaunch {
    match requests(link).as_slice() {
        [LaunchRequest::Terminal(launch)] => launch.clone(),
        other => panic!("{link:?} must yield one terminal launch, got {other:?}"),
    }
}

/// The one notice a link yields.
fn notice(link: &str) -> String {
    match requests(link).as_slice() {
        [LaunchRequest::Notice(message)] => message.clone(),
        other => panic!("{link:?} must yield one notice, got {other:?}"),
    }
}

/// An ssh link runs `/usr/bin/ssh -l user -p port -- host` in home, unheld, as a url launch.
#[test]
fn an_ssh_link_runs_ssh_with_separate_fields_in_home() {
    let launch = terminal("ssh://user@example.invalid:2222");
    let command = launch.command.expect("command");
    assert_eq!(command.program, "/usr/bin/ssh");
    assert_eq!(command.args, ["-l", "user", "-p", "2222", "--", "example.invalid"]);
    assert_eq!(launch.cwd, Some(PathBuf::from("/Users/someone")));
    assert_eq!(launch.cwd_source, Some(CwdSource::Home));
    assert!(!launch.hold, "an unclean exit still holds under the pane exit rule");
    assert_eq!(launch.origin, LaunchOrigin::Url { scheme: "ssh" });
    assert_eq!(launch.title, None, "a link never renames the window");
}

/// A man page link runs `/usr/bin/man -S section -- page`, the form man actually parses.
#[test]
fn a_man_page_link_runs_man_with_dash_s() {
    let command = terminal("x-man-page://1/ls").command.expect("command");
    assert_eq!(command.program, "/usr/bin/man");
    assert_eq!(command.args, ["-S", "1", "--", "ls"]);
    assert_eq!(terminal("x-man-page://ls").command.expect("command").args, ["--", "ls"]);
}

/// A rejected link opens no tab; its notice names the scheme and field, never the value.
#[test]
fn a_rejected_link_queues_only_a_notice() {
    let message = notice("ssh://-oProxyCommand=secret-value@host");
    assert!(message.contains("ssh") && message.contains("user"), "{message}");
    assert!(!message.contains("secret-value"), "{message}");
    assert!(notice("ssh://a%25b@host").contains("user"));
    assert!(notice("ssh://%2Dx").contains("host"));
    assert!(notice("telnet://host").contains("not handled"));
}

/// A missing client opens no tab and no fallback; the notice names the client.
#[test]
fn a_missing_client_queues_only_a_notice() {
    let requests = requests_for_link("ssh://example.invalid", |_| false, None);
    let [LaunchRequest::Notice(message)] = requests.as_slice() else {
        panic!("a missing client must yield one notice, got {requests:?}");
    };
    assert!(message.contains("/usr/bin/ssh"), "{message}");
}

/// Without a home directory the link keeps the platform default instead of the process cwd `/`.
#[test]
fn a_link_without_home_records_no_cwd() {
    let requests = requests_for_link("ssh://example.invalid", |_| true, None);
    let [LaunchRequest::Terminal(launch)] = requests.as_slice() else {
        panic!("expected a terminal launch");
    };
    assert_eq!((launch.cwd.clone(), launch.cwd_source), (None, None));
}

/// A `GURL` Apple Event's direct-object string is the link the handler reads.
#[test]
fn a_get_url_event_yields_its_link() {
    use objc2_core_services::{kAnyTransactionID, kAutoGenerateReturnID};
    use objc2_foundation::NSString;

    let event = NSAppleEventDescriptor::appleEventWithEventClass_eventID_targetDescriptor_returnID_transactionID(
        INTERNET_EVENT_CLASS,
        GET_URL_EVENT_ID,
        Some(&NSAppleEventDescriptor::currentProcessDescriptor()),
        kAutoGenerateReturnID.try_into().expect("auto-generated return ID must fit an i16"),
        kAnyTransactionID,
    );
    assert_eq!(link_from_event(&event), None, "an event without a direct object names no link");
    let link = NSAppleEventDescriptor::descriptorWithString(&NSString::from_str("ssh://h"));
    event.setParamDescriptor_forKeyword(&link, keyDirectObject);
    assert_eq!(link_from_event(&event).as_deref(), Some("ssh://h"));
    assert_eq!(INTERNET_EVENT_CLASS, 0x4755_524c, "'GURL'");
}

/// No log level records a link's text, user, host, port or page, accepted or rejected.
#[test]
fn link_logs_carry_no_link_values() {
    let log = capture_all_levels(|| {
        let _ = requests("ssh://-oProxyCommand=secret-cmd-AAA@host");
        let _ = requests("ssh://secret-user-BBB@secret-host-CCC:2345/extra-DDD");
        let _ = requests("x-man-page:///-secret-page-EEE");
        let _ = requests_for_link("ssh://secret-host-FFF", |_| false, None);
    });
    assert!(log.contains("terminal link rejected"), "{log}");
    assert!(log.contains("field=\"user\"") || log.contains("field=user"), "{log}");
    assert!(log.contains("client_missing"), "{log}");
    for secret in ["secret-cmd-AAA", "secret-user-BBB", "secret-host-CCC", "2345", "extra-DDD"] {
        assert!(!log.contains(secret), "{secret} leaked into the log: {log}");
    }
    for secret in ["secret-page-EEE", "secret-host-FFF", "ProxyCommand"] {
        assert!(!log.contains(secret), "{secret} leaked into the log: {log}");
    }
}
