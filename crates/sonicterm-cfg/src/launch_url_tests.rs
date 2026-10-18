use super::*;

/// Parse `link`, expecting a rejection, and return its scheme name and field name.
fn rejected(link: &str) -> (&'static str, &'static str) {
    match parse_terminal_link(link) {
        Ok(parsed) => panic!("{link:?} was accepted as {parsed:?}"),
        Err(rejection) => (rejection.scheme_name(), rejection.field.as_str()),
    }
}

/// Parse `link`, expecting success, and return the client program and its argv.
fn accepted(link: &str) -> (&'static str, Vec<String>) {
    let parsed = parse_terminal_link(link).unwrap_or_else(|rejection| {
        panic!("{link:?} was rejected: {rejection:?}");
    });
    (parsed.scheme().client_path(), parsed.args())
}

/// Every accepted ssh form becomes `/usr/bin/ssh [-l user] [-p port] -- host`, one field per element.
#[test]
fn ssh_links_build_exact_argv() {
    let cases: &[(&str, &[&str])] = &[
        ("ssh://example.invalid", &["--", "example.invalid"]),
        ("ssh://example.invalid/", &["--", "example.invalid"]),
        ("ssh://user@example.invalid:2222", &["-l", "user", "-p", "2222", "--", "example.invalid"]),
        (
            "SSH://u.s_e-r@host-1.example:22/",
            &["-l", "u.s_e-r", "-p", "22", "--", "host-1.example"],
        ),
        ("ssh://192.0.2.7", &["--", "192.0.2.7"]),
        ("ssh://user@192.0.2.7:65535", &["-l", "user", "-p", "65535", "--", "192.0.2.7"]),
        ("ssh://ex%61mple.invalid", &["--", "example.invalid"]),
    ];
    for (link, argv) in cases {
        assert_eq!(
            accepted(link),
            ("/usr/bin/ssh", argv.iter().map(ToString::to_string).collect())
        );
    }
}

/// A bracketed IPv6 host is passed to ssh without brackets, with or without a port.
#[test]
fn ssh_accepts_bracketed_ipv6_with_and_without_port() {
    assert_eq!(accepted("ssh://[2001:db8::1]").1, ["--", "2001:db8::1"]);
    assert_eq!(
        accepted("ssh://me@[2001:db8::1]:2200/").1,
        ["-l", "me", "-p", "2200", "--", "2001:db8::1"]
    );
    assert_eq!(accepted("ssh://[::1]").1, ["--", "::1"]);
}

/// Option injection through the host, the user or an escape never reaches ssh.
#[test]
fn ssh_rejects_option_injection() {
    assert_eq!(rejected("ssh://-oProxyCommand=touch%20pwned"), ("ssh", "host"));
    assert_eq!(rejected("ssh://-oProxyCommand=x@host"), ("ssh", "user"));
    assert_eq!(rejected("ssh://%2DoProxyCommand=x@host"), ("ssh", "user"));
    assert_eq!(rejected("ssh://%2Dx"), ("ssh", "host"));
    assert_eq!(rejected("ssh://-x"), ("ssh", "host"));
    assert_eq!(rejected("ssh://user@-host"), ("ssh", "host"));
}

/// A user that is otherwise valid but starts with `-`, raw or escaped, is refused as an option.
#[test]
fn ssh_rejects_a_leading_dash_user() {
    assert_eq!(rejected("ssh://-x@host"), ("ssh", "user"));
    assert_eq!(rejected("ssh://%2Dx@host"), ("ssh", "user"));
    assert_eq!(rejected("ssh://-l@host"), ("ssh", "user"));
}

/// `%25` is refused as double encoding before any grammar check, in every field.
#[test]
fn double_encoding_is_refused_by_its_own_rule() {
    for link in
        ["ssh://a%25b@host", "ssh://host%2541", "x-man-page://1/l%25s", "x-man-page://1%2541/ls"]
    {
        let rejection = parse_terminal_link(link).unwrap_err();
        assert_eq!(rejection.reason, "double encoding (%25) is not accepted", "{link}");
    }
}

/// `%u`, `%h` and other ssh tokens or shell metacharacters cannot appear in user or host.
#[test]
fn ssh_rejects_tokens_and_metacharacters() {
    assert_eq!(rejected("ssh://a%25u@host"), ("ssh", "user"));
    assert_eq!(rejected("ssh://host%25h"), ("ssh", "host"));
    assert_eq!(rejected("ssh://a%25b@host"), ("ssh", "user"));
    assert_eq!(rejected("ssh://a$(id)@host"), ("ssh", "user"));
    assert_eq!(rejected("ssh://a;b@host"), ("ssh", "user"));
    assert_eq!(rejected("ssh://host%60id%60"), ("ssh", "host"));
    assert_eq!(rejected("ssh://a%20b@host"), ("ssh", "user"));
}

