//! Key identifier comparison with RFC 3986 section 5.2 relative resolution.
//! Absolute identifiers retain their exact token spelling, including opaque schemes.

use std::borrow::Cow;

pub(crate) const MAX_BASE_URI_BYTES: usize = 8192;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ReferenceResolutionError {
    MissingBase,
    InvalidUri,
    UnsupportedBase,
    ResourceLimit,
}

#[derive(Clone, Copy)]
struct Parts<'a> {
    scheme: Option<&'a str>,
    authority: Option<&'a str>,
    path: &'a str,
    query: Option<&'a str>,
    fragment: Option<&'a str>,
}

impl<'a> Parts<'a> {
    fn parse(value: &'a str) -> Result<Self, ReferenceResolutionError> {
        if !value.is_empty() && !crate::hls::valid_uri_reference(value) {
            return Err(ReferenceResolutionError::InvalidUri);
        }
        let (remaining, fragment) = value
            .split_once('#')
            .map_or((value, None), |(left, right)| (left, Some(right)));
        if fragment.is_some_and(|value| value.contains('#')) {
            return Err(ReferenceResolutionError::InvalidUri);
        }
        let (remaining, query) = remaining
            .split_once('?')
            .map_or((remaining, None), |(left, right)| (left, Some(right)));
        let first_end = remaining.find('/').unwrap_or(remaining.len());
        let (scheme, remaining) = if let Some(colon) = remaining[..first_end].find(':') {
            let scheme = &remaining[..colon];
            if scheme.is_empty()
                || !scheme.as_bytes()[0].is_ascii_alphabetic()
                || !scheme
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"+-.".contains(&byte))
            {
                return Err(ReferenceResolutionError::InvalidUri);
            }
            (Some(scheme), &remaining[colon + 1..])
        } else {
            (None, remaining)
        };
        let (authority, path) = if let Some(remaining) = remaining.strip_prefix("//") {
            let end = remaining.find('/').unwrap_or(remaining.len());
            (Some(&remaining[..end]), &remaining[end..])
        } else {
            (None, remaining)
        };
        Ok(Self {
            scheme,
            authority,
            path,
            query,
            fragment,
        })
    }
}

/// This is URI reference resolution, not the WHATWG URL algorithm: percent-encoded
/// dots and slashes retain their spelling and are never treated as path separators.
/// Nothing is requested from a URI, and absolute key identifiers remain opaque.
pub(crate) fn key_uri_identity<'a>(
    reference: &'a str,
    base_uri: Option<&str>,
) -> Result<Cow<'a, str>, ReferenceResolutionError> {
    let source = Parts::parse(reference)?;
    if source.scheme.is_some() {
        return Ok(Cow::Borrowed(reference));
    }
    let base_uri = base_uri.ok_or(ReferenceResolutionError::MissingBase)?;
    // Caller metadata must not amplify a tiny document into unbounded URI work.
    if base_uri.len() > MAX_BASE_URI_BYTES {
        return Err(ReferenceResolutionError::ResourceLimit);
    }
    let base = Parts::parse(base_uri).map_err(|_| ReferenceResolutionError::UnsupportedBase)?;
    let scheme = base
        .scheme
        .ok_or(ReferenceResolutionError::UnsupportedBase)?;
    if base.authority.is_none() && !base.path.starts_with('/') {
        return Err(ReferenceResolutionError::UnsupportedBase);
    }
    if (scheme.eq_ignore_ascii_case("http") || scheme.eq_ignore_ascii_case("https"))
        && base.authority == Some("")
    {
        return Err(ReferenceResolutionError::UnsupportedBase);
    }
    let (authority, path, query) = if source.authority.is_some() {
        (source.authority, remove_dots(source.path), source.query)
    } else if source.path.is_empty() {
        (
            base.authority,
            base.path.to_owned(),
            source.query.or(base.query),
        )
    } else if source.path.starts_with('/') {
        (base.authority, remove_dots(source.path), source.query)
    } else {
        let directory = base
            .path
            .rfind('/')
            .map_or("", |index| &base.path[..=index]);
        let directory = if directory.is_empty() && base.authority.is_some() {
            "/"
        } else {
            directory
        };
        (
            base.authority,
            remove_dots(&format!("{directory}{}", source.path)),
            source.query,
        )
    };
    let mut resolved = String::with_capacity(base_uri.len().saturating_add(reference.len()));
    resolved.push_str(scheme);
    resolved.push(':');
    if let Some(authority) = authority {
        resolved.push_str("//");
        resolved.push_str(authority);
    }
    resolved.push_str(&path);
    if let Some(query) = query {
        resolved.push('?');
        resolved.push_str(query);
    }
    if let Some(fragment) = source.fragment {
        resolved.push('#');
        resolved.push_str(fragment);
    }
    Ok(Cow::Owned(resolved))
}

