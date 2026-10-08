//! URI value semantics, independent of transport, transaction or codec policy.
//!
//! URI values follow `java.net.URI` component equality, not URL normalization. Parsing
//! borrows the original spelling; no DNS, percent decoding, path cleanup, default
//! ports or host rewriting occurs. Callers keep original bytes for exact receipts.
//!
//! Specification: <https://docs.oracle.com/en/java/javase/17/docs/api/java.base/java/net/URI.html>
//! Parser edge cases were checked against OpenJDK 17 URI.Parser (parseHierarchical,
//! parseAuthority, parseServer, parseHostname). This is a native implementation,
//! not a copy of its parser or a Java-compatible numeric hash implementation.

use std::cmp::Ordering;
use std::hash::{Hash, Hasher};

#[derive(Clone, Copy, Debug)]
struct Uri<'a> {
    scheme: Option<&'a str>,
    body: Body<'a>,
    fragment: Option<&'a str>,
}

#[derive(Clone, Copy, Debug)]
enum Body<'a> {
    Hierarchical {
        authority: Authority<'a>,
        path: &'a str,
        query: Option<&'a str>,
    },
    Opaque(&'a str),
}

#[derive(Clone, Copy, Debug)]
enum Authority<'a> {
    Absent,
    Server {
        user_info: Option<&'a str>,
        host: &'a str,
        port: Option<u32>,
    },
    Registry(&'a str),
}

impl Authority<'_> {
    fn rank(self) -> u8 {
        match self {
            Self::Absent => 0,
            Self::Server { .. } => 1,
            Self::Registry(_) => 2,
        }
    }
}

/// Syntax only: callers choose admission errors appropriate to their boundary.
pub(crate) fn validate(input: &str) -> bool {
    parse(input).is_some()
}

/// Total comparison even for invalid strings constructed through public Value.
/// Invalid values form a separate raw-text domain; admission rejects them.
pub(crate) fn compare(left: &str, right: &str) -> Ordering {
    match (parse(left), parse(right)) {
        (Some(left), Some(right)) => compare_uri(left, right),
        (None, None) => left.encode_utf16().cmp(right.encode_utf16()),
        (None, Some(_)) => Ordering::Less,
        (Some(_), None) => Ordering::Greater,
    }
}

/// Hash exactly the same logical components as compare; never the persisted key.
pub(crate) fn hash(input: &str, state: &mut impl Hasher) {
    let Some(uri) = parse(input) else {
        false.hash(state);
        input.hash(state);
        return;
    };
    true.hash(state);
    hash_component(uri.scheme, true, state);
    match uri.body {
        Body::Hierarchical {
            authority,
            path,
            query,
        } => {
            0_u8.hash(state);
            authority.rank().hash(state);
            match authority {
                Authority::Absent => {}
                Authority::Server {
                    user_info,
                    host,
                    port,
                } => {
                    hash_component(user_info, false, state);
                    hash_component(Some(host), true, state);
                    port.hash(state);
                }
                Authority::Registry(raw) => hash_component(Some(raw), false, state),
            }
            hash_component(Some(path), false, state);
            hash_component(query, false, state);
        }
        Body::Opaque(part) => {
            1_u8.hash(state);
            hash_component(Some(part), false, state);
        }
    }
    hash_component(uri.fragment, false, state);
}

fn parse(input: &str) -> Option<Uri<'_>> {
    let (main, fragment) = split_optional(input, '#');
    if !fragment.is_none_or(|value| valid_component(value, Component::General)) {
        return None;
    }
    let (scheme, rest) = match main.find([':', '/', '?']) {
        Some(index) if main.as_bytes()[index] == b':' => {
            let candidate = &main[..index];
            if !valid_scheme(candidate) {
                return None;
            }
            (Some(candidate), &main[index + 1..])
        }
        _ => (None, main),
    };
    let body = if scheme.is_some() && !rest.starts_with('/') {
        if rest.is_empty() || !valid_component(rest, Component::General) {
            return None;
        }
        Body::Opaque(rest)
    } else {
        let (location, query) = split_optional(rest, '?');
        if !query.is_none_or(|value| valid_component(value, Component::General)) {
            return None;
        }
        let (authority, path) = if let Some(after_slashes) = location.strip_prefix("//") {
            let end = after_slashes.find('/').unwrap_or(after_slashes.len());
            let raw = &after_slashes[..end];
            let path = &after_slashes[end..];
            let authority = if raw.is_empty() {
                // Source accepts an empty authority before a path, query or
                // fragment delimiter, but not a bare trailing "//".
                if path.is_empty() && query.is_none() && fragment.is_none() {
                    return None;
                }
                Authority::Absent
            } else {
                parse_authority(raw)?
            };
            (authority, path)
        } else {
            (Authority::Absent, location)
        };
        if !valid_component(path, Component::Path) {
            return None;
        }
        Body::Hierarchical {
            authority,
            path,
            query,
        }
    };
    Some(Uri {
        scheme,
        body,
        fragment,
    })
}

