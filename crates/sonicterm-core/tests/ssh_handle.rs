//! Integration tests for [`sonicterm_core::ssh`].
//!
//! Scope: parser + validator only — no network. The live `SshHandle`
//! connect path is gated behind `feature = "ssh"` and pulls a tokio
//! runtime + russh; exercising it would require a mock SSH server,
//! which is out of scope for v1.

use sonicterm_core::ssh::{parse_target, validate_host, validate_user, SshError, SshTarget};

#[test]
fn parses_user_at_host_default_port() {
    let t = parse_target("alice@example.com").unwrap();
    assert_eq!(t, SshTarget { user: "alice".into(), host: "example.com".into(), port: 22 });
}

#[test]
fn parses_user_at_host_explicit_port() {
    let t = parse_target("bob@dev.box:2222").unwrap();
    assert_eq!(t, SshTarget { user: "bob".into(), host: "dev.box".into(), port: 2222 });
}

#[test]
fn parses_ipv4_target() {
    let t = parse_target("root@10.0.0.1:22").unwrap();
    assert_eq!(t.host, "10.0.0.1");
    assert_eq!(t.port, 22);
}

#[test]
fn parses_bracketed_ipv6_target() {
    let t = parse_target("user@[::1]:2200").unwrap();
    assert_eq!(t.host, "::1");
    assert_eq!(t.port, 2200);
}

#[test]
fn display_omits_default_port() {
    let t = SshTarget { user: "alice".into(), host: "example.com".into(), port: 22 };
    assert_eq!(t.to_string(), "alice@example.com");
    let t2 = SshTarget { user: "alice".into(), host: "example.com".into(), port: 2222 };
    assert_eq!(t2.to_string(), "alice@example.com:2222");
}

#[test]
fn rejects_empty_string() {
    assert!(matches!(parse_target(""), Err(SshError::ParseTarget(_))));
}

#[test]
fn rejects_missing_at() {
    assert!(matches!(parse_target("just-a-host"), Err(SshError::ParseTarget(_))));
}

#[test]
fn rejects_empty_user() {
    assert!(matches!(parse_target("@example.com"), Err(SshError::ParseTarget(_))));
}

#[test]
fn rejects_empty_host() {
    assert!(matches!(parse_target("alice@"), Err(SshError::ParseTarget(_))));
}

#[test]
fn rejects_port_zero() {
    assert!(matches!(parse_target("alice@example.com:0"), Err(SshError::ParseTarget(_))));
}

#[test]
fn rejects_non_numeric_port() {
    assert!(matches!(parse_target("alice@example.com:abc"), Err(SshError::ParseTarget(_))));
}

#[test]
fn rejects_oversize_target() {
    let big = format!("a@{}", "x".repeat(300));
    assert!(matches!(parse_target(&big), Err(SshError::ParseTarget(_))));
}

#[test]
fn rejects_shell_metachars_in_host() {
    // Defense in depth: russh doesn't shell-exec, but if a future code
    // path ever logs / displays / dispatches the host string we don't
    // want metachars to sneak through.
    for evil in
        ["alice@example.com;rm", "alice@host`whoami`", "alice@h$x", "alice@h|nc", "alice@h&x"]
    {
        let r = parse_target(evil);
        assert!(matches!(r, Err(SshError::ParseTarget(_))), "should reject {evil:?}, got {r:?}");
    }
}

#[test]
fn rejects_shell_metachars_in_user() {
    for evil in ["al ice@host", "al;ice@host", "al|ice@host", "al`x`@host"] {
        let r = parse_target(evil);
        assert!(matches!(r, Err(SshError::ParseTarget(_))), "should reject {evil:?}");
    }
}

#[test]
fn rejects_control_chars() {
    assert!(parse_target("alice@host\nrm").is_err());
    assert!(parse_target("alice@host\0").is_err());
    assert!(parse_target("alice\r@host").is_err());
}

#[test]
fn validators_accept_normal_inputs() {
    validate_host("example.com").unwrap();
    validate_host("sub.dom.example.co.uk").unwrap();
    validate_host("10.0.0.1").unwrap();
    validate_host("::1").unwrap();
    validate_user("alice").unwrap();
    validate_user("alice.smith_42-test").unwrap();
}

#[test]
fn validators_reject_empty_and_overlong() {
    assert!(validate_host("").is_err());
    assert!(validate_host(&"a".repeat(300)).is_err());
    assert!(validate_user("").is_err());
    assert!(validate_user(&"a".repeat(100)).is_err());
}

