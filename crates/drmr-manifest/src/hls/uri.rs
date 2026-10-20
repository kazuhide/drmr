//! RFC 3986 reference syntax only; no resolution, normalization, DNS, or I/O.

fn component(value: &str, extra: &[u8]) -> bool {
    let bytes = value.as_bytes();
    let mut at = 0;
    while at < bytes.len() {
        let byte = bytes[at];
        if byte == b'%' {
            if !bytes
                .get(at + 1..at + 3)
                .is_some_and(|digits| digits.iter().all(u8::is_ascii_hexdigit))
            {
                return false;
            }
            at += 3;
        } else {
            if !byte.is_ascii_alphanumeric()
                && !b"-._~!$&'()*+,;=".contains(&byte)
                && !extra.contains(&byte)
            {
                return false;
            }
            at += 1;
        }
    }
    true
}

fn authority(value: &str) -> bool {
    let host_port = if let Some((user, rest)) = value.rsplit_once('@') {
        if !component(user, b":") {
            return false;
        }
        rest
    } else {
        value
    };
    if let Some(literal) = host_port.strip_prefix('[') {
        let Some((address, rest)) = literal.split_once(']') else {
            return false;
        };
        let future = address
            .strip_prefix('v')
            .or_else(|| address.strip_prefix('V'))
            .is_some_and(|value| {
                value.split_once('.').is_some_and(|(version, address)| {
                    !version.is_empty()
                        && version.bytes().all(|byte| byte.is_ascii_hexdigit())
                        && !address.is_empty()
                        && component(address, b":")
                        && !address.contains('%')
                })
            });
        let valid = future || address.parse::<std::net::Ipv6Addr>().is_ok();
        return valid
            && (rest.is_empty()
                || rest
                    .strip_prefix(':')
                    .is_some_and(|port| port.bytes().all(|byte| byte.is_ascii_digit())));
    }
    let (host, port) = host_port
        .rsplit_once(':')
        .map_or((host_port, None), |(host, port)| (host, Some(port)));
    component(host, b"") && port.is_none_or(|port| port.bytes().all(|byte| byte.is_ascii_digit()))
}

pub(super) fn valid(value: &str) -> bool {
    if value.is_empty() || !value.is_ascii() {
        return false;
    }
    let (rest, fragment) = value
        .split_once('#')
        .map_or((value, None), |(rest, fragment)| (rest, Some(fragment)));
    if fragment.is_some_and(|fragment| !component(fragment, b":@/?")) {
        return false;
    }
    let (rest, query) = rest
        .split_once('?')
        .map_or((rest, None), |(rest, query)| (rest, Some(query)));
    if query.is_some_and(|query| !component(query, b":@/?")) {
        return false;
    }
    let hierarchy = if let Some(colon) = rest
        .find(':')
        .filter(|colon| rest.find('/').is_none_or(|slash| *colon < slash))
    {
        let scheme = &rest[..colon];
        if !scheme
            .as_bytes()
            .first()
            .is_some_and(u8::is_ascii_alphabetic)
            || !scheme
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"+.-".contains(&byte))
        {
            return false;
        }
        &rest[colon + 1..]
    } else {
        rest
    };
    let path = if let Some(remote) = hierarchy.strip_prefix("//") {
        let (host, path) = remote
            .find('/')
            .map_or((remote, ""), |slash| (&remote[..slash], &remote[slash..]));
        if !authority(host) {
            return false;
        }
        path
    } else {
        hierarchy
    };
    component(path, b":@/")
}

#[cfg(test)]
mod tests {
    use super::valid;

    #[test]
    fn uri_components_preserve_opaque_identifiers_and_reject_malformed_references() {
        for value in [
            "part.ts",
            "../video/part%20one.m4s?opaque=a%2fb",
            "skd://asset/identifier",
            "data:text/plain;base64,AAAA=",
            "https://user:pass@[2001:db8::1]:443/a?b=c#d",
            "//example.invalid/path",
            "https://[v1.a:b]/part",
            "?query",
            "#fragment",
        ] {
            assert!(valid(value), "valid synthetic URI rejected");
        }
        for value in [
            "",
            "part with spaces.ts",
            "part%GG.ts",
            "part%2.ts",
            "https://[missing/part",
            "https://[::1]trailer/part",
            "https://host:not-a-port/part",
            "1:relative",
            "part[one].ts",
            "https://user@user@host/part",
            "part#one#two",
            "file\\part.ts",
            "コンテンツ.ts",
        ] {
            assert!(!valid(value), "invalid synthetic URI accepted");
        }
    }
}
