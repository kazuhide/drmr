//! Explicit operator signaling. No license service is contacted by this module.
pub(in crate::conversion) mod flagfile;
mod multi;
mod records;
use super::{ConversionError, OutputKey};
use crate::{
    inspection::{InputFormat, OutputFormat, Protection, TargetProtection},
    mp4, playready,
};
pub use multi::DrmSystem;
use std::{
    fs::File,
    io::{Read, Seek, SeekFrom},
    path::Path,
};
use zeroize::{Zeroize, Zeroizing};

const INVALID: ConversionError = ConversionError("output-signaling-invalid");
pub(super) const UNSUPPORTED: ConversionError =
    ConversionError("output-signaling-profile-not-supported");
const MISMATCH: ConversionError = ConversionError("output-signaling-verification-failed");
const CLEAR_METADATA_BOXES: &[[u8; 4]] = &[
    *b"moov", *b"udta", *b"meta", *b"mvhd", *b"trak", *b"tkhd", *b"edts", *b"elst", *b"mdia",
    *b"mdhd", *b"hdlr", *b"minf", *b"vmhd", *b"smhd", *b"dinf", *b"dref", *b"url ", *b"stbl",
    *b"stsd", *b"stts", *b"ctts", *b"stss", *b"stsc", *b"stsz", *b"stco", *b"co64", *b"avc1",
    *b"avcC", *b"mp4a", *b"esds", *b"btrt", *b"pasp", *b"colr", *b"clap", *b"fiel", *b"mvex",
    *b"trex", *b"mehd", *b"moof", *b"mfhd", *b"traf", *b"tfhd", *b"tfdt", *b"trun", *b"mfra",
    *b"tfra", *b"mfro", *b"free",
];

const LIMIT: usize = 16 * 1024 * 1024;
const DASH: &str = "urn:mpeg:dash:schema:mpd:2011";
const CENC: &str = "urn:mpeg:cenc:2013";
const PR: &str = "urn:uuid:9a04f079-9840-4286-ab92-e65be0885f95";

