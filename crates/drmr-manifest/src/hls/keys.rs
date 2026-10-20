use super::{
    check, rule,
    syntax::{self, Attribute, Line},
    unknown_attributes,
};
use crate::{CheckStatus, DiagnosticSink};
use crate::{
    NotCheckedReason, Span,
    uri::{ReferenceResolutionError, key_uri_identity},
};
use sha2::{Digest as _, Sha256};
use std::collections::BTreeMap;

/// Comparison receipts contain no source identifiers or payload bytes.
#[derive(Clone, Debug)]
pub(crate) struct KeyFact {
    pub unresolved: Option<NotCheckedReason>,
    pub uri: [u8; 32],
    pub matching: [u8; 32],
    pub duplicate: [u8; 32],
    pub span: Span,
}

pub(super) fn fact(
    attributes: &[Attribute<'_>],
    span: Span,
    base_uri: Option<&str>,
) -> Option<KeyFact> {
    let method = syntax::get(attributes, "METHOD")?.value;
    if method == "NONE" {
        return None;
    }
    let uri = syntax::get(attributes, "URI")?.value;
    let identity = key_uri_identity(uri, base_uri);
    let unresolved = identity.as_ref().err().map(|error| match error {
        ReferenceResolutionError::MissingBase | ReferenceResolutionError::UnsupportedBase => {
            NotCheckedReason::MissingReferenceBase
        }
        ReferenceResolutionError::InvalidUri => NotCheckedReason::UnsupportedFeature,
        ReferenceResolutionError::ResourceLimit => NotCheckedReason::ResourceLimit,
    });
    let format =
        syntax::get(attributes, "KEYFORMAT").map_or("identity", |attribute| attribute.value);
    let versions =
        syntax::get(attributes, "KEYFORMATVERSIONS").map_or("1", |attribute| attribute.value);
    let explicit_iv = syntax::get(attributes, "IV");
    let iv = explicit_iv.map_or("", |attribute| attribute.value);
    let iv = iv
        .strip_prefix("0x")
        .or_else(|| iv.strip_prefix("0X"))
        .unwrap_or(iv)
        .trim_start_matches('0')
        .to_ascii_lowercase();
    Some(KeyFact {
        unresolved,
        uri: fingerprint(&[identity.as_deref().unwrap_or(uri)]),
        matching: fingerprint(&[method, format, versions]),
        duplicate: fingerprint(&[
            uri,
            method,
            format,
            versions,
            if explicit_iv.is_some() {
                "explicit"
            } else {
                "implicit"
            },
            &iv,
        ]),
        span,
    })
}

pub(super) fn fingerprint(parts: &[&str]) -> [u8; 32] {
    let mut digest = Sha256::new();
    for part in parts {
        digest.update((part.len() as u64).to_be_bytes());
        digest.update(part.as_bytes());
    }
    digest.finalize().into()
}

#[derive(Clone, Copy)]
struct Effective<'a> {
    method: &'a str,
    missing_map_iv: bool,
    unknown: bool,
}

#[derive(Default)]
pub(super) struct State<'a> {
    formats: BTreeMap<&'a str, Effective<'a>>,
    methods: BTreeMap<&'a str, usize>,
    unknown: usize,
    missing_map_iv: usize,
}