fn split_optional(input: &str, delimiter: char) -> (&str, Option<&str>) {
    match input.split_once(delimiter) {
        Some((before, after)) => (before, Some(after)),
        None => (input, None),
    }
}

fn valid_scheme(input: &str) -> bool {
    input
        .as_bytes()
        .first()
        .is_some_and(u8::is_ascii_alphabetic)
        && input
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'-' | b'.'))
}

#[derive(Clone, Copy)]
enum Component {
    General,
    Path,
    Registry,
    UserInfo,
}

fn unreserved(byte: u8) -> bool {
    byte.is_ascii_alphanumeric()
        || matches!(
            byte,
            b'_' | b'-' | b'.' | b'!' | b'~' | b'*' | b'\'' | b'(' | b')'
        )
}

fn valid_component(input: &str, component: Component) -> bool {
    let bytes = input.as_bytes();
    let mut offset = 0;
    while let Some(&byte) = bytes.get(offset) {
        if byte == b'%' {
            if !bytes.get(offset + 1).is_some_and(u8::is_ascii_hexdigit)
                || !bytes.get(offset + 2).is_some_and(u8::is_ascii_hexdigit)
            {
                return false;
            }
            offset += 3;
        } else if byte.is_ascii() {
            let extra = match component {
                Component::General => b";/?:@&=+$,[]".contains(&byte),
                Component::Path => b"/;:@&=+$,".contains(&byte),
                Component::Registry => b"$,;:@&=+".contains(&byte),
                Component::UserInfo => b";:&=+$,".contains(&byte),
            };
            if !unreserved(byte) && !extra {
                return false;
            }
            offset += 1;
        } else {
            let Some(character) = input[offset..].chars().next() else {
                return false;
            };
            if character.is_control() || character.is_whitespace() {
                return false;
            }
            offset += character.len_utf8();
        }
    }
    true
}

fn parse_authority(input: &str) -> Option<Authority<'_>> {
    if let Some(server) = parse_server(input) {
        return Some(server);
    }
    valid_component(input, Component::Registry).then_some(Authority::Registry(input))
}

fn parse_server(input: &str) -> Option<Authority<'_>> {
    let (user_info, address) = match input.split_once('@') {
        Some((user, rest)) => {
            if !valid_component(user, Component::UserInfo) {
                return None;
            }
            (Some(user), rest)
        }
        None => (None, input),
    };
    let (host, port_text) = if let Some(bracketed) = address.strip_prefix('[') {
        let closing = bracketed.find(']')?;
        if !valid_ipv6_literal(&bracketed[..closing]) {
            return None;
        }
        let end = closing + 2;
        let suffix = &address[end..];
        let port = if suffix.is_empty() {
            None
        } else {
            Some(suffix.strip_prefix(':')?)
        };
        (&address[..end], port)
    } else {
        let (host, port) = split_optional(address, ':');
        if !valid_hostname(host) && !valid_ipv4(host) {
            return None;
        }
        (host, port)
    };
    let port = match port_text {
        None | Some("") => None,
        Some(text) => {
            if !text.bytes().all(|byte| byte.is_ascii_digit()) {
                return None;
            }
            let number = text.bytes().try_fold(0_u32, |number, digit| {
                number.checked_mul(10)?.checked_add(u32::from(digit - b'0'))
            })?;
            if number > i32::MAX as u32 {
                return None;
            }
            Some(number)
        }
    };
    Some(Authority::Server {
        user_info,
        host,
        port,
    })
}

fn valid_hostname(input: &str) -> bool {
    let name = input.strip_suffix('.').unwrap_or(input);
    let mut labels = name.split('.');
    let mut count = 0;
    let mut last_starts_alpha = false;
    for label in &mut labels {
        let Some(first) = label.as_bytes().first() else {
            return false;
        };
        if !first.is_ascii_alphanumeric()
            || !label
                .as_bytes()
                .last()
                .is_some_and(u8::is_ascii_alphanumeric)
            || !label
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        {
            return false;
        }
        count += 1;
        last_starts_alpha = first.is_ascii_alphabetic();
    }
    count == 1 || last_starts_alpha
}

fn valid_ipv4(input: &str) -> bool {
    let mut count = 0;
    for part in input.split('.') {
        if part.is_empty()
            || part.len() > 3
            || !part.bytes().all(|byte| byte.is_ascii_digit())
            || !part.parse::<u16>().is_ok_and(|number| number <= 255)
        {
            return false;
        }
        count += 1;
    }
    count == 4
}