/// One explicit `PlayReady` license address, kept out of diagnostics and reports.
#[derive(Clone)]
pub struct OutputSignaling {
    license_url: Zeroizing<String>,
    multi: Option<multi::MultiDrm>,
}
impl std::fmt::Debug for OutputSignaling {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OutputSignaling")
            .field("systems", &self.systems())
            .finish_non_exhaustive()
    }
}
impl OutputSignaling {
    /// Select `PlayReady` v4.3 metadata for CBCS DASH. The URL is metadata, never fetched.
    pub fn playready(license_url: &str) -> Result<Self, ConversionError> {
        if license_url.is_empty()
            || license_url.len() > 4096
            || license_url
                .chars()
                .any(|c| c.is_control() || c.is_whitespace())
        {
            return Err(INVALID);
        }
        let url = url::Url::parse(license_url).map_err(|_| INVALID)?;
        if !matches!(url.scheme(), "http" | "https")
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.fragment().is_some()
        {
            return Err(INVALID);
        }
        Ok(Self {
            license_url: Zeroizing::new(license_url.into()),
            multi: None,
        })
    }
    /// Select explicit DRM systems. Widevine data must be a complete hexadecimal or base64
    /// PSSH box; the `FairPlay` key URI belongs to the separate HLS output-key configuration.
    pub fn multi_drm(
        widevine_pssh: Option<&str>,
        playready_license_url: Option<&str>,
        fairplay: bool,
    ) -> Result<Self, ConversionError> {
        let license_url = playready_license_url
            .map(Self::playready)
            .transpose()?
            .map_or_else(
                || Zeroizing::new(String::new()),
                |value| value.license_url.clone(),
            );
        let multi = multi::MultiDrm::new(widevine_pssh, playready_license_url.is_some(), fairplay)?;
        Ok(Self {
            license_url,
            multi: Some(multi),
        })
    }
    /// Selected system identifiers are public; provider metadata remains private.
    pub fn systems(&self) -> Vec<DrmSystem> {
        self.multi.as_ref().map_or_else(
            || vec![DrmSystem::PlayReady],
            |value| value.systems(!self.license_url.is_empty()),
        )
    }
    pub(in crate::conversion) fn is_multi(&self) -> bool {
        self.multi.is_some()
    }
    pub(in crate::conversion) fn validate_uri(
        &self,
        uri: Option<&str>,
    ) -> Result<(), ConversionError> {
        if self.systems().contains(&DrmSystem::FairPlay)
            && !uri.is_some_and(records::valid_fairplay_uri)
        {
            return Err(INVALID);
        }
        Ok(())
    }
    pub(in crate::conversion) fn validate_profile(
        &self,
        format: OutputFormat,
        target: TargetProtection,
    ) -> Result<(), ConversionError> {
        if self.is_multi() {
            multi::profile(&self.systems(), format, target)
        } else {
            Self::profile(format, target)
        }
    }
    pub(super) fn profile(
        format: OutputFormat,
        target: TargetProtection,
    ) -> Result<(), ConversionError> {
        if format != OutputFormat::Dash || target != TargetProtection::Cbcs {
            return Err(UNSUPPORTED);
        }
        Ok(())
    }
    fn escaped_url(&self) -> Zeroizing<String> {
        let mut value = Zeroizing::new(String::with_capacity(self.license_url.len() * 5));
        for character in self.license_url.chars() {
            match character {
                '&' => value.push_str("&amp;"),
                '<' => value.push_str("&lt;"),
                '>' => value.push_str("&gt;"),
                other => value.push(other),
            }
        }
        value
    }
    pub(super) fn objects(&self, key: &OutputKey) -> Result<Objects, ConversionError> {
        if self.multi.is_some() {
            return Err(UNSUPPORTED);
        }
        let mut generated =
            playready::build_playready_v43(key.0.id_hex().as_str(), &self.license_url)
                .map_err(|_| INVALID)?;
        let xml = Zeroizing::new(std::mem::take(&mut generated.wrmheader_xml));
        let pro_hex = Zeroizing::new(hex::encode(&generated.pro_bytes));
        generated.pro_bytes.zeroize();
        Ok(Objects {
            url: self.license_url.clone(),
            pro: Zeroizing::new(generated.pro_b64),
            pssh: Zeroizing::new(generated.pssh_b64),
            hex: Zeroizing::new(generated.pssh_hex),
            bytes: Zeroizing::new(generated.pssh_box),
            xml,
            pro_hex,
            escaped_url: self.escaped_url(),
        })
    }
}