fn remove_dots(path: &str) -> String {
    let mut output = Vec::new();
    let mut segments = path.split('/').peekable();
    while let Some(segment) = segments.next() {
        match segment {
            "." => {
                if segments.peek().is_none() {
                    output.push("");
                }
            }
            ".." => {
                if output.len() > 1 {
                    output.pop();
                }
                if segments.peek().is_none() {
                    output.push("");
                }
            }
            _ => output.push(segment),
        }
    }
    output.join("/")
}

#[cfg(test)]
mod tests {
    use super::*;
    const BASE: &str = "http://a/b/c/d;p?q";

    #[test]
    fn relative_resolution_matches_rfc3986_normal_and_abnormal_examples() {
        for (reference, expected) in [
            ("g:h", "g:h"),
            ("g", "http://a/b/c/g"),
            ("./g", "http://a/b/c/g"),
            ("g/", "http://a/b/c/g/"),
            ("/g", "http://a/g"),
            ("//g", "http://g"),
            ("?y", "http://a/b/c/d;p?y"),
            ("g?y", "http://a/b/c/g?y"),
            ("#s", "http://a/b/c/d;p?q#s"),
            ("g#s", "http://a/b/c/g#s"),
            ("g?y#s", "http://a/b/c/g?y#s"),
            (";x", "http://a/b/c/;x"),
            ("g;x", "http://a/b/c/g;x"),
            ("", "http://a/b/c/d;p?q"),
            (".", "http://a/b/c/"),
            ("./", "http://a/b/c/"),
            ("..", "http://a/b/"),
            ("../", "http://a/b/"),
            ("../g", "http://a/b/g"),
            ("../..", "http://a/"),
            ("../../", "http://a/"),
            ("../../g", "http://a/g"),
            ("../../../g", "http://a/g"),
            ("/./g", "http://a/g"),
            ("/../g", "http://a/g"),
            ("g/./h", "http://a/b/c/g/h"),
            ("g/../h", "http://a/b/c/h"),
            ("g?y/../x", "http://a/b/c/g?y/../x"),
            ("g#s/../x", "http://a/b/c/g#s/../x"),
        ] {
            assert_eq!(
                key_uri_identity(reference, Some(BASE)).unwrap(),
                expected,
                "{reference}"
            );
        }
    }

    #[test]
    fn encoded_separators_are_preserved_and_absolute_tokens_are_opaque() {
        assert_eq!(
            key_uri_identity("%2e%2e/g%2Fh", Some(BASE)).unwrap(),
            "http://a/b/c/%2e%2e/g%2Fh"
        );
        assert_eq!(
            key_uri_identity("skd://issuer/../Key?token=x#fragment", None).unwrap(),
            "skd://issuer/../Key?token=x#fragment"
        );
        assert_eq!(
            key_uri_identity("https://HOST/a/../key", Some(BASE)).unwrap(),
            "https://HOST/a/../key"
        );
        assert!(key_uri_identity("key.bin", None).is_err());
        assert!(key_uri_identity("key.bin", Some("relative/base")).is_err());
        assert!(key_uri_identity("key.bin", Some("skd:opaque")).is_err());
        assert!(key_uri_identity("bad%", Some(BASE)).is_err());
    }

    #[test]
    fn master_and_child_bases_compare_the_resolved_resource_not_relative_tokens() {
        let master = Some("https://manifest.invalid/master.m3u8");
        let child = Some("https://manifest.invalid/sub/media.m3u8");
        assert_ne!(
            key_uri_identity("key.bin", master).unwrap(),
            key_uri_identity("key.bin", child).unwrap()
        );
        assert_eq!(
            key_uri_identity("sub/key.bin", master).unwrap(),
            key_uri_identity("key.bin", child).unwrap()
        );
        assert_eq!(
            key_uri_identity("key.bin", master).unwrap(),
            key_uri_identity("../key.bin", child).unwrap()
        );
    }
}