impl<'a> State<'a> {
    pub fn declare(
        &mut self,
        attributes: &[Attribute<'a>],
        line: Line<'_>,
        minimum: &mut u64,
        sink: &mut DiagnosticSink<'_>,
    ) {
        validate(attributes, line, sink);
        let method = syntax::get(attributes, "METHOD");
        let iv = syntax::get(attributes, "IV");
        let format = syntax::get(attributes, "KEYFORMAT");
        if iv.is_some() {
            *minimum = (*minimum).max(2);
        }
        if format.is_some() || syntax::get(attributes, "KEYFORMATVERSIONS").is_some() {
            *minimum = (*minimum).max(5);
        }
        let Some(method) = method else {
            return;
        };
        if method.value == "NONE" {
            *self = Self::default();
            return;
        }
        let state = Effective {
            method: method.value,
            missing_map_iv: method.value == "AES-128" && iv.is_none(),
            unknown: !matches!(method.value, "AES-128" | "SAMPLE-AES"),
        };
        if let Some(previous) = self
            .formats
            .insert(format.map_or("identity", |format| format.value), state)
        {
            self.remove(previous);
        }
        *self.methods.entry(state.method).or_default() += 1;
        self.unknown += usize::from(state.unknown);
        self.missing_map_iv += usize::from(state.missing_map_iv);
    }

    fn remove(&mut self, previous: Effective<'a>) {
        if let Some(count) = self.methods.get_mut(previous.method) {
            *count -= 1;
            if *count == 0 {
                self.methods.remove(previous.method);
            }
        }
        self.unknown -= usize::from(previous.unknown);
        self.missing_map_iv -= usize::from(previous.missing_map_iv);
    }

    pub fn check_methods(&self, span: Span, sink: &mut DiagnosticSink<'_>) {
        let requirement = rule(
            "HLS-KEY-METHOD-CONSISTENCY",
            "hls-effective-encryption-methods-differ",
            "6.2.3",
        );
        if self.unknown > 0 {
            sink.check(
                requirement,
                CheckStatus::NotChecked(NotCheckedReason::UnsupportedFeature),
                Some(span),
            );
        } else {
            check(sink, requirement, self.methods.len() <= 1, span);
        }
    }

    pub fn check_map(&self, span: Span, sink: &mut DiagnosticSink<'_>) {
        self.check_methods(span, sink);
        let requirement = rule("HLS-MAP-KEY-IV", "hls-encrypted-map-iv-required", "4.3.2.5");
        if self.missing_map_iv > 0 {
            check(sink, requirement, false, span);
        } else if self.unknown > 0 {
            sink.check(
                requirement,
                CheckStatus::NotChecked(NotCheckedReason::UnsupportedFeature),
                Some(span),
            );
        } else {
            check(sink, requirement, true, span);
        }
    }
}

pub(super) fn validate(
    attributes: &[Attribute<'_>],
    line: Line<'_>,
    sink: &mut DiagnosticSink<'_>,
) {
    unknown_attributes(
        attributes,
        &["METHOD", "URI", "IV", "KEYFORMAT", "KEYFORMATVERSIONS"],
        sink,
    );
    super::attribute_types(
        attributes,
        &["URI", "KEYFORMAT", "KEYFORMATVERSIONS"],
        &["METHOD", "IV"],
        sink,
    );
    let method = syntax::get(attributes, "METHOD");
    let uri = syntax::get(attributes, "URI");
    let iv = syntax::get(attributes, "IV");
    let format = syntax::get(attributes, "KEYFORMAT");
    let versions = syntax::get(attributes, "KEYFORMATVERSIONS");
    let valid = method.is_some()
        && if method.is_some_and(|method| method.value == "NONE") {
            attributes.len() == 1
        } else {
            uri.is_some_and(|uri| {
                !uri.value.is_empty() && !uri.value.chars().any(char::is_whitespace)
            }) && iv.is_none_or(|iv| {
                iv.value
                    .strip_prefix("0x")
                    .or_else(|| iv.value.strip_prefix("0X"))
                    .is_some_and(|digits| {
                        !digits.is_empty()
                            && digits.len() <= 32
                            && digits.bytes().all(|byte| byte.is_ascii_hexdigit())
                    })
            }) && format.is_none_or(|format| !format.value.is_empty())
                && versions.is_none_or(|versions| {
                    versions
                        .value
                        .split('/')
                        .all(|value| syntax::integer(value).is_some_and(|number| number > 0))
                })
        };
    check(
        sink,
        rule("HLS-KEY", "hls-key-declaration-invalid", "4.3.2.4"),
        valid,
        line.span,
    );
    if let Some(uri) = uri {
        super::uri_check(uri, sink);
    }
    let Some(method) = method else {
        return;
    };
    if method.value == "NONE" {
        return;
    }
    let known = matches!(method.value, "AES-128" | "SAMPLE-AES");
    if known {
        sink.check(
            rule(
                "HLS-KEY-METHOD",
                "hls-encryption-method-not-checked",
                "4.3.2.4",
            ),
            CheckStatus::Passed,
            Some(method.span),
        );
    } else {
        sink.check(
            rule(
                "HLS-KEY-METHOD",
                "hls-encryption-method-not-checked",
                "4.3.2.4",
            ),
            CheckStatus::NotChecked(NotCheckedReason::UnsupportedFeature),
            Some(method.span),
        );
    }
}