pub(super) struct Objects {
    url: Zeroizing<String>,
    pro: Zeroizing<String>,
    pssh: Zeroizing<String>,
    pub hex: Zeroizing<String>,
    bytes: Zeroizing<Vec<u8>>,
    xml: Zeroizing<String>,
    pro_hex: Zeroizing<String>,
    escaped_url: Zeroizing<String>,
}
impl Objects {
    pub fn secrets(&self) -> [&str; 7] {
        [
            &self.url,
            &self.pro,
            &self.pssh,
            &self.hex,
            &self.xml,
            &self.pro_hex,
            &self.escaped_url,
        ]
    }
    /// Require the actual init/manifest PSSH and PRO to equal the one generated object.
    /// Shaka derives the manifest records from the supplied PSSH; no MPD is rewritten.
    pub fn finish(
        &self,
        root: &Path,
        count: usize,
        key: &OutputKey,
    ) -> Result<(), ConversionError> {
        for index in 0..count {
            let data = read_bounded(&root.join(format!("track-{index:04}/init.mp4")))?;
            let boxes = mp4::parse_boxes(&data).map_err(|_| MISMATCH)?;
            let pssh: Vec<_> = mp4::walk(&boxes)
                .into_iter()
                .filter(|b| b.kind() == *b"pssh")
                .collect();
            if pssh.len() != 1 || pssh[0].header.bytes(&data) != *self.bytes {
                return Err(MISMATCH);
            }
        }
        let path = root.join("manifest.mpd");
        let data = read_bounded(&path)?;
        let text = std::str::from_utf8(&data).map_err(|_| MISMATCH)?;
        let document = roxmltree::Document::parse(text).map_err(|_| MISMATCH)?;
        let sets: Vec<_> = document
            .descendants()
            .filter(|n| n.has_tag_name((DASH, "AdaptationSet")))
            .collect();
        if sets.len() != count {
            return Err(MISMATCH);
        }
        let expected_kid = key.0.id_hex();
        for set in sets {
            let protection: Vec<_> = set
                .children()
                .filter(|n| n.has_tag_name((DASH, "ContentProtection")))
                .collect();
            if protection.len() != 2
                || set
                    .descendants()
                    .filter(|n| n.has_tag_name((DASH, "ContentProtection")))
                    .count()
                    != 2
            {
                return Err(MISMATCH);
            }
            let common = protection
                .iter()
                .find(|n| n.attribute("schemeIdUri") == Some("urn:mpeg:dash:mp4protection:2011"))
                .ok_or(MISMATCH)?;
            let kid = common
                .attribute((CENC, "default_KID"))
                .ok_or(MISMATCH)?
                .replace('-', "");
            if common.attribute("value") != Some("cbcs") || !kid.eq_ignore_ascii_case(&expected_kid)
            {
                return Err(MISMATCH);
            }
            let pr = protection
                .iter()
                .find(|n| n.attribute("schemeIdUri") == Some(PR))
                .ok_or(MISMATCH)?;
            let nodes: Vec<_> = pr.children().filter(roxmltree::Node::is_element).collect();
            let pssh = nodes
                .iter()
                .find(|n| n.has_tag_name((CENC, "pssh")))
                .ok_or(MISMATCH)?;
            let pro = nodes
                .iter()
                .find(|n| n.has_tag_name(("urn:microsoft:playready", "pro")))
                .ok_or(MISMATCH)?;
            if nodes.len() != 2
                || pssh.text() != Some(self.pssh.as_str())
                || pro.text() != Some(self.pro.as_str())
                || nodes.iter().any(|n| {
                    n.children().any(|c| c.is_element())
                        || n.children().filter(roxmltree::Node::is_text).count() != 1
                })
            {
                return Err(MISMATCH);
            }
        }
        Ok(())
    }
}

fn read_bounded(path: &Path) -> Result<Zeroizing<Vec<u8>>, ConversionError> {
    let mut file = File::open(path).map_err(|_| MISMATCH)?;
    let size =
        usize::try_from(file.metadata().map_err(|_| MISMATCH)?.len()).map_err(|_| MISMATCH)?;
    if size > LIMIT {
        return Err(MISMATCH);
    }
    // No growth/reallocation after metadata bytes enter the owned wiping buffer.
    let mut bytes = Zeroizing::new(vec![0; size]);
    file.read_exact(&mut bytes).map_err(|_| MISMATCH)?;
    let mut extra = Zeroizing::new([0; 1]);
    if file.read(&mut *extra).map_err(|_| MISMATCH)? != 0 {
        return Err(MISMATCH);
    }
    Ok(bytes)
}

