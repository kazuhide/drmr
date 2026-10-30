//! Bind supported HLS provider declarations to the actual initialization and known key.
use super::{DATA, FP_FORMAT, PR_FORMAT, WV_FORMAT, read_bounded, resource};
use crate::{
    concurrency::Stop,
    conversion::{ContentKey, ConversionError, InputKeys, OutputKey, OutputSignaling},
    inspection::{OutputFormat, TargetProtection},
    mp4,
};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use std::{collections::BTreeMap, path::Path};
use zeroize::Zeroizing;
const INVALID: ConversionError = ConversionError("hls-drm-signaling-invalid");

fn data_uri(uri: &str) -> Result<Zeroizing<Vec<u8>>, ConversionError> {
    let value = uri
        .strip_prefix(DATA)
        .or_else(|| uri.strip_prefix("data:application/octet-stream;base64,"))
        .ok_or(INVALID)?;
    if value.len() > 131_072 {
        return Err(INVALID);
    }
    let bytes = Zeroizing::new(STANDARD.decode(value).map_err(|_| INVALID)?);
    if bytes.is_empty() || bytes.len() > 65_536 || STANDARD.encode(&bytes) != value {
        return Err(INVALID);
    }
    Ok(bytes)
}
fn values<'a>(text: &'a str, prefix: &str) -> Vec<&'a str> {
    text.lines()
        .map(str::trim)
        .filter_map(|line| line.strip_prefix(prefix))
        .collect()
}
fn parse(values: &[&str]) -> Result<BTreeMap<String, Zeroizing<String>>, ConversionError> {
    let mut result = BTreeMap::new();
    for value in values {
        let attrs = crate::conversion::hls::attributes(value).map_err(|_| INVALID)?;
        let format = attrs.get("KEYFORMAT").ok_or(INVALID)?;
        if !matches!(format, WV_FORMAT | PR_FORMAT | FP_FORMAT)
            || attrs.len() != 4 + usize::from(attrs.get("IV").is_some())
            || attrs.get("METHOD") == Some("SAMPLE-AES-CTR") && attrs.get("IV").is_some()
            || attrs.get("KEYFORMATVERSIONS") != Some("1")
            || !matches!(attrs.get("METHOD"), Some("SAMPLE-AES" | "SAMPLE-AES-CTR"))
            || attrs.get("URI").is_none_or(str::is_empty)
            || result
                .insert(format.to_owned(), Zeroizing::new((*value).to_owned()))
                .is_some()
            || result.len() > 3
        {
            return Err(INVALID);
        }
    }
    Ok(result)
}
fn sample_provider(values: &[&str]) -> bool {
    values.iter().any(|value| {
        crate::conversion::hls::attributes(value).is_ok_and(|attrs| {
            attrs
                .get("KEYFORMAT")
                .is_some_and(|format| format != "identity")
                && matches!(attrs.get("METHOD"), Some("SAMPLE-AES" | "SAMPLE-AES-CTR"))
        })
    })
}
pub(in crate::conversion) fn validate_source(
    input: &Path,
    keys: &InputKeys,
    backend: &crate::conversion::backend::Backend,
) -> Result<(), ConversionError> {
    let stop = &backend.stop;
    super::super::super::cancelled(stop)?;
    let root = read_bounded(input)?;
    let root = std::str::from_utf8(&root).map_err(|_| INVALID)?;
    let sessions = values(root, "#EXT-X-SESSION-KEY:");
    for playlist in crate::conversion::presentation::hls_media_paths(input)? {
        super::super::super::cancelled(stop)?;
        let bytes = read_bounded(&playlist.path)?;
        let text = std::str::from_utf8(&bytes).map_err(|_| INVALID)?;
        let declarations = values(text, "#EXT-X-KEY:");
        if !text.contains("#EXT-X-MAP:")
            || (!sample_provider(&declarations) && !sample_provider(&sessions))
        {
            continue;
        }
        let declarations = parse(&declarations)?;
        let selected = parse(&sessions)?;
        for (format, session) in selected {
            let media = declarations.get(&format).ok_or(INVALID)?;
            let session = crate::conversion::hls::attributes(&session).map_err(|_| INVALID)?;
            let media = crate::conversion::hls::attributes(media).map_err(|_| INVALID)?;
            if session.len() != media.len()
                || session.iter().any(|(name, value)| match media.get(name) {
                    Some(expected) if name == "IV" => !super::iv_equivalent(value, expected),
                    expected => expected != Some(value),
                })
            {
                return Err(INVALID);
            }
        }
        let mut initialization = None;
        let mut saw_media = false;
        let mut resources = Vec::new();
        for line in text.lines().map(str::trim) {
            if line.starts_with("#EXT-X-KEY:") && (initialization.is_some() || saw_media) {
                return Err(INVALID);
            }
            if let Some(value) = line.strip_prefix("#EXT-X-MAP:") {
                let attrs = crate::conversion::hls::attributes(value).map_err(|_| INVALID)?;
                if initialization.is_some() || attrs.len() != 1 {
                    return Err(INVALID);
                }
                initialization = Some(resource(
                    playlist.path.parent().ok_or(INVALID)?,
                    attrs.get("URI").ok_or(INVALID)?,
                )?);
            } else if !line.is_empty() && !line.starts_with('#') {
                saw_media = true;
                resources.push(resource(playlist.path.parent().ok_or(INVALID)?, line)?);
            }
        }
        let initialization = initialization.ok_or(INVALID)?;
        for value in declarations.values() {
            let attrs = crate::conversion::hls::attributes(value).map_err(|_| INVALID)?;
            backend.runner.secrets().register(attrs.get("URI"));
        }
        let objects = bind(&initialization, &declarations, keys, stop)?;
        backend.runner.secrets().register(objects.secrets());
        for path in resources {
            super::super::media::check(
                &path,
                super::super::media::Selection::Providers(&objects),
                stop,
            )
            .map_err(|error| {
                if error.0 == "cancelled" {
                    error
                } else {
                    INVALID
                }
            })?;
        }
    }
    Ok(())
}
fn initialization_key(
    initialization: &Path,
    keys: &InputKeys,
) -> Result<(OutputKey, TargetProtection, Option<Zeroizing<String>>), ConversionError> {
    let bytes = read_bounded(initialization)?;
    let boxes = mp4::parse_boxes(&bytes).map_err(|_| INVALID)?;
    let nodes = mp4::walk(&boxes);
    let tenc: Vec<_> = nodes
        .iter()
        .filter(|node| node.kind() == *b"tenc")
        .collect();
    let schm: Vec<_> = nodes
        .iter()
        .filter(|node| node.kind() == *b"schm")
        .collect();
    if tenc.len() != 1 || schm.len() != 1 {
        return Err(INVALID);
    }
    let tenc = mp4::Tenc::parse(tenc[0].header.payload(&bytes)).ok_or(INVALID)?;
    let target = match schm[0].header.payload(&bytes).get(4..8) {
        Some(b"cenc") => TargetProtection::Cenc,
        Some(b"cbcs") => TargetProtection::Cbcs,
        _ => return Err(INVALID),
    };
    if tenc.default_is_protected != 1 {
        return Err(INVALID);
    }
    let key = keys
        .keys
        .iter()
        .find(|key| key.id == tenc.default_kid)
        .ok_or(ConversionError("input-key-required"))?;
    Ok((
        OutputKey(ContentKey {
            id: key.id,
            bytes: Zeroizing::new(*key.bytes),
        }),
        target,
        tenc.default_constant_iv
            .as_ref()
            .map(|iv| Zeroizing::new(hex::encode(iv))),
    ))
}
fn bind(
    initialization: &Path,
    declarations: &BTreeMap<String, Zeroizing<String>>,
    keys: &InputKeys,
    stop: &Stop,
) -> Result<super::super::MultiObjects, ConversionError> {
    let (key, target, iv) = initialization_key(initialization, keys)?;
    let mut widevine = None;
    let mut playready = None;
    let mut playready_pro = None;
    let mut fairplay = false;
    for (format, value) in declarations {
        let attrs = crate::conversion::hls::attributes(value).map_err(|_| INVALID)?;
        if attrs.get("METHOD") != Some(super::method(target)?) {
            return Err(INVALID);
        }
        super::validate_iv(&attrs, target, iv.as_deref().map(String::as_str))
            .map_err(|_| INVALID)?;
        let uri = attrs.get("URI").ok_or(INVALID)?;
        match format.as_str() {
            WV_FORMAT => {
                widevine = Some(Zeroizing::new(hex::encode(&*data_uri(uri)?)));
            }
            PR_FORMAT => {
                let encoded = Zeroizing::new(STANDARD.encode(&*data_uri(uri)?));
                let parsed = crate::playready::parse_pro(&encoded).map_err(|_| INVALID)?;
                if parsed.kid_values_b64
                    != [
                        crate::playready::kid_uuid_to_playready_b64(key.0.id_hex().as_str())
                            .map_err(|_| INVALID)?,
                    ]
                    || parsed.algid.as_deref()
                        != Some(if target == TargetProtection::Cenc {
                            "AESCTR"
                        } else {
                            "AESCBC"
                        })
                {
                    return Err(INVALID);
                }
                playready = Some(Zeroizing::new(parsed.la_url.ok_or(INVALID)?));
                playready_pro = Some(encoded);
            }
            FP_FORMAT => {
                if target != TargetProtection::Cbcs || !super::super::valid_fairplay_uri(uri) {
                    return Err(INVALID);
                }
                fairplay = true;
            }
            _ => return Err(INVALID),
        }
    }
    let signaling = OutputSignaling::multi_drm(
        widevine.as_deref().map(String::as_str),
        playready.as_deref().map(String::as_str),
        fairplay,
    )
    .map_err(|_| INVALID)?;
    signaling
        .validate_profile(OutputFormat::Hls, target)
        .map_err(|_| INVALID)?;
    let objects = signaling.multi_objects(&key, target).map_err(|_| INVALID)?;
    if playready_pro.as_ref().is_some_and(|pro| {
        objects
            .playready
            .as_ref()
            .is_none_or(|expected| expected.pro.as_str() != pro.as_str())
    }) {
        return Err(INVALID);
    }
    objects
        .init(initialization, None, &key, stop)
        .map_err(|error| {
            if error.0 == "cancelled" {
                error
            } else {
                INVALID
            }
        })?;
    Ok(objects)
}

/// Acquisition checks provider syntax only; key and container binding happens after retention.
pub(in crate::conversion) fn validate_retained_provider(
    method: &str,
    format: &str,
    uri: &str,
) -> Result<(), ConversionError> {
    let target = match method {
        "SAMPLE-AES" => TargetProtection::Cbcs,
        "SAMPLE-AES-CTR" => TargetProtection::Cenc,
        _ => return Err(INVALID),
    };
    let bytes = data_uri(uri)?;
    match format {
        WV_FORMAT => {
            crate::conversion::signaling::multi::validate_widevine(&bytes, None, Some(target))
                .map_err(|_| INVALID)
        }
        PR_FORMAT => {
            let pro = Zeroizing::new(STANDARD.encode(&bytes));
            let parsed = crate::playready::parse_pro(&pro).map_err(|_| INVALID)?;
            if parsed.algid.as_deref()
                == Some(if target == TargetProtection::Cenc {
                    "AESCTR"
                } else {
                    "AESCBC"
                })
            {
                Ok(())
            } else {
                Err(INVALID)
            }
        }
        _ => Err(INVALID),
    }
}