/// Escapes that decode to a delimiter, NUL or newline are refused in every ssh field.
#[test]
fn ssh_rejects_encoded_delimiters_and_controls() {
    assert_eq!(rejected("ssh://a%40b@host"), ("ssh", "user"));
    assert_eq!(rejected("ssh://a%2Fb@host"), ("ssh", "user"));
    assert_eq!(rejected("ssh://a%3Ab@host"), ("ssh", "user"));
    assert_eq!(rejected("ssh://a%00@host"), ("ssh", "user"));
    assert_eq!(rejected("ssh://host%0A"), ("ssh", "host"));
    assert_eq!(rejected("ssh://host%2Fx"), ("ssh", "host"));
    assert_eq!(rejected("ssh://host%3A22"), ("ssh", "host"));
    assert_eq!(rejected("ssh://host%C2%85"), ("ssh", "host"));
    assert_eq!(rejected("ssh://host%FF"), ("ssh", "host"));
    assert_eq!(rejected("ssh://host%2"), ("ssh", "host"));
}

/// Passwords, two `@`, an empty user, query, fragment and a path are refused.
#[test]
fn ssh_rejects_structure_outside_the_grammar() {
    assert_eq!(rejected("ssh://user:pass@host"), ("ssh", "password"));
    assert_eq!(rejected("ssh://user:@host"), ("ssh", "password"));
    assert_eq!(rejected("ssh://a@b@host"), ("ssh", "user"));
    assert_eq!(rejected("ssh://@host"), ("ssh", "user"));
    assert_eq!(rejected("ssh://host?x=1"), ("ssh", "query"));
    assert_eq!(rejected("ssh://host#frag"), ("ssh", "fragment"));
    assert_eq!(rejected("ssh://host/path"), ("ssh", "path"));
    assert_eq!(rejected("ssh://host//"), ("ssh", "path"));
    assert_eq!(rejected("ssh:host"), ("ssh", "authority"));
    assert_eq!(rejected("ssh://"), ("ssh", "host"));
    assert_eq!(rejected("ssh://user@"), ("ssh", "host"));
    assert_eq!(rejected("ssh://host name"), ("ssh", "link"));
}

/// Malformed IPv6, zone identifiers and trailing junk after the brackets are refused.
#[test]
fn ssh_rejects_bad_ipv6() {
    assert_eq!(rejected("ssh://[2001:db8::zz]"), ("ssh", "host"));
    assert_eq!(rejected("ssh://[2001:db8::1"), ("ssh", "host"));
    assert_eq!(rejected("ssh://[fe80::1%25en0]"), ("ssh", "host"));
    assert_eq!(rejected("ssh://[fe80::1%en0]"), ("ssh", "host"));
    assert_eq!(rejected("ssh://[::1]x"), ("ssh", "host"));
    assert_eq!(rejected("ssh://2001:db8::1"), ("ssh", "port"));
    assert_eq!(rejected("ssh://[]"), ("ssh", "host"));
}

/// Internationalized host names are refused, raw or percent-encoded.
#[test]
fn ssh_rejects_idn() {
    assert_eq!(rejected("ssh://bücher.example"), ("ssh", "link"));
    assert_eq!(rejected("ssh://b%C3%BCcher.example"), ("ssh", "host"));
}

/// Ports outside 1-65535, zero-padded, escaped or empty ports are refused.
#[test]
fn ssh_rejects_bad_ports() {
    for link in [
        "ssh://host:0",
        "ssh://host:65536",
        "ssh://host:022",
        "ssh://host:",
        "ssh://host:%32%32",
        "ssh://host:22a",
        "ssh://host:999999",
        "ssh://host:1:2",
    ] {
        assert_eq!(rejected(link), ("ssh", "port"), "{link}");
    }
}

/// DNS names follow the label grammar: no empty, edge-dash or overlong labels.
#[test]
fn ssh_rejects_bad_dns_names() {
    for link in ["ssh://host-", "ssh://a..b", "ssh://.host", "ssh://host.", "ssh://a_b"] {
        assert_eq!(rejected(link), ("ssh", "host"), "{link}");
    }
    let long_label = "a".repeat(64);
    assert_eq!(rejected(&format!("ssh://{long_label}")), ("ssh", "host"));
    let long_user = "u".repeat(33);
    assert_eq!(rejected(&format!("ssh://{long_user}@host")), ("ssh", "user"));
    assert!(parse_terminal_link(&format!("ssh://{}@host", "u".repeat(32))).is_ok());
}

