//! Parse and validate the terminal links SonicTerm handles: `ssh:` and `x-man-page:`.
//!
//! A link becomes a fixed system client with each validated field as its own
//! argument, never a shell command. Parsing runs in three stages: split the URI
//! structure on its raw delimiters, percent-decode each accepted component
//! exactly once, then match the decoded text against a strict per-field grammar.
//! The grammar admits no `%`, whitespace or shell metacharacter in any field, and
//! no field may start with `-`, so no value can become an option of the client.
//!
//! Rejections name the scheme, the failing field and a fixed reason. They never
//! carry the link's text, so callers can log and display them safely.

use std::net::{Ipv4Addr, Ipv6Addr};

/// Longest link accepted, in bytes; longer input is rejected before parsing.
const MAX_LINK_BYTES: usize = 2048;
/// Longest DNS host name accepted, in bytes.
const MAX_DNS_NAME_BYTES: usize = 253;
/// Longest DNS label accepted, in bytes.
const MAX_DNS_LABEL_BYTES: usize = 63;
/// Longest man page name accepted, in bytes.
const MAX_PAGE_BYTES: usize = 128;
/// The reason given for an escape that decodes to a delimiter or control character.
const ENCODED_DELIMITER: &str = "an encoded /, @, :, NUL or control character is not accepted";

/// A link scheme SonicTerm handles.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LinkScheme {
    /// `ssh://[user@]host[:port][/]`, run with `/usr/bin/ssh`.
    Ssh,
    /// `x-man-page:///page` or `x-man-page://section/page`, run with `/usr/bin/man`.
    ManPage,
}

impl LinkScheme {
    /// The scheme as it appears in a link and in logs.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ssh => "ssh",
            Self::ManPage => "x-man-page",
        }
    }

    /// The absolute path of the system client that serves this scheme.
    ///
    /// Clients are resolved by absolute path because a launchd-started app has a
    /// minimal `PATH`, and a user-writable directory must never supply them.
    #[must_use]
    pub fn client_path(self) -> &'static str {
        match self {
            Self::Ssh => "/usr/bin/ssh",
            Self::ManPage => "/usr/bin/man",
        }
    }

    /// The client's file name, for tab titles, logs and messages.
    #[must_use]
    pub fn client_name(self) -> &'static str {
        match self {
            Self::Ssh => "ssh",
            Self::ManPage => "man",
        }
    }
}

/// The part of a link a rejection refers to.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LinkField {
    /// The scheme itself, or the link as a whole.
    Link,
    /// The authority (`//...`) part of the link.
    Authority,
    /// The `ssh` user name.
    User,
    /// A password in the `ssh` user information.
    Password,
    /// The `ssh` host.
    Host,
    /// The `ssh` port.
    Port,
    /// The path part of the link.
    Path,
    /// A query (`?...`).
    Query,
    /// A fragment (`#...`).
    Fragment,
    /// The man page section.
    Section,
    /// The man page name.
    Page,
}

impl LinkField {
    /// The stable field name used in logs and messages.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Link => "link",
            Self::Authority => "authority",
            Self::User => "user",
            Self::Password => "password",
            Self::Host => "host",
            Self::Port => "port",
            Self::Path => "path",
            Self::Query => "query",
            Self::Fragment => "fragment",
            Self::Section => "section",
            Self::Page => "page",
        }
    }
}

/// Why a link was not opened: its scheme, the failing field and a fixed reason.
///
/// No value from the link is stored, so a rejection is safe to log and show.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LinkRejection {
    /// The link's scheme, or `None` when the scheme is not one SonicTerm handles.
    pub scheme: Option<LinkScheme>,
    /// The part of the link that failed.
    pub field: LinkField,
    /// A fixed explanation of the failure.
    pub reason: &'static str,
}

impl LinkRejection {
    fn new(scheme: LinkScheme, field: LinkField, reason: &'static str) -> Self {
        Self { scheme: Some(scheme), field, reason }
    }

    /// The scheme name for logs: the handled scheme, or `unsupported`.
    #[must_use]
    pub fn scheme_name(&self) -> &'static str {
        self.scheme.map_or("unsupported", LinkScheme::as_str)
    }

    /// The user-facing notice for this rejection; it names the scheme and field, never a value.
    #[must_use]
    pub fn notice(&self) -> String {
        match self.scheme {
            Some(scheme) => format!(
                "Link not opened: the {} link's {} was rejected ({}).",
                scheme.as_str(),
                self.field.as_str(),
                self.reason
            ),
            None => format!("Link not opened: {}.", self.reason),
        }
    }
}