/// A full top-level traversal plus bounded metadata trees; unknown opaque metadata refuses.
/// This is separate from inspection, which intentionally summarizes recognized protection.
pub(super) fn clear_mp4(
    path: &Path,
    stop: &crate::concurrency::Stop,
) -> Result<(), ConversionError> {
    cancelled(stop)?;
    let report = crate::inspection::inspect(path).map_err(|_| UNSUPPORTED)?;
    if report.format != InputFormat::Mp4
        || report.protection != Protection::Clear
        || report.tracks.is_empty()
        || report.tracks.len() > 2
        || report
            .tracks
            .iter()
            .filter(|t| t.kind == crate::inspection::TrackKind::Video)
            .count()
            != 1
        || report
            .tracks
            .iter()
            .any(|t| !matches!(t.codec.as_deref(), Some("avc1" | "mp4a")))
    {
        return Err(UNSUPPORTED);
    }
    let mut file = File::open(path).map_err(|_| UNSUPPORTED)?;
    let length = file.metadata().map_err(|_| UNSUPPORTED)?.len();
    let mut position = 0u64;
    let mut metadata_bytes = 0usize;
    let mut moov = 0;
    for _ in 0..100_000 {
        cancelled(stop)?;
        if position == length {
            return if moov == 1 { Ok(()) } else { Err(UNSUPPORTED) };
        }
        let mut header = [0u8; 8];
        file.read_exact(&mut header).map_err(|_| UNSUPPORTED)?;
        let mut bytes = Zeroizing::new(header.to_vec());
        let size = match u32::from_be_bytes(header[..4].try_into().unwrap()) {
            0 => length - position,
            1 => {
                let mut large = [0; 8];
                file.read_exact(&mut large).map_err(|_| UNSUPPORTED)?;
                bytes.extend(large);
                u64::from_be_bytes(large)
            }
            size => u64::from(size),
        };
        if size < bytes.len() as u64 || size > length - position {
            return Err(UNSUPPORTED);
        }
        let kind = &header[4..];
        if !matches!(
            kind,
            b"ftyp" | b"moov" | b"mdat" | b"free" | b"moof" | b"mfra" | b"sidx" | b"styp"
        ) {
            return Err(UNSUPPORTED);
        }
        if matches!(kind, b"moov" | b"moof" | b"mfra") {
            let size = usize::try_from(size).map_err(|_| UNSUPPORTED)?;
            metadata_bytes = metadata_bytes
                .checked_add(size)
                .filter(|n| *n <= LIMIT)
                .ok_or(UNSUPPORTED)?;
            let head = bytes.len();
            bytes.resize(size, 0);
            file.read_exact(&mut bytes[head..])
                .map_err(|_| UNSUPPORTED)?;
            clear_metadata_tree(&bytes, stop)?;
            if kind == b"moov" {
                moov += 1;
            }
        }
        position += size;
        file.seek(SeekFrom::Start(position))
            .map_err(|_| UNSUPPORTED)?;
    }
    Err(UNSUPPORTED)
}

fn cancelled(stop: &crate::concurrency::Stop) -> Result<(), ConversionError> {
    if stop.is_set() {
        return Err(ConversionError("cancelled"));
    }
    Ok(())
}

fn clear_metadata_tree(
    bytes: &[u8],
    stop: &crate::concurrency::Stop,
) -> Result<(), ConversionError> {
    let tree = mp4::parse_boxes(bytes).map_err(|_| UNSUPPORTED)?;
    for node in mp4::walk(&tree) {
        cancelled(stop)?;
        let kind = node.kind();
        let payload = node.header.payload(bytes);
        if kind == *b"ilst" && payload.is_empty() {
            continue;
        }
        if !CLEAR_METADATA_BOXES.contains(&kind) {
            return Err(UNSUPPORTED);
        }
        match &kind {
            b"dref" | b"stsd"
                if payload.get(..8) != Some(&[0, 0, 0, 0, 0, 0, 0, 1])
                    || node.children.len() != 1 =>
            {
                return Err(UNSUPPORTED);
            }
            b"url " if payload != [0, 0, 0, 1] => return Err(UNSUPPORTED),
            b"avc1"
                if payload.len() < 78
                    || node
                        .children
                        .iter()
                        .filter(|n| n.kind() == *b"avcC")
                        .count()
                        != 1 =>
            {
                return Err(UNSUPPORTED);
            }
            b"mp4a"
                if payload.len() < 28
                    || payload.get(8..10) != Some(&[0, 0])
                    || node
                        .children
                        .iter()
                        .filter(|n| n.kind() == *b"esds")
                        .count()
                        != 1 =>
            {
                return Err(UNSUPPORTED);
            }
            _ => {}
        }
    }
    Ok(())
}

