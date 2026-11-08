//! Private validators for explicit acquisition resume; ordinary fetches stay unconditional.

use super::{
    Attempt, ByteRange, Failure, Fetched, Fetcher, Request, Response, Sink, Step, fatal,
    fault_to_attempt,
};
use sha2::{Digest as _, Sha256};
use std::fmt;

/// A bounded, strong entity tag. Raw values are used only in private receipts and headers.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct StrongEtag(String);

impl StrongEtag {
    /// Accept the ASCII subset of RFC 9110 section 8.8.3, including its surrounding quotes.
    /// Weak tags and unsupported values simply disable conditional reuse.
    pub(crate) fn parse(value: &str) -> Option<Self> {
        let bytes = value.as_bytes();
        if !(2..=1024).contains(&bytes.len())
            || bytes.first() != Some(&b'"')
            || bytes.last() != Some(&b'"')
            || !bytes[1..bytes.len() - 1]
                .iter()
                .all(|byte| *byte == 0x21 || (0x23..=0x7e).contains(byte))
        {
            return None;
        }
        Some(Self(value.to_string()))
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for StrongEtag {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("StrongEtag([redacted])")
    }
}

/// Prior immutable bytes, already verified by the caller and bound to the current range.
#[derive(Clone)]
pub(crate) struct Revalidation {
    pub(crate) effective_url_sha256: String,
    pub(crate) strong_etag: StrongEtag,
    pub(crate) cached_size: u64,
    pub(crate) byte_range: Option<ByteRange>,
}

impl Revalidation {
    pub(super) fn matches(&self, url: &str) -> bool {
        hex::encode(Sha256::digest(url.as_bytes())) == self.effective_url_sha256
    }

    pub(super) fn check_range(&self, byte_range: Option<ByteRange>) -> Result<(), Failure> {
        if self.byte_range != byte_range
            || byte_range.is_some_and(|range| range.length == 0 || range.length != self.cached_size)
        {
            return Err(Failure::source("invalid-byte-range"));
        }
        if self.cached_size == 0 {
            return Err(Failure::source("empty-source-resource"));
        }
        Ok(())
    }
}

impl fmt::Debug for Revalidation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Revalidation")
            .field("cached_size", &self.cached_size)
            .field("byte_range", &self.byte_range)
            .finish_non_exhaustive()
    }
}

/// A current response either created a fresh private writer or revalidated prior bytes.
pub(crate) enum RevalidatedFetch {
    Fresh {
        fetched: Fetched,
        created: crate::native::FileIdentity,
        etag: Option<StrongEtag>,
    },
    NotModified {
        effective: String,
    },
}

impl fmt::Debug for RevalidatedFetch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Fresh { fetched, etag, .. } => f
                .debug_struct("Fresh")
                .field("size", &fetched.size)
                .field("has_validator", &etag.is_some())
                .finish_non_exhaustive(),
            Self::NotModified { .. } => f.write_str("NotModified { .. }"),
        }
    }
}

/// Claim extra cache-copy space using the same concurrent free-disk floor as body writes.
#[must_use = "Keep the reservation alive while copying."]
pub(crate) struct CopyReservation<'f> {
    pub(super) fetcher: &'f Fetcher,
    pub(super) size: u128,
}

impl Drop for CopyReservation<'_> {
    fn drop(&mut self) {
        self.fetcher.release_disk(self.size);
    }
}

#[derive(Default)]
pub(super) struct State {
    pub(super) record: bool,
    pub(super) prior: Option<Revalidation>,
    pub(super) etag: Option<StrongEtag>,
    pub(super) not_modified: bool,
}

impl State {
    pub(super) fn record(&mut self, response: &Response<'_>) {
        if self.record {
            self.etag = response
                .header("ETag", None)
                .and_then(|value| StrongEtag::parse(&value));
        }
    }
}

pub(super) fn reuse(
    response: &Response<'_>,
    request: &Request<'_>,
    sink: &mut Sink<'_>,
) -> Result<Step, Attempt> {
    let prior = request
        .revalidation
        .ok_or_else(|| fatal("source-http-failed"))?;
    check_not_modified(response, prior)?;
    sink.reuse(prior.cached_size).map_err(fault_to_attempt)?;
    // A 304 has no content. Close this connection rather than trusting any
    // unsolicited bytes buffered beyond its headers as a future response.
    Ok(Step::Done { keep: false })
}

/// Do not combine a caller's preconditions with the private resume protocol.
pub(super) fn check_headers(headers: &[(String, String)]) -> Result<(), Failure> {
    if headers.iter().any(|(name, _)| {
        [
            "if-match",
            "if-none-match",
            "if-modified-since",
            "if-unmodified-since",
            "if-range",
        ]
        .iter()
        .any(|conditional| name.eq_ignore_ascii_case(conditional))
    }) {
        return Err(Failure::source("invalid-fetch-policy"));
    }
    Ok(())
}

/// A 304 is metadata only. For exact-byte reuse, require the submitted strong tag back.
pub(super) fn check_not_modified(
    response: &Response<'_>,
    prior: &Revalidation,
) -> Result<(), Attempt> {
    let invalid = || fatal("source-http-failed");
    let etag = response
        .header("ETag", None)
        .and_then(|value| StrongEtag::parse(&value));
    if etag.as_ref() != Some(&prior.strong_etag)
        || response.header("Transfer-Encoding", None).is_some()
        || response.header("Content-Range", None).is_some()
    {
        return Err(invalid());
    }
    if response
        .header("Content-Encoding", Some("identity"))
        .is_none_or(|encoding| !encoding.eq_ignore_ascii_case("identity"))
    {
        return Err(fatal("unsupported-content-encoding"));
    }
    // RFC 9110 sections 8.6 and 14.2: Range is ignored on 304; this optional
    // length describes the complete representation, not the cached byte slice.
    if let Some(value) = response.header("Content-Length", None) {
        if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err(invalid());
        }
        let length = value.parse::<u128>().map_err(|_| invalid())?;
        let valid = match prior.byte_range {
            Some(range) => length >= u128::from(range.offset) + u128::from(range.length),
            None => length == u128::from(prior.cached_size),
        };
        if !valid {
            return Err(invalid());
        }
    }
    Ok(())
}