/// A validated terminal link, ready to run as its fixed client.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TerminalLink {
    /// Connect with `ssh`.
    Ssh {
        /// The login name, when the link names one.
        user: Option<String>,
        /// A DNS name, IPv4 address, or IPv6 address without brackets.
        host: String,
        /// The port, when the link names one.
        port: Option<u16>,
    },
    /// Show a manual page with `man`.
    ManPage {
        /// The manual section, when the link names one.
        section: Option<String>,
        /// The page name.
        page: String,
    },
}

impl TerminalLink {
    /// The link's scheme.
    #[must_use]
    pub fn scheme(&self) -> LinkScheme {
        match self {
            Self::Ssh { .. } => LinkScheme::Ssh,
            Self::ManPage { .. } => LinkScheme::ManPage,
        }
    }

    /// The client's arguments, excluding the program; every field is its own element.
    ///
    /// `--` always precedes the host or page, so neither can be read as an option.
    #[must_use]
    pub fn args(&self) -> Vec<String> {
        let mut args = Vec::new();
        match self {
            Self::Ssh { user, host, port } => {
                if let Some(user) = user {
                    args.extend(["-l".to_string(), user.clone()]);
                }
                if let Some(port) = port {
                    args.extend(["-p".to_string(), port.to_string()]);
                }
                args.extend(["--".to_string(), host.clone()]);
            }
            Self::ManPage { section, page } => {
                // `man SECTION -- PAGE` reads `--` as the page, so the section goes through `-S`.
                if let Some(section) = section {
                    args.extend(["-S".to_string(), section.clone()]);
                }
                args.extend(["--".to_string(), page.clone()]);
            }
        }
        args
    }
}

/// Parse, decode and validate a terminal link.
///
/// # Errors
/// Returns a [`LinkRejection`] naming the scheme, the failing field and the
/// reason when the link is not an `ssh:` or `x-man-page:` link that matches the
/// accepted grammar exactly.
pub fn parse_terminal_link(link: &str) -> Result<TerminalLink, LinkRejection> {
    let unsupported =
        LinkRejection { scheme: None, field: LinkField::Link, reason: "the scheme is not handled" };
    let Some((scheme_text, rest)) = link.split_once(':') else {
        // When: `link` has no `:`, it has no scheme and cannot be one of ours.
        return Err(unsupported);
    };
    let scheme = match scheme_text.to_ascii_lowercase().as_str() {
        "ssh" => LinkScheme::Ssh,
        "x-man-page" => LinkScheme::ManPage,
        _ => {
            // When: `scheme_text` names any other scheme, including telnet, nothing here may run it.
            return Err(unsupported);
        }
    };
    if link.len() > MAX_LINK_BYTES {
        // When: `link` exceeds the bound, reject before any component is examined.
        return Err(LinkRejection::new(scheme, LinkField::Link, "the link is too long"));
    }
    if !rest.bytes().all(|byte| byte.is_ascii_graphic()) {
        // When: `rest` holds a raw space, control or non-ASCII byte, which no valid link contains.
        let reason = if rest.bytes().any(|byte| !byte.is_ascii()) {
            "non-ASCII text, including internationalized names, is not accepted"
        } else {
            "it contains a space or control character"
        };
        return Err(LinkRejection::new(scheme, LinkField::Link, reason));
    }
    reject_query_and_fragment(scheme, rest)?;
    let Some(hierarchy) = rest.strip_prefix("//") else {
        // When: `rest` lacks `//`, the link has no authority and is not a form we accept.
        return Err(LinkRejection::new(
            scheme,
            LinkField::Authority,
            "the link must start with //",
        ));
    };
    let (authority, path) = match hierarchy.find('/') {
        Some(index) => (&hierarchy[..index], &hierarchy[index..]),
        None => (hierarchy, ""),
    };
    match scheme {
        LinkScheme::Ssh => parse_ssh(authority, path),
        LinkScheme::ManPage => parse_man_page(authority, path),
    }
}

/// Reject any `?` query or `#` fragment, naming whichever comes first.
fn reject_query_and_fragment(scheme: LinkScheme, rest: &str) -> Result<(), LinkRejection> {
    match rest.find(['?', '#']) {
        Some(index) if rest.as_bytes()[index] == b'?' => {
            Err(LinkRejection::new(scheme, LinkField::Query, "queries are not accepted"))
        }
        Some(_) => {
            Err(LinkRejection::new(scheme, LinkField::Fragment, "fragments are not accepted"))
        }
        None => Ok(()),
    }
}