fn valid_ipv6_literal(input: &str) -> bool {
    let (address, scope) = split_optional(input, '%');
    if !scope.is_none_or(|scope| {
        !scope.is_empty()
            && scope
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.'))
    }) {
        return false;
    }
    // Only validate syntax. Expanding compressed groups would rewrite host
    // identity, and numeric IP address equality is not URI host-text equality.
    match address.split_once("::") {
        Some((left, right)) => ipv6_units(left, false)
            .zip(ipv6_units(right, true))
            .is_some_and(|(left, right)| left + right < 8),
        None => ipv6_units(address, true) == Some(8),
    }
}

fn ipv6_units(input: &str, allow_ipv4_tail: bool) -> Option<usize> {
    if input.is_empty() {
        return Some(0);
    }
    let mut units = 0;
    let mut parts = input.split(':').peekable();
    while let Some(part) = parts.next() {
        if allow_ipv4_tail && parts.peek().is_none() && part.contains('.') {
            if !valid_ipv4(part) {
                return None;
            }
            units += 2;
        } else {
            if part.is_empty() || part.len() > 4 || !part.bytes().all(|b| b.is_ascii_hexdigit()) {
                return None;
            }
            units += 1;
        }
        if units > 8 {
            return None;
        }
    }
    Some(units)
}

fn compare_uri(left: Uri<'_>, right: Uri<'_>) -> Ordering {
    compare_component(left.scheme, right.scheme, true)
        .then_with(|| match (left.body, right.body) {
            (Body::Opaque(left), Body::Opaque(right)) => {
                compare_component(Some(left), Some(right), false)
            }
            (Body::Opaque(_), Body::Hierarchical { .. }) => Ordering::Greater,
            (Body::Hierarchical { .. }, Body::Opaque(_)) => Ordering::Less,
            (
                Body::Hierarchical {
                    authority: left_authority,
                    path: left_path,
                    query: left_query,
                },
                Body::Hierarchical {
                    authority: right_authority,
                    path: right_path,
                    query: right_query,
                },
            ) => compare_authority(left_authority, right_authority)
                .then_with(|| compare_component(Some(left_path), Some(right_path), false))
                .then_with(|| compare_component(left_query, right_query, false)),
        })
        .then_with(|| compare_component(left.fragment, right.fragment, false))
}

fn compare_authority(left: Authority<'_>, right: Authority<'_>) -> Ordering {
    // Native Ord-law adaptation: Java URI.compareTo falls back to raw authority
    // text if either side is registry-based (OpenJDK 17 URI.java:1600-1616).
    // From that rule, server aliases "http://A" == "http://a" fall on opposite
    // sides of registry "http://Z_". This inferred counterexample cannot be an
    // ordered-key contract. Rank authority kinds before within-kind comparison;
    // preserve Java equality and within-kind order, not its mixed-kind fallback.
    left.rank()
        .cmp(&right.rank())
        .then_with(|| match (left, right) {
            (Authority::Absent, Authority::Absent) => Ordering::Equal,
            (Authority::Registry(left), Authority::Registry(right)) => {
                compare_component(Some(left), Some(right), false)
            }
            (
                Authority::Server {
                    user_info: left_user,
                    host: left_host,
                    port: left_port,
                },
                Authority::Server {
                    user_info: right_user,
                    host: right_host,
                    port: right_port,
                },
            ) => compare_component(left_user, right_user, false)
                .then_with(|| compare_component(Some(left_host), Some(right_host), true))
                .then_with(|| left_port.cmp(&right_port)),
            _ => unreachable!("equal authority ranks have the same variant"),
        })
}

/// Normalized UTF-16 units, without allocating or decoding percent escapes.
fn component_units(input: &str, fold_case: bool) -> impl Iterator<Item = u16> + '_ {
    let mut escaped = 0;
    input.encode_utf16().map(move |unit| {
        if fold_case {
            if unit <= u16::from(b'Z') && unit >= u16::from(b'A') {
                unit + u16::from(b'a' - b'A')
            } else {
                unit
            }
        } else if escaped > 0 {
            escaped -= 1;
            if unit <= u16::from(b'f') && unit >= u16::from(b'a') {
                unit - u16::from(b'a' - b'A')
            } else {
                unit
            }
        } else {
            if unit == u16::from(b'%') {
                escaped = 2;
            }
            unit
        }
    })
}

fn compare_component(left: Option<&str>, right: Option<&str>, fold_case: bool) -> Ordering {
    match (left, right) {
        (None, None) => Ordering::Equal,
        (None, Some(_)) => Ordering::Less,
        (Some(_), None) => Ordering::Greater,
        (Some(left), Some(right)) => {
            component_units(left, fold_case).cmp(component_units(right, fold_case))
        }
    }
}

fn hash_component(component: Option<&str>, fold_case: bool, state: &mut impl Hasher) {
    component.is_some().hash(state);
    if let Some(component) = component {
        for unit in component_units(component, fold_case) {
            unit.hash(state);
        }
        // A sentinel outside UTF-16's domain separates adjacent components.
        0x1_0000_u32.hash(state);
    }
}
