//! Allocation-free preflight of native file lists. Walk borrowed lines and decoded characters;
//! only a staged offer whose lease covers its destination may allocate path strings or items.

/// Maximum number of native file items in one clipboard representation, on reads and writes.
pub(super) const ITEM_CAP: usize = 256;

/// Nonempty, non-comment URI lines. Refuse excessive lists before constructing native items.
pub(super) fn items(bytes: &[u8]) -> Option<impl Iterator<Item = &str> + Clone> {
    let lines = std::str::from_utf8(bytes)
        .ok()?
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'));
    let count = lines.clone().take(ITEM_CAP + 1).count();
    (1..=ITEM_CAP).contains(&count).then_some(lines)
}

/// A percent-encoded component, yielding one checked Unicode scalar at a time. An invalid
/// escape, malformed UTF-8 or control character yields None rather than a partial path.
#[derive(Clone)]
struct Decoded<'uri>(&'uri [u8]);

impl Decoded<'_> {
    fn byte(&mut self) -> Option<u8> {
        let (&byte, rest) = self.0.split_first()?;
        self.0 = rest;
        if byte != b'%' {
            // When: byte is literal, it contributes one octet without consuming an escape.
            return Some(byte);
        }
        let high = char::from(*rest.first()?).to_digit(16)?;
        let low = char::from(*rest.get(1)?).to_digit(16)?;
        self.0 = &rest[2..];
        Some((high * 16 + low) as u8)
    }
}

impl Iterator for Decoded<'_> {
    type Item = Option<char>;
    fn next(&mut self) -> Option<Self::Item> {
        if self.0.is_empty() {
            // When: the remaining input is_empty, there is no further scalar.
            return None;
        }
        Some((|| {
            let first = self.byte()?;
            let width = match first {
                0..=0x7f => 1,
                0xc2..=0xdf => 2,
                0xe0..=0xef => 3,
                0xf0..=0xf4 => 4,
                _ => {
                    // When: first is not a UTF-8 lead byte, the path is not decodable.
                    return None;
                }
            };
            let mut bytes = [first; 4];
            for slot in &mut bytes[1..width] {
                *slot = self.byte()?;
            }
            std::str::from_utf8(&bytes[..width])
                .ok()?
                .chars()
                .next()
                .filter(|scalar| !scalar.is_control())
        })())
    }
}

/// A decoded authority may not smuggle delimiters, credentials or Windows device namespaces.
fn valid_host(host: &str) -> bool {
    let decoded = Decoded(host.as_bytes());
    !decoded.clone().eq(".".chars().map(Some))
        && !decoded.clone().eq("..".chars().map(Some))
        && decoded.clone().all(|part| {
            part.is_some_and(|scalar| scalar.is_alphanumeric() || matches!(scalar, '.' | '-' | '_'))
        })
}

/// Split an absolute file URI into its checked host and encoded path (without the leading slash).
pub(super) fn file_parts(uri: &str) -> Option<(&str, &str)> {
    let (scheme, rest) = uri.split_once(':')?;
    if !scheme.eq_ignore_ascii_case("file")
        || rest.contains(['?', '#', '\\'])
        || rest.contains(char::is_whitespace)
    {
        // When: scheme or rest is not a plain file URI, do not publish it as a file item.
        return None;
    }
    let (host, path) = match rest.strip_prefix("//") {
        Some(authority) => authority.split_once('/')?,
        None => ("", rest.strip_prefix('/')?),
    };
    (valid_host(host)
        && Decoded(path.as_bytes()).all(|part| part.is_some_and(|scalar| scalar != '\\')))
    .then_some((host, path))
}

/// Whether a validated file authority denotes the local machine, after percent decoding.
pub(super) fn local_authority(host: &str) -> bool {
    host.is_empty()
        || Decoded(host.as_bytes())
            .flatten()
            .map(|scalar| scalar.to_ascii_lowercase())
            .eq("localhost".chars())
}

/// Emit a validated absolute drive or UNC path without allocating during preflight.
#[cfg(any(test, target_os = "windows"))]
pub(super) fn windows_path(uri: &str, mut emit: impl FnMut(char)) -> Option<()> {
    let (host, path) = file_parts(uri)?;
    let chars = Decoded(path.as_bytes()).flatten();
    if local_authority(host) {
        // When: host is empty or localhost, require a rooted drive path.
        let mut drive = chars.clone();
        if !drive.next()?.is_ascii_alphabetic() || drive.next()? != ':' || drive.next()? != '/' {
            // When: drive lacks a letter, colon and slash, the path is not absolute.
            return None;
        }
    } else {
        // When: host is remote, a nonempty share must follow it.
        if chars.clone().next().is_none_or(|first| first == '/') {
            // When: chars starts with a separator or is empty, there is no share name.
            return None;
        }
        emit('\\');
        emit('\\');
        Decoded(host.as_bytes()).flatten().for_each(&mut emit);
        emit('\\');
    }
    chars.map(|scalar| if scalar == '/' { '\\' } else { scalar }).for_each(emit);
    Some(())
}