/// Parse `[user@]host[:port]` with an empty or `/` path.
fn parse_ssh(authority: &str, path: &str) -> Result<TerminalLink, LinkRejection> {
    let scheme = LinkScheme::Ssh;
    if !path.is_empty() && path != "/" {
        // When: `path` is anything but empty or `/`, ssh has no use for it and it may hide a payload.
        return Err(LinkRejection::new(
            scheme,
            LinkField::Path,
            "only an empty path or / is accepted",
        ));
    }
    let (userinfo, host_port) = match authority.split_once('@') {
        Some((userinfo, host_port)) => (Some(userinfo), host_port),
        None => (None, authority),
    };
    if host_port.contains('@') {
        // When: `host_port` still holds `@`, a second `@` makes the user/host split ambiguous.
        return Err(LinkRejection::new(scheme, LinkField::User, "more than one @ is not accepted"));
    }
    let user = match userinfo {
        Some(userinfo) => Some(parse_ssh_user(userinfo)?),
        None => None,
    };
    let (host, port) = parse_ssh_host_port(host_port)?;
    Ok(TerminalLink::Ssh { user, host, port })
}

/// Decode and validate the raw user information of an `ssh` link.
fn parse_ssh_user(userinfo: &str) -> Result<String, LinkRejection> {
    let scheme = LinkScheme::Ssh;
    if userinfo.contains(':') {
        // When: `userinfo` holds a raw `:`, it carries a password, which is never accepted.
        return Err(LinkRejection::new(scheme, LinkField::Password, "passwords are not accepted"));
    }
    if userinfo.is_empty() {
        // When: `userinfo` is empty (`ssh://@host`), there is no name to pass.
        return Err(LinkRejection::new(scheme, LinkField::User, "the user name is empty"));
    }
    let user = decode_component(scheme, LinkField::User, userinfo)?;
    if !is_valid_user(&user) {
        // When: the decoded `user` breaks the grammar, for example by starting with `-`.
        return Err(LinkRejection::new(
            scheme,
            LinkField::User,
            "a user name is 1-32 letters, digits, '.', '_' or '-', not starting with '-'",
        ));
    }
    Ok(user)
}

/// Split and validate `host[:port]`, where an IPv6 host is bracketed.
fn parse_ssh_host_port(host_port: &str) -> Result<(String, Option<u16>), LinkRejection> {
    let scheme = LinkScheme::Ssh;
    let (host, port_text) = if let Some(bracketed) = host_port.strip_prefix('[') {
        // When: `host_port` starts with `[`, it must be a bracketed IPv6 literal.
        let Some((literal, after)) = bracketed.split_once(']') else {
            // When: `bracketed` has no `]`, the IPv6 literal never closes.
            return Err(LinkRejection::new(
                scheme,
                LinkField::Host,
                "the IPv6 address is not closed",
            ));
        };
        let port_text = match after {
            "" => None,
            _ => Some(after.strip_prefix(':').ok_or_else(|| {
                LinkRejection::new(
                    scheme,
                    LinkField::Host,
                    "unexpected text after the IPv6 address",
                )
            })?),
        };
        (parse_ipv6_literal(literal)?, port_text)
    } else {
        // When: `host_port` has no `[`, it is a DNS name or IPv4 address with an optional port.
        let (host_text, port_text) = match host_port.split_once(':') {
            Some((host_text, port_text)) => (host_text, Some(port_text)),
            None => (host_port, None),
        };
        (parse_ssh_host_name(host_text)?, port_text)
    };
    let port = port_text.map(parse_port).transpose()?;
    Ok((host, port))
}

/// Validate a bracketed IPv6 literal and return it without brackets.
fn parse_ipv6_literal(literal: &str) -> Result<String, LinkRejection> {
    let scheme = LinkScheme::Ssh;
    if literal.contains('%') {
        // When: `literal` holds `%`, it names a zone identifier or an escape, neither accepted.
        return Err(LinkRejection::new(
            scheme,
            LinkField::Host,
            "IPv6 zone identifiers and escapes are not accepted",
        ));
    }
    literal
        .parse::<Ipv6Addr>()
        .map(|address| address.to_string())
        .map_err(|_| LinkRejection::new(scheme, LinkField::Host, "the IPv6 address is not valid"))
}

