//! The Go relay's exact-origin policy, without DNS or browser-style URL normalization.
//! Host parsing follows Go's BSD-licensed net/url rules; see THIRD_PARTY_NOTICES.md.

use std::net::Ipv6Addr;

/// Requires a usable HTTP origin before comparing its original spelling or decoded authority.
pub(super) fn allowed(origin: &str, host: &str, configured: &[String]) -> bool {
    if origin.is_empty() {
        return true;
    }
    let Some(authority) = authority(origin) else {
        return false;
    };
    if configured.is_empty() {
        authority == host
    } else {
        configured.iter().any(|allowed| allowed == origin)
    }
}

/// Preserves net/url's empty query/fragment and host spelling; neither approves an upstream target.
fn authority(origin: &str) -> Option<String> {
    let (without_fragment, fragment) = origin.split_once('#').unwrap_or((origin, ""));
    let (without_query, query) = without_fragment
        .split_once('?')
        .unwrap_or((without_fragment, ""));
    if !fragment.is_empty() || !query.is_empty() {
        return None;
    }
    let (scheme, host) = without_query.split_once("://")?;
    if !(scheme.eq_ignore_ascii_case("http") || scheme.eq_ignore_ascii_case("https"))
        || host.is_empty()
        || host.contains(['/', '@'])
    {
        return None;
    }
    if let Some(literal) = host.strip_prefix('[') {
        let (literal, port) = literal.rsplit_once(']')?;
        if !optional_port(port) {
            return None;
        }
        let decoded = match literal.find("%25") {
            Some(index) => {
                let mut decoded = unescape(&literal[..index], false)?;
                decoded.push_str(&unescape(&literal[index..], true)?);
                decoded
            }
            None => unescape(literal, false)?,
        };
        let address = match decoded.split_once('%') {
            Some((address, zone)) if !zone.is_empty() => address,
            Some(_) => return None,
            None => &decoded,
        };
        address.parse::<Ipv6Addr>().ok()?;
        return Some(format!("[{decoded}]{port}"));
    }
    if host.contains('[') {
        return None;
    }
    if let Some((_, port)) = host.split_once(':')
        && !port.bytes().all(|byte| byte.is_ascii_digit())
    {
        return None;
    }
    unescape(host, false)
}

/// A present port may be empty but otherwise contains only decimal digits, without range rewriting.
fn optional_port(port: &str) -> bool {
    port.is_empty()
        || port
            .strip_prefix(':')
            .is_some_and(|port| port.bytes().all(|byte| byte.is_ascii_digit()))
}

/// Decodes host escapes and the IPv6 zone exception while rejecting control/authority delimiters.
fn unescape(raw: &str, zone: bool) -> Option<String> {
    let mut result = Vec::with_capacity(raw.len());
    let mut bytes = raw.bytes();
    while let Some(byte) = bytes.next() {
        if byte == b'%' {
            let high = char::from(bytes.next()?).to_digit(16)?;
            let low = char::from(bytes.next()?).to_digit(16)?;
            let decoded = u8::try_from(high * 16 + low).ok()?;
            if decoded != b'%'
                && if zone {
                    decoded != b' ' && !host_byte(decoded)
                } else {
                    decoded.is_ascii()
                }
            {
                return None;
            }
            result.push(decoded);
        } else {
            if byte.is_ascii() && !host_byte(byte) {
                return None;
            }
            result.push(byte);
        }
    }
    String::from_utf8(result).ok()
}

/// URL host characters, including Go's retained host sub-delimiters; not a DNS allowlist.
fn host_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric()
        || matches!(
            byte,
            b'-' | b'_'
                | b'.'
                | b'~'
                | b'!'
                | b'$'
                | b'&'
                | b'\''
                | b'('
                | b')'
                | b'*'
                | b'+'
                | b','
                | b';'
                | b'='
                | b':'
                | b'['
                | b']'
                | b'<'
                | b'>'
                | b'"'
        )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

    #[derive(Deserialize)]
    struct Case {
        name: String,
        origin: String,
        host: String,
        configured: Vec<String>,
        accepted: bool,
    }

    /// The same corpus is checked against the actual Go relay, including case/zone/empty-tail edges.
    #[test]
    fn origin_policy_matches_the_shared_go_cases() {
        let cases: Vec<Case> =
            serde_json::from_str(include_str!("../../tests/fixtures/origins.json")).unwrap();
        for case in cases {
            assert_eq!(
                allowed(&case.origin, &case.host, &case.configured),
                case.accepted,
                "{}",
                case.name
            );
        }
    }
}