impl super::Converter {
    /// Independently check the current manifest and initialization records against operator
    /// metadata. Media comparison and license playback are separate verification domains.
    pub fn verify_output_signaling(
        &self,
        input: &Path,
        format: OutputFormat,
        target: TargetProtection,
        keys: &super::key_file::OperatorKeys,
    ) -> Result<(), ConversionError> {
        self.backend.check_cancelled()?;
        let frozen = super::FrozenSource::capture(input, &self.backend.stop).map_err(|error| {
            if error.0 == "cancelled" {
                error
            } else {
                MISMATCH
            }
        })?;
        let input = frozen.entrypoint();
        let Some(signaling) = &keys.signaling else {
            return records::unrequested(&frozen, target, keys.output.as_ref());
        };
        signaling.validate_profile(format, target)?;
        if signaling.is_multi() {
            if format == OutputFormat::Dash {
                let key = keys.output.as_ref().ok_or(INVALID)?;
                signaling.multi_objects(key)?.finish(input, key)
            } else {
                records::fairplay(
                    input,
                    format,
                    target,
                    keys.hls_output_uri().ok_or(INVALID)?,
                    keys.output.as_ref(),
                    keys.hls_output.as_ref(),
                )
            }?;
            return frozen.check_original().map_err(|error| {
                if error.0 == "cancelled" {
                    error
                } else {
                    MISMATCH
                }
            });
        }
        let key = keys.output.as_ref().ok_or(INVALID)?;
        records::MultiObjects::legacy_playready(signaling.objects(key)?).finish(input, key)?;
        frozen.check_original().map_err(|error| {
            if error.0 == "cancelled" {
                error
            } else {
                MISMATCH
            }
        })
    }
    pub(super) fn validate_signaling_codec(
        &self,
        profile: &serde_json::Value,
        kind: crate::inspection::TrackKind,
    ) -> Result<(), ConversionError> {
        if self.output_signaling.is_some()
            && profile["codec_name"]
                != if kind == crate::inspection::TrackKind::Video {
                    "h264"
                } else {
                    "aac"
                }
        {
            return Err(UNSUPPORTED);
        }
        Ok(())
    }
    pub(super) fn validate_signaling_tracks(
        &self,
        request: &super::TrackConversionRequest<'_>,
        qualified_presentation: bool,
    ) -> Result<(), ConversionError> {
        if let Some(signaling) = &self.output_signaling {
            signaling.validate_profile(request.format, request.target)?;
            if signaling.is_multi() && qualified_presentation {
                return Ok(());
            }
            let first = request.tracks.first().ok_or(UNSUPPORTED)?;
            if request
                .tracks
                .iter()
                .enumerate()
                .any(|(index, track)| track.source != first.source || track.index != index)
            {
                return Err(UNSUPPORTED);
            }
            clear_mp4(&first.source, &self.backend.stop)?;
            let actual = crate::inspection::inspect(&first.source).map_err(|_| UNSUPPORTED)?;
            if actual.tracks.len() != request.tracks.len() {
                return Err(UNSUPPORTED);
            }
        }
        Ok(())
    }
    pub(super) fn validate_signaling_input(
        &self,
        request: &super::ConversionRequest<'_>,
    ) -> Result<(), ConversionError> {
        if let Some(signaling) = &self.output_signaling {
            signaling.validate_profile(request.format, request.target)?;
            if signaling.is_multi() {
                self.validate_drm_source_inner(
                    super::VerificationSource {
                        input: request.input,
                        input_keys: request.input_keys,
                        hls_keys: request.hls_input_keys,
                    },
                    &signaling.systems(),
                    request.format,
                    request.target,
                )?;
                if request.format != OutputFormat::Dash
                    && !records::valid_fairplay_uri(
                        request
                            .hls_key_uri
                            .or_else(|| request.hls_output_key.map(super::HlsOutputKey::key_uri))
                            .ok_or(INVALID)?,
                    )
                {
                    return Err(INVALID);
                }
            } else {
                clear_mp4(request.input, &self.backend.stop)?;
            }
        }
        Ok(())
    }
    pub(super) fn verify_signaled_output(
        &self,
        source: &Path,
        request: &super::TrackConversionRequest<'_>,
        stage: &Path,
    ) -> Result<(), ConversionError> {
        use super::{ContentKey, InputKeys, VerificationRequest, VerificationSource};
        if self
            .output_signaling
            .as_ref()
            .is_some_and(|value| !value.is_multi())
        {
            // The published MPD is an independent input, including its declared window.
            // A per-track concatenation alone cannot prove the final presentation graph.
            let key = request
                .output_key
                .ok_or(ConversionError("output-key-required"))?;
            let keys = InputKeys::new(vec![ContentKey::from_hex(
                &key.0.id_hex(),
                &key.0.key_hex(),
            )?])?;
            let hls_keys = super::HlsInputKeys::default();
            let final_check = self.verify(VerificationRequest {
                source: VerificationSource {
                    input: source,
                    input_keys: request.input_keys,
                    hls_keys: &hls_keys,
                },
                output: VerificationSource {
                    input: &stage.join("manifest.mpd"),
                    input_keys: &keys,
                    hls_keys: &hls_keys,
                },
            })?;
            if !final_check.passed {
                return Err(ConversionError("output-signaling-verification-failed"));
            }
        }
        Ok(())
    }
    pub(in crate::conversion) fn finish_multi_signaling(
        &self,
        root: &Path,
        format: OutputFormat,
        target: TargetProtection,
        key: Option<&OutputKey>,
        uri: Option<&str>,
        hls: Option<&super::HlsOutputKey>,
    ) -> Result<(), ConversionError> {
        let Some(signaling) = self
            .output_signaling
            .as_ref()
            .filter(|value| value.is_multi())
        else {
            return Ok(());
        };
        if format == OutputFormat::Dash {
            let key = key.ok_or(INVALID)?;
            signaling
                .multi_objects(key)?
                .finish(&root.join("manifest.mpd"), key)
        } else {
            records::fairplay(
                &root.join("master.m3u8"),
                format,
                target,
                uri.or_else(|| hls.map(super::HlsOutputKey::key_uri))
                    .ok_or(INVALID)?,
                key,
                hls,
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const URL: &str = "https://example.invalid/license?tenant=synthetic&a=b";
    fn key() -> OutputKey {
        OutputKey::from_hex(
            "000102030405060708090a0b0c0d0e0f",
            "101112131415161718191a1b1c1d1e1f",
        )
        .unwrap()
    }
    #[test]
    fn cancelled_admission_never_opens_the_source() {
        let stop = crate::concurrency::Stop::new();
        stop.set();
        assert_eq!(
            clear_mp4(Path::new("nonexistent-source"), &stop),
            Err(ConversionError("cancelled"))
        );
    }

    #[test]
    fn metadata_census_refuses_hidden_or_unknown_protection_and_external_references() {
        fn boxed(kind: [u8; 4], payload: &[u8]) -> Vec<u8> {
            [
                (8 + u32::try_from(payload.len()).unwrap())
                    .to_be_bytes()
                    .as_slice(),
                kind.as_slice(),
                payload,
            ]
            .concat()
        }
        let object = OutputSignaling::playready(URL)
            .unwrap()
            .objects(&key())
            .unwrap();
        for bytes in [
            object.bytes.to_vec(),
            boxed(*b"moov", &object.bytes),
            boxed(*b"moov", &boxed(*b"trak", &object.bytes)),
            boxed(*b"moov", &boxed(*b"uuid", &[0; 16])),
            boxed(*b"moov", &boxed(*b"senc", &[0; 8])),
            boxed(*b"moov", &boxed(*b"sgpd", &[0; 12])),
            boxed(*b"url ", b"\0\0\0\0https://example.invalid/data"),
            boxed(*b"avc1", &[0; 10]),
            boxed(*b"stsd", &[0; 8]),
        ] {
            assert_eq!(
                clear_metadata_tree(&bytes, &crate::concurrency::Stop::new()),
                Err(UNSUPPORTED)
            );
        }
    }
    #[test]
    fn explicit_addresses_are_bounded_absolute_metadata_and_never_debug_values() {
        for url in [
            "",
            "/relative",
            "https://",
            "file:///license",
            "https://user:pass@example.invalid/license",
            "https://example.invalid/#fragment",
            " https://example.invalid/license",
            "https://example.invalid/a b",
            "https://example.invalid/\nlicense",
        ] {
            assert!(matches!(OutputSignaling::playready(url), Err(INVALID)));
        }
        assert!(
            OutputSignaling::playready(&format!("https://example.invalid/{}", "x".repeat(4096)))
                .is_err()
        );
        let value = OutputSignaling::playready(URL).unwrap();
        assert!(!format!("{value:?}").contains(URL));
        let objects = value.objects(&key()).unwrap();
        let parsed = playready::parse_pro(&objects.pro).unwrap();
        assert_eq!(parsed.la_url.as_deref(), Some(URL));
        let secrets = crate::toolrun::Secrets::with(objects.secrets());
        for secret in objects.secrets() {
            assert!(!secrets.scrub(secret).contains(secret));
        }
    }
    #[test]
    fn specialized_converters_cannot_silently_ignore_attached_signaling() {
        use super::super::{Aes128Key, Converter, HlsInputKeys, HlsOutputKey, InputKeys};
        use crate::{
            events::NoSink,
            toolrun::{Secrets, ToolRunner},
        };
        use std::sync::Arc;
        let converter = Converter::new(
            Arc::new(ToolRunner::new(Arc::new(Secrets::new()), Arc::new(NoSink))),
            "must-not-start-packager",
            "must-not-start-ffmpeg",
        )
        .with_output_signaling(OutputSignaling::playready(URL).unwrap());
        let input = Path::new("nonexistent-source");
        let output = Path::new("nonexistent-result");
        let keys = InputKeys::default();
        let hls = HlsInputKeys::default();
        let key = key();
        let segment = HlsOutputKey::new(
            "https://example.invalid/key",
            Aes128Key::from_hex("202122232425262728292a2b2c2d2e2f").unwrap(),
        )
        .unwrap();
        let results = [
            converter.convert_mp4(input, output, TargetProtection::Cbcs, &keys, Some(&key)),
            converter.convert_from_hls(
                input,
                output,
                TargetProtection::Cbcs,
                &keys,
                &hls,
                Some(&key),
            ),
            converter.convert_hls_aes128(input, output, &keys, &segment),
            converter.convert_hls_to_hls_aes128(input, output, &keys, &hls, &segment),
        ];
        for result in results {
            assert!(matches!(result, Err(UNSUPPORTED)));
        }
    }

    #[test]
    fn every_init_and_both_manifest_records_must_match_the_generated_object() {
        let owner = crate::native::private_tempdir().unwrap();
        let root = owner.path().join("media");
        crate::native::create_media_dir_all(&root.join("track-0000")).unwrap();
        let key = key();
        let objects = OutputSignaling::playready(URL)
            .unwrap()
            .objects(&key)
            .unwrap();
        let init = root.join("track-0000/init.mp4");
        std::fs::write(&init, &*objects.bytes).unwrap();
        let common = "<ContentProtection schemeIdUri=\"urn:mpeg:dash:mp4protection:2011\" value=\"cbcs\" cenc:default_KID=\"00010203-0405-0607-0809-0a0b0c0d0e0f\"/>";
        let manifest = format!(
            "<MPD xmlns=\"{DASH}\" xmlns:cenc=\"{CENC}\" xmlns:mspr=\"urn:microsoft:playready\"><Period><AdaptationSet>{common}<ContentProtection schemeIdUri=\"{PR}\"><cenc:pssh>{}</cenc:pssh><mspr:pro>{}</mspr:pro></ContentProtection></AdaptationSet></Period></MPD>",
            objects.pssh.as_str(),
            objects.pro.as_str()
        );
        let mpd = root.join("manifest.mpd");
        std::fs::write(&mpd, &manifest).unwrap();
        objects.finish(&root, 1, &key).unwrap();
        for changed in [
            manifest.replace(objects.pro.as_str(), "AAAA"),
            manifest.replace(objects.pssh.as_str(), "AAAA"),
            manifest.replace("value=\"cbcs\"", "value=\"cenc\""),
            manifest.replace("00010203-0405", "ffffffff-0405"),
        ] {
            std::fs::write(&mpd, changed).unwrap();
            assert_eq!(objects.finish(&root, 1, &key), Err(MISMATCH));
        }
        std::fs::write(&mpd, manifest).unwrap();
        let mut changed = objects.bytes.to_vec();
        changed.extend_from_slice(&objects.bytes);
        std::fs::write(init, changed).unwrap();
        assert_eq!(objects.finish(&root, 1, &key), Err(MISMATCH));
    }
}