/// Decode and validate an unbracketed host: an ASCII DNS name or an IPv4 address.
fn parse_ssh_host_name(host_text: &str) -> Result<String, LinkRejection> {
    let scheme = LinkScheme::Ssh;
    if host_text.is_empty() {
        // When: `host_text` is empty, the link names no host to connect to.
        return Err(LinkRejection::new(scheme, LinkField::Host, "the host is empty"));
    }
    let host = decode_component(scheme, LinkField::Host, host_text)?;
    if !host.is_ascii() {
        // When: the decoded `host` is non-ASCII, it is an internationalized name, which is refused.
        return Err(LinkRejection::new(
            scheme,
            LinkField::Host,
            "non-ASCII text, including internationalized names, is not accepted",
        ));
    }
    if host.parse::<Ipv4Addr>().is_err() && !is_valid_dns_name(&host) {
        // When: `host` is neither an IPv4 address nor an ASCII DNS name, it may carry an option or token.
        return Err(LinkRejection::new(
            scheme,
            LinkField::Host,
            "a host is an ASCII DNS name, an IPv4 address or a bracketed IPv6 address",
        ));
    }
    Ok(host)
}

/// Parse a decimal port in 1-65535; escapes and leading zeros are refused.
fn parse_port(port_text: &str) -> Result<u16, LinkRejection> {
    let invalid =
        LinkRejection::new(LinkScheme::Ssh, LinkField::Port, "a port is a number from 1 to 65535");
    let is_plain_decimal = !port_text.is_empty()
        && port_text.len() <= 5
        && port_text.bytes().all(|byte| byte.is_ascii_digit())
        && !port_text.starts_with('0');
    if !is_plain_decimal {
        // When: `is_plain_decimal` is false, the port is empty, too long, escaped or zero-padded.
        return Err(invalid);
    }
    match port_text.parse::<u32>() {
        Ok(port) if (1..=u32::from(u16::MAX)).contains(&port) => {
            u16::try_from(port).map_err(|_| invalid)
        }
        _ => Err(invalid),
    }
}

/// Parse `x-man-page:///page` or `x-man-page://section/page`.
///
/// `x-man-page://page`, with the page in the authority, is also accepted, as
/// Terminal.app accepts it.
fn parse_man_page(authority: &str, path: &str) -> Result<TerminalLink, LinkRejection> {
    let scheme = LinkScheme::ManPage;
    if authority.contains(';') || path.contains(';') {
        // When: `authority` or `path` holds `;`, it carries a parameter such as `;type=`.
        return Err(LinkRejection::new(
            scheme,
            LinkField::Path,
            "path parameters are not accepted",
        ));
    }
    let (section_text, page_text) = split_man_page(authority, path);
    if page_text.contains('/') {
        // When: `page_text` still holds `/`, the link has extra path segments.
        return Err(LinkRejection::new(
            scheme,
            LinkField::Path,
            "extra path segments are not accepted",
        ));
    }
    if page_text.is_empty() {
        // When: `page_text` is empty, the link names no page.
        return Err(LinkRejection::new(scheme, LinkField::Page, "the page is empty"));
    }
    let section = section_text.map(parse_section).transpose()?;
    let page = decode_component(scheme, LinkField::Page, page_text)?;
    if !is_valid_page(&page) {
        // When: the decoded `page` breaks the grammar, for example by starting with `-` or `.`.
        return Err(LinkRejection::new(
            scheme,
            LinkField::Page,
            "a page is letters, digits, '_', '.', '+', ':' or '-', not starting with '-' or '.'",
        ));
    }
    Ok(TerminalLink::ManPage { section, page })
}

/// Split a man page link into its optional section text and its page text.
fn split_man_page<'link>(
    authority: &'link str,
    path: &'link str,
) -> (Option<&'link str>, &'link str) {
    if authority.is_empty() {
        // When: `authority` is empty (`x-man-page:///page`), the path after its `/` is the page.
        return (None, path.strip_prefix('/').unwrap_or(path));
    }
    if path.is_empty() || path == "/" {
        // When: `path` is empty or `/` (`x-man-page://page`), the authority names the page.
        return (None, authority);
    }
    (Some(authority), &path[1..])
}

/// Decode and validate a man page section.
fn parse_section(section_text: &str) -> Result<String, LinkRejection> {
    let scheme = LinkScheme::ManPage;
    let section = decode_component(scheme, LinkField::Section, section_text)?;
    if !is_valid_section(&section) {
        // When: the decoded `section` breaks the grammar, it could become an option of `man`.
        return Err(LinkRejection::new(
            scheme,
            LinkField::Section,
            "a section is a digit or 'n' followed by up to 8 lowercase letters or digits",
        ));
    }
    Ok(section)
}

