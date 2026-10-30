//! Exact HLS provider declarations, sharing the generated MP4 provider records.
use super::{
    ConversionError, MISMATCH, MultiObjects, OutputKey, OutputSignaling, graph, media,
    read_bounded, resource, unchanged, version,
};
use crate::{
    concurrency::Stop,
    conversion::{DrmSystem, FrozenSource},
    inspection::{OutputFormat, TargetProtection},
};
use std::{collections::BTreeMap, fmt::Write as _, io::Write as _, path::Path};
use zeroize::Zeroizing;

mod source;
pub(in crate::conversion) use source::{validate_retained_provider, validate_source};
const WV_FORMAT: &str = "urn:uuid:edef8ba9-79d6-4ace-a3c8-27dcd51d21ed";
const PR_FORMAT: &str = "com.microsoft.playready";
const FP_FORMAT: &str = "com.apple.streamingkeydelivery";
const DATA: &str = "data:text/plain;base64,";

pub(super) fn method(target: TargetProtection) -> Result<&'static str, ConversionError> {
    match target {
        TargetProtection::Cenc => Ok("SAMPLE-AES-CTR"),
        TargetProtection::Cbcs => Ok("SAMPLE-AES"),
        _ => Err(super::super::UNSUPPORTED),
    }
}
fn tag(
    target: TargetProtection,
    format: &str,
    uri: &str,
) -> Result<Zeroizing<String>, ConversionError> {
    Ok(Zeroizing::new(format!(
        "#EXT-X-KEY:METHOD={},URI=\"{uri}\",KEYFORMAT=\"{format}\",KEYFORMATVERSIONS=\"1\"",
        method(target)?
    )))
}
impl MultiObjects {
    fn uris(
        &self,
        fairplay: bool,
        uri: Option<&str>,
    ) -> Result<BTreeMap<&'static str, Zeroizing<String>>, ConversionError> {
        let mut values = BTreeMap::new();
        if let Some(uri) = &self.widevine_uri {
            values.insert(WV_FORMAT, uri.clone());
        }
        if let Some(uri) = &self.playready_uri {
            values.insert(PR_FORMAT, uri.clone());
        }
        if fairplay {
            let uri = uri
                .filter(|uri| super::valid_fairplay_uri(uri))
                .ok_or(super::super::INVALID)?;
            values.insert(FP_FORMAT, Zeroizing::new(uri.to_owned()));
        }
        Ok(values)
    }
    fn key_tags(
        &self,
        fairplay: bool,
        uri: Option<&str>,
    ) -> Result<Zeroizing<String>, ConversionError> {
        let mut result = Zeroizing::new(String::new());
        for (format, uri) in self.uris(fairplay, uri)? {
            if !result.is_empty() {
                result.push('\n');
            }
            result.push_str(&tag(self.target, format, &uri)?);
        }
        Ok(result)
    }
    pub(in crate::conversion) fn finish_hls(
        &self,
        frozen: &FrozenSource,
        key: &OutputKey,
        fairplay: bool,
        uri: Option<&str>,
        stop: &Stop,
    ) -> Result<(), ConversionError> {
        let expected = self.uris(fairplay, uri)?;
        let mut graph = graph::Resources::new(frozen, stop)?;
        let input = frozen.entrypoint();
        graph.assign(input, graph::Role::Manifest)?;
        let root = read_bounded(input)?;
        let root = std::str::from_utf8(&root).map_err(|_| MISMATCH)?;
        let sessions: Vec<_> = root
            .lines()
            .filter_map(|line| line.strip_prefix("#EXT-X-SESSION-KEY:"))
            .collect();
        version(root, 5, !sessions.is_empty())?;
        for playlist in crate::conversion::presentation::hls_media_paths(input)? {
            super::super::cancelled(stop)?;
            graph.assign(&playlist.path, graph::Role::Manifest)?;
            let bytes = read_bounded(&playlist.path)?;
            let text = std::str::from_utf8(&bytes).map_err(|_| MISMATCH)?;
            version(text, 6, true)?;
            let mut values = Vec::new();
            let mut maps = 0;
            let mut media_count = 0;
            for line in text.lines().map(str::trim) {
                super::super::cancelled(stop)?;
                if let Some(value) = line.strip_prefix("#EXT-X-KEY:") {
                    if maps != 0 || media_count != 0 {
                        return Err(MISMATCH);
                    }
                    values.push(value);
                } else if let Some(value) = line.strip_prefix("#EXT-X-MAP:") {
                    if maps != 0 || media_count != 0 {
                        return Err(MISMATCH);
                    }
                    let attrs = crate::conversion::hls::attributes(value).map_err(|_| MISMATCH)?;
                    if attrs.len() != 1 {
                        return Err(MISMATCH);
                    }
                    let path = resource(
                        playlist.path.parent().ok_or(MISMATCH)?,
                        attrs.get("URI").ok_or(MISMATCH)?,
                    )?;
                    graph.assign(&path, graph::Role::Initialization)?;
                    self.init(&path, None, key, stop)?;
                    let init = read_bounded(&path)?;
                    let iv = crate::mp4::tenc_constant_iv_hex(&init).map_err(|_| MISMATCH)?;
                    let iv = iv.as_ref().map(crate::media::HexSecret::expose);
                    check_values(&sessions, &expected, self.target, false, iv)?;
                    check_values(&values, &expected, self.target, true, iv)?;
                    maps += 1;
                } else if !line.is_empty() && !line.starts_with('#') {
                    if maps != 1 {
                        return Err(MISMATCH);
                    }
                    let path = resource(playlist.path.parent().ok_or(MISMATCH)?, line)?;
                    graph.assign(&path, graph::Role::Media)?;
                    media::check(&path, media::Selection::Providers(self), stop)?;
                    media_count += 1;
                }
            }
            if maps != 1 || media_count == 0 {
                return Err(MISMATCH);
            }
        }
        graph.complete()?;
        unchanged(frozen)
    }
}
fn check_values(
    values: &[&str],
    expected: &BTreeMap<&str, Zeroizing<String>>,
    target: TargetProtection,
    complete: bool,
    iv: Option<&str>,
) -> Result<(), ConversionError> {
    let mut seen = std::collections::BTreeSet::new();
    for value in values {
        let attrs = crate::conversion::hls::attributes(value).map_err(|_| MISMATCH)?;
        let format = attrs.get("KEYFORMAT").ok_or(MISMATCH)?;
        if attrs.len() != 4 + usize::from(attrs.get("IV").is_some())
            || attrs.get("METHOD") != Some(method(target)?)
            || attrs.get("KEYFORMATVERSIONS") != Some("1")
            || attrs.get("URI") != expected.get(format).map(|value| value.as_str())
            || !seen.insert(format.to_owned())
        {
            return Err(MISMATCH);
        }
        validate_iv(&attrs, target, iv)?;
    }
    if complete && seen.len() != expected.len() {
        return Err(MISMATCH);
    }
    Ok(())
}
pub(super) fn iv_equivalent(a: &str, b: &str) -> bool {
    use crate::conversion::SegmentIv;
    // HLS hexadecimal integers may use either prefix case and leading zeroes.
    match (
        SegmentIv::from_hex(a.strip_prefix("0X").unwrap_or(a)),
        SegmentIv::from_hex(b.strip_prefix("0X").unwrap_or(b)),
    ) {
        (Ok(a), Ok(b)) => a.0 == b.0,
        _ => false,
    }
}
pub(super) fn validate_iv(
    attrs: &crate::m3u8::Attributes,
    target: TargetProtection,
    expected: Option<&str>,
) -> Result<(), ConversionError> {
    if let Some(actual) = attrs.get("IV")
        && (target != TargetProtection::Cbcs
            || expected.is_none_or(|expected| !iv_equivalent(actual, expected)))
    {
        return Err(MISMATCH);
    }
    Ok(())
}
impl OutputSignaling {
    pub(in crate::conversion) fn hls_key_tags(
        &self,
        key: &OutputKey,
        target: TargetProtection,
        uri: Option<&str>,
    ) -> Result<Zeroizing<String>, ConversionError> {
        self.validate_profile(OutputFormat::Hls, target)?;
        self.multi_objects(key, target)?
            .key_tags(self.systems().contains(&DrmSystem::FairPlay), uri)
    }
    pub(in crate::conversion) fn rewrite_hls(
        &self,
        root: &Path,
        key: &OutputKey,
        target: TargetProtection,
        uri: Option<&str>,
        stop: &Stop,
    ) -> Result<(), ConversionError> {
        let tags = self.hls_key_tags(key, target, uri)?;
        rewrite(root, &tags, stop)
    }
}
pub(in crate::conversion) fn rewrite_identity(
    root: &Path,
    target: TargetProtection,
    uri: &str,
    stop: &Stop,
) -> Result<(), ConversionError> {
    let tags = tag(target, "identity", uri)?;
    rewrite(root, &tags, stop)
}
fn rewrite(root: &Path, tags: &str, stop: &Stop) -> Result<(), ConversionError> {
    let entrypoint = root.join("master.m3u8");
    let playlists = crate::conversion::presentation::hls_media_paths(&entrypoint)?;
    for playlist in &playlists {
        super::super::cancelled(stop)?;
        let bytes = read_bounded(&playlist.path)?;
        let text = std::str::from_utf8(&bytes).map_err(|_| MISMATCH)?;
        let mut rewritten = Zeroizing::new(String::new());
        let mut maps = 0;
        for line in text.lines() {
            if line.starts_with("#EXT-X-KEY:") {
                continue;
            }
            if line.starts_with("#EXT-X-MAP:") {
                writeln!(rewritten, "{tags}").map_err(|_| MISMATCH)?;
                maps += 1;
            }
            writeln!(rewritten, "{line}").map_err(|_| MISMATCH)?;
        }
        if maps != 1 {
            return Err(MISMATCH);
        }
        replace(&playlist.path, &rewritten, stop)?;
    }
    let bytes = read_bounded(&entrypoint)?;
    let text = std::str::from_utf8(&bytes).map_err(|_| MISMATCH)?;
    let mut rewritten = Zeroizing::new(String::new());
    for line in text
        .lines()
        .filter(|line| !line.starts_with("#EXT-X-SESSION-KEY:"))
    {
        writeln!(rewritten, "{line}").map_err(|_| MISMATCH)?;
    }
    replace(&entrypoint, &rewritten, stop)
}

fn replace(path: &Path, text: &str, stop: &Stop) -> Result<(), ConversionError> {
    super::super::cancelled(stop)?;
    if text.len() as u64 > crate::inspection::MAX_MANIFEST_BYTES {
        return Err(MISMATCH);
    }
    let mut output = crate::native::create_media_or_truncate(path).map_err(|_| MISMATCH)?;
    output
        .write_all(text.as_bytes())
        .and_then(|()| output.sync_all())
        .map_err(|_| MISMATCH)
}

#[cfg(test)]
mod iv_tests {
    use super::*;
    #[test]
    fn iv_comparison_uses_bounded_integer_values_and_never_accepts_changed_bytes() {
        assert!(iv_equivalent(
            "0XABCDEF",
            "00000000000000000000000000abcdef"
        ));
        assert!(iv_equivalent("0x00", "0X0000"));
        for invalid in [
            "",
            "0x",
            "0X",
            "0xGG",
            "0x123456789012345678901234567890123",
        ] {
            assert!(!iv_equivalent(invalid, invalid));
        }
        assert!(!iv_equivalent("0x01", "0x00"));
    }
}