#[test]
fn action_round_trips_through_serde_plain() {
    use sonicterm_core::keymap::Action;
    let a = Action::OpenSshPane("alice@host:2222".into());
    // serde_json is enough to confirm the variant participates in
    // serialize/deserialize cleanly (we don't bind it as a bare string
    // in the keymap — it takes an argument).
    let s = serde_json::to_string(&a).unwrap();
    let back: Action = serde_json::from_str(&s).unwrap();
    assert_eq!(a, back);
}

// Drop test for SshHandle is feature-gated; without `feature = "ssh"`
// the type doesn't exist. The thread-shutdown contract is unit-checked
// inside the module when the feature is enabled.
#[cfg(feature = "ssh")]
#[test]
fn handle_drop_signals_shutdown() {
    // Pure smoke: constructing a handle to a non-routable address fails
    // fast, but the parser path stays clean. We don't drive a real
    // connection here; a mock SSH server is tracked as follow-up.
    let target = parse_target("alice@127.0.0.1:1").unwrap();
    let res = sonicterm_core::ssh::SshHandle::connect(target, None, 80, 24);
    // Either Ok (background thread will fail) or Err (build-time spawn
    // failed) — both must drop cleanly without hanging the test.
    drop(res);
}

// ----------------------------------------------------------------------
// Bypass regression: `SshTarget` exposes public fields, so a direct
// caller can sidestep `parse_target`. `SshHandle::connect` MUST
// re-validate before touching the network.
// ----------------------------------------------------------------------

#[cfg(feature = "ssh")]
#[test]
fn connect_rejects_bypass_attempts_in_host() {
    use sonicterm_core::ssh::SshHandle;
    let bad_hosts = [
        "host with space",
        "host;rm",
        "host|nc",
        "host\nrm",
        "host\0null",
        "ho`x`",
        "ho$x",
        "ho&x",
        "ho<x",
        "ho>x",
        "ho\"x",
    ];
    for h in bad_hosts {
        let target = SshTarget { user: "alice".into(), host: h.into(), port: 22 };
        let r = SshHandle::connect(target, None, 80, 24);
        assert!(
            matches!(r, Err(SshError::ParseTarget(_))),
            "connect should reject malformed host {h:?}, got {:?}",
            r.as_ref().err()
        );
    }
}

#[cfg(feature = "ssh")]
#[test]
fn connect_rejects_dotdot_is_allowed_but_overlong_is_not() {
    // `..` happens to be inside the host charset (dot is allowed) —
    // it's not a path traversal risk because the host is never used
    // as a filesystem path. But a wildly oversized host MUST be
    // rejected before reaching russh.
    use sonicterm_core::ssh::SshHandle;
    let target = SshTarget { user: "alice".into(), host: "a".repeat(300), port: 22 };
    let r = SshHandle::connect(target, None, 80, 24);
    assert!(matches!(r, Err(SshError::ParseTarget(_))), "overlong host must be rejected");
}

#[cfg(feature = "ssh")]
#[test]
fn connect_rejects_bypass_attempts_in_user() {
    use sonicterm_core::ssh::SshHandle;
    let bad_users = ["al ice", "al;ice", "al|x", "al`x`", "al\nice", "al\0x", "al$x"];
    for u in bad_users {
        let target = SshTarget { user: u.into(), host: "example.com".into(), port: 22 };
        let r = SshHandle::connect(target, None, 80, 24);
        assert!(
            matches!(r, Err(SshError::ParseTarget(_))),
            "connect should reject malformed user {u:?}, got {:?}",
            r.as_ref().err()
        );
    }
}

#[cfg(feature = "ssh")]
#[test]
fn connect_rejects_port_zero_bypass() {
    use sonicterm_core::ssh::SshHandle;
    let target = SshTarget { user: "alice".into(), host: "example.com".into(), port: 0 };
    let r = SshHandle::connect(target, None, 80, 24);
    assert!(matches!(r, Err(SshError::ParseTarget(_))), "port 0 must be rejected");
}

#[cfg(feature = "ssh")]
#[test]
fn connect_accepts_valid_hosts() {
    // Smoke: each of these passes validation. The background tokio
    // thread will fail to actually connect (unroutable / no shell), but
    // `connect` itself must return Ok with the validators happy.
    use sonicterm_core::ssh::SshHandle;
    for h in ["example.com", "192.168.1.1", "::1", "host.sub.example.com"] {
        let target = SshTarget { user: "alice".into(), host: h.into(), port: 22 };
        let r = SshHandle::connect(target, None, 80, 24);
        assert!(r.is_ok(), "valid host {h:?} should pass validation, got {:?}", r.as_ref().err());
        drop(r);
    }
}