/// Every accepted x-man-page form becomes `/usr/bin/man [-S section] -- page`.
#[test]
fn man_page_links_build_exact_argv() {
    let cases: &[(&str, &[&str])] = &[
        ("x-man-page:///ls", &["--", "ls"]),
        ("x-man-page://ls", &["--", "ls"]),
        ("x-man-page://ls/", &["--", "ls"]),
        ("x-man-page://1/ls", &["-S", "1", "--", "ls"]),
        ("x-man-page://3pm/Foo::Bar", &["-S", "3pm", "--", "Foo::Bar"]),
        ("x-man-page://n/after", &["-S", "n", "--", "after"]),
        ("X-MAN-PAGE://8/git-upload-pack", &["-S", "8", "--", "git-upload-pack"]),
        ("x-man-page:///g++", &["--", "g++"]),
    ];
    for (link, argv) in cases {
        assert_eq!(
            accepted(link),
            ("/usr/bin/man", argv.iter().map(ToString::to_string).collect())
        );
    }
}

/// Leading `-`/`.`, extra segments, parameters, queries and bad sections never reach man.
#[test]
fn man_page_rejects_outside_the_grammar() {
    assert_eq!(rejected("x-man-page:///-P%20sh"), ("x-man-page", "page"));
    assert_eq!(rejected("x-man-page:///-Psh"), ("x-man-page", "page"));
    assert_eq!(rejected("x-man-page:///%2DPsh"), ("x-man-page", "page"));
    assert_eq!(rejected("x-man-page:///.hidden"), ("x-man-page", "page"));
    assert_eq!(rejected("x-man-page://1/ls/extra"), ("x-man-page", "path"));
    assert_eq!(rejected("x-man-page:///a/b"), ("x-man-page", "path"));
    assert_eq!(rejected("x-man-page://1/ls;type=a"), ("x-man-page", "path"));
    assert_eq!(rejected("x-man-page:///ls?x"), ("x-man-page", "query"));
    assert_eq!(rejected("x-man-page:///ls#x"), ("x-man-page", "fragment"));
    assert_eq!(rejected("x-man-page://0/ls"), ("x-man-page", "section"));
    assert_eq!(rejected("x-man-page://-S/ls"), ("x-man-page", "section"));
    assert_eq!(rejected("x-man-page://1X/ls"), ("x-man-page", "section"));
    assert_eq!(rejected("x-man-page://1abcdefghi/ls"), ("x-man-page", "section"));
    assert_eq!(rejected("x-man-page://1/ls%2Fx"), ("x-man-page", "page"));
    assert_eq!(rejected("x-man-page://1/l%25s"), ("x-man-page", "page"));
    assert_eq!(rejected("x-man-page://1/ls%00"), ("x-man-page", "page"));
    assert_eq!(rejected("x-man-page:///"), ("x-man-page", "page"));
    assert_eq!(rejected("x-man-page://"), ("x-man-page", "page"));
    assert_eq!(rejected("x-man-page:ls"), ("x-man-page", "authority"));
    assert_eq!(rejected("x-man-page:///a%3Ab"), ("x-man-page", "page"));
}

/// Schemes other than ssh and x-man-page, including telnet, are not handled.
#[test]
fn other_schemes_are_unsupported() {
    for link in ["telnet://host", "http://example.com", "file:///bin/ls", "ssh", ""] {
        assert_eq!(rejected(link), ("unsupported", "link"), "{link}");
    }
}

/// A rejection's notice and log values name the scheme and field but never echo the link.
#[test]
fn rejection_notice_never_contains_link_text() {
    let secret = "ssh://-oProxyCommand=secret-value@host";
    let rejection = parse_terminal_link(secret).unwrap_err();
    let notice = rejection.notice();
    assert!(notice.contains("ssh") && notice.contains("user"), "{notice}");
    assert!(!notice.contains("secret-value") && !notice.contains("ProxyCommand"), "{notice}");
    let unsupported = parse_terminal_link("telnet://secret-host").unwrap_err().notice();
    assert!(!unsupported.contains("secret-host"), "{unsupported}");
}

/// Links longer than the bound are refused before parsing.
#[test]
fn overlong_links_are_rejected() {
    let link = format!("ssh://{}", "a.".repeat(MAX_LINK_BYTES));
    assert_eq!(rejected(&link), ("ssh", "link"));
}