/// Percent-decode one raw component exactly once and reject unsafe results.
///
/// Raw delimiters were already split away, so any escape that decodes to `/`,
/// `@`, `:` or a control byte is refused, as are malformed escapes, an escaped
/// `%` (double encoding), and decoded text that is not valid UTF-8 or holds a
/// non-ASCII control character.
fn decode_component(
    scheme: LinkScheme,
    field: LinkField,
    raw: &str,
) -> Result<String, LinkRejection> {
    let bytes = raw.as_bytes();
    let mut index = 0;
    while let Some(offset) = bytes[index..].iter().position(|byte| *byte == b'%') {
        let start = index + offset;
        let escaped = bytes
            .get(start + 1..start + 3)
            .and_then(|digits| std::str::from_utf8(digits).ok())
            .filter(|digits| digits.bytes().all(|byte| byte.is_ascii_hexdigit()))
            .and_then(|digits| u8::from_str_radix(digits, 16).ok());
        let Some(escaped) = escaped else {
            // When: `escaped` is absent, `%` is not followed by two hex digits.
            return Err(LinkRejection::new(scheme, field, "a percent escape is malformed"));
        };
        if escaped == b'%' {
            // When: `escaped` is `%` (`%25`), the component is double-encoded.
            return Err(LinkRejection::new(scheme, field, "double encoding (%25) is not accepted"));
        }
        if matches!(escaped, b'/' | b'@' | b':') || escaped.is_ascii_control() {
            // When: `escaped` is a delimiter, NUL or control byte that the raw split would have seen.
            return Err(LinkRejection::new(scheme, field, ENCODED_DELIMITER));
        }
        index = start + 3;
    }
    let decoded: Vec<u8> = percent_encoding::percent_decode(bytes).collect();
    let text = String::from_utf8(decoded)
        .map_err(|_| LinkRejection::new(scheme, field, "the decoded text is not valid UTF-8"))?;
    if text.chars().any(char::is_control) {
        // When: `text` decodes to a non-ASCII control character such as U+0085.
        return Err(LinkRejection::new(scheme, field, ENCODED_DELIMITER));
    }
    Ok(text)
}

/// Whether `name` is an ASCII DNS name: dot-separated labels of letters, digits and inner `-`.
fn is_valid_dns_name(name: &str) -> bool {
    if name.is_empty() || name.len() > MAX_DNS_NAME_BYTES {
        // When: `name` is empty or longer than DNS allows.
        return false;
    }
    name.split('.').all(|label| {
        let bytes = label.as_bytes();
        !bytes.is_empty()
            && bytes.len() <= MAX_DNS_LABEL_BYTES
            && bytes.first().is_some_and(u8::is_ascii_alphanumeric)
            && bytes.last().is_some_and(u8::is_ascii_alphanumeric)
            && bytes.iter().all(|byte| byte.is_ascii_alphanumeric() || *byte == b'-')
    })
}

/// Whether `user` matches `^[A-Za-z0-9._][A-Za-z0-9._-]{0,31}$`.
fn is_valid_user(user: &str) -> bool {
    let bytes = user.as_bytes();
    let allowed = |byte: &u8| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-');
    (1..=32).contains(&bytes.len()) && bytes[0] != b'-' && bytes.iter().all(allowed)
}

/// Whether `section` matches `^[1-9n][a-z0-9]{0,8}$`.
fn is_valid_section(section: &str) -> bool {
    let bytes = section.as_bytes();
    let first_ok = bytes.first().is_some_and(|byte| matches!(byte, b'1'..=b'9' | b'n'));
    let rest_ok =
        bytes.iter().skip(1).all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit());
    first_ok && bytes.len() <= 9 && rest_ok
}

/// Whether `page` matches `^[A-Za-z0-9_.+:-]+$` and starts with neither `-` nor `.`.
fn is_valid_page(page: &str) -> bool {
    let bytes = page.as_bytes();
    let allowed = |byte: &u8| {
        byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b'+' | b':' | b'-')
    };
    !bytes.is_empty()
        && bytes.len() <= MAX_PAGE_BYTES
        && !matches!(bytes[0], b'-' | b'.')
        && bytes.iter().all(allowed)
}

#[cfg(test)]
#[path = "launch_url_tests.rs"]
mod launch_url_tests;