/// Validate RFC 3986 components without allocating during pasteboard preflight. Component-specific
/// delimiters are allowed only at their own level; a percent escape always needs two hex digits.
#[cfg(target_os = "macos")]
fn component(value: &str, extra: &[u8]) -> bool {
    let mut bytes = value.bytes();
    while let Some(byte) = bytes.next() {
        if byte == b'%' {
            // When: byte introduces an escape, consume its encoded octet as one unit.
            if !(0..2).all(|_| bytes.next().is_some_and(|digit| digit.is_ascii_hexdigit())) {
                // When: bytes does not have two hexadecimal digits after %, the escape cannot be decoded.
                return false;
            }
        } else if !(byte.is_ascii_alphanumeric()
            || b"-._~!$&'()*+,;=".contains(&byte)
            || extra.contains(&byte))
        {
            // When: byte is neither unreserved nor allowed by extra, it is illegal in this component.
            return false;
        }
    }
    true
}

/// Parse userinfo, a reg-name or IP literal, and an optional decimal port.
#[cfg(target_os = "macos")]
fn authority(value: &str, require_host: bool) -> bool {
    let (user, server) = value.split_once('@').unwrap_or(("", value));
    if !component(user, b":") {
        // When: component rejects user information, the authority is not well-formed.
        return false;
    }
    let (host, port) = if let Some(literal) = server.strip_prefix('[') {
        // When: server starts with a bracket, the host must be an IP literal.
        let Some((address, tail)) = literal.split_once(']') else {
            // When: literal has no closing bracket, no host boundary can be identified.
            return false;
        };
        let future = address
            .strip_prefix(['v', 'V'])
            .and_then(|value| value.split_once('.'))
            .is_some_and(|(version, body)| {
                !version.is_empty()
                    && version.bytes().all(|digit| digit.is_ascii_hexdigit())
                    && !body.is_empty()
                    && !body.contains('%')
                    && component(body, b":")
            });
        if !future && address.parse::<std::net::Ipv6Addr>().is_err() {
            // When: address is neither IPvFuture nor IPv6, brackets cannot make it a host.
            return false;
        }
        let Some(port) = tail.strip_prefix(':').or(tail.is_empty().then_some("")) else {
            // When: tail is nonempty, only a colon followed by the port may follow the literal.
            return false;
        };
        (address, port)
    } else {
        // When: server has no bracket prefix, split the reg-name from its optional port.
        let (host, port) = server.split_once(':').unwrap_or((server, ""));
        if !component(host, b"") {
            // When: component rejects host, publishing it would require URL repair.
            return false;
        }
        (host, port)
    };
    (!require_host || !host.is_empty()) && port.bytes().all(|digit| digit.is_ascii_digit())
}

/// An absolute non-file URI suitable for a public.url pasteboard item, without URL repair.
#[cfg(target_os = "macos")]
pub(super) fn absolute_uri(value: &str) -> bool {
    let Some((scheme, remainder)) = value.split_once(':') else {
        // When: value has no scheme delimiter, this is not an absolute URI.
        return false;
    };
    let mut letters = scheme.bytes();
    if !letters.next().is_some_and(|first| first.is_ascii_alphabetic())
        || !letters.all(|byte| byte.is_ascii_alphanumeric() || b"+-.".contains(&byte))
    {
        // When: letters do not form a scheme, the URI has no valid scheme identifier.
        return false;
    }
    let (resource, fragment) = remainder.split_once('#').unwrap_or((remainder, ""));
    let (hierarchy, query) = resource.split_once('?').unwrap_or((resource, ""));
    let web = scheme.eq_ignore_ascii_case("http") || scheme.eq_ignore_ascii_case("https");
    let path = if let Some(network) = hierarchy.strip_prefix("//") {
        // When: hierarchy starts with two slashes, validate the authority separately from the path.
        let (server, path) = network.split_once('/').unwrap_or((network, ""));
        if !authority(server, web) {
            // When: authority rejects the server, the URI cannot be published as an absolute URL.
            return false;
        }
        path
    } else if web {
        // When: web has no network authority, it does not identify an HTTP resource.
        return false;
    } else {
        // When: hierarchy has no authority, its full value is the scheme-specific path.
        hierarchy
    };
    component(path, b"/:@") && component(query, b"/:@?") && component(fragment, b"/:@?")
}
