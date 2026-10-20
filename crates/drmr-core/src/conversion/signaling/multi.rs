//! Bounded provider metadata with no diagnostic representation of its values.
use super::{ConversionError, INVALID};
use crate::inspection::{InputFormat, OutputFormat, TargetProtection};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

/// DRM signaling systems, independently of the encryption scheme used by media samples.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DrmSystem {
    Widevine,
    PlayReady,
    FairPlay,
}

#[derive(Clone)]
pub(super) struct MultiDrm {
    pub widevine: Option<Zeroizing<Vec<u8>>>,
    pub fairplay: bool,
}
impl MultiDrm {
    pub fn new(
        pssh: Option<&str>,
        playready: bool,
        fairplay: bool,
    ) -> Result<Self, ConversionError> {
        if pssh.is_none() && !playready && !fairplay {
            return Err(INVALID);
        }
        let widevine = pssh.map(decode).transpose()?;
        Ok(Self { widevine, fairplay })
    }
    pub fn systems(&self, playready: bool) -> Vec<DrmSystem> {
        [
            self.widevine.is_some().then_some(DrmSystem::Widevine),
            playready.then_some(DrmSystem::PlayReady),
            self.fairplay.then_some(DrmSystem::FairPlay),
        ]
        .into_iter()
        .flatten()
        .collect()
    }
}
fn decode(value: &str) -> Result<Zeroizing<Vec<u8>>, ConversionError> {
    if value.is_empty() || value.len() > 131_072 || value.trim() != value {
        return Err(INVALID);
    }
    let bytes = Zeroizing::new(
        if value.len().is_multiple_of(2) && value.bytes().all(|v| v.is_ascii_hexdigit()) {
            hex::decode(value).map_err(|_| INVALID)?
        } else {
            STANDARD.decode(value).map_err(|_| INVALID)?
        },
    );
    validate_widevine(&bytes, None)?;
    Ok(bytes)
}
fn word(data: &[u8], at: usize) -> Result<usize, ConversionError> {
    Ok(u32::from_be_bytes(
        data.get(at..at + 4)
            .ok_or(INVALID)?
            .try_into()
            .map_err(|_| INVALID)?,
    ) as usize)
}
/// Validate a single complete box and every declared key identifier, including payload KIDs.
pub(super) fn validate_widevine(
    data: &[u8],
    expected: Option<&str>,
) -> Result<(), ConversionError> {
    if !(32..=65_536).contains(&data.len())
        || word(data, 0)? != data.len()
        || data.get(4..8) != Some(b"pssh")
        || !matches!(data[8], 0 | 1)
        || data[9..12] != [0, 0, 0]
        || data[12..28] != crate::playready::WIDEVINE_SYSTEM_ID
    {
        return Err(INVALID);
    }
    let mut at = 28;
    let mut kids = Vec::<&[u8]>::new();
    if data[8] == 1 {
        let count = word(data, at)?;
        at += 4;
        if count > 64 {
            return Err(INVALID);
        }
        for _ in 0..count {
            kids.push(data.get(at..at + 16).ok_or(INVALID)?);
            at += 16;
        }
    }
    let length = word(data, at)?;
    at += 4;
    if data.len() - at != length {
        return Err(INVALID);
    }
    let payload = &data[at..];
    let mut offset = 0;
    let mut scheme = false;
    while offset < payload.len() {
        let tag = varint(payload, &mut offset)?;
        if tag >> 3 == 0 {
            return Err(INVALID);
        }
        match tag & 7 {
            0 => {
                let value = varint(payload, &mut offset)?;
                if tag >> 3 == 9 {
                    if scheme || value != u64::from(u32::from_be_bytes(*b"cbcs")) {
                        return Err(INVALID);
                    }
                    scheme = true;
                }
            }
            1 | 5 => {
                offset = offset
                    .checked_add(if tag & 7 == 1 { 8 } else { 4 })
                    .ok_or(INVALID)?;
            }
            2 => {
                let size = usize::try_from(varint(payload, &mut offset)?).map_err(|_| INVALID)?;
                let end = offset
                    .checked_add(size)
                    .filter(|end| *end <= payload.len())
                    .ok_or(INVALID)?;
                if tag >> 3 == 2 {
                    kids.push(&payload[offset..end]);
                }
                offset = end;
            }
            _ => return Err(INVALID),
        }
        if matches!(tag >> 3, 2 | 9) && tag & 7 != if tag >> 3 == 2 { 2 } else { 0 } {
            return Err(INVALID);
        }
        if offset > payload.len() || kids.len() > 128 {
            return Err(INVALID);
        }
    }
    if kids.is_empty() || kids.iter().any(|kid| kid.len() != 16) {
        return Err(INVALID);
    }
    if let Some(expected) = expected {
        let expected = hex::decode(expected).map_err(|_| INVALID)?;
        if expected.len() != 16 || kids.iter().any(|kid| **kid != expected) {
            return Err(INVALID);
        }
    }
    Ok(())
}
fn varint(data: &[u8], at: &mut usize) -> Result<u64, ConversionError> {
    let mut value = 0u64;
    for shift in (0..70).step_by(7) {
        let byte = *data.get(*at).ok_or(INVALID)?;
        *at += 1;
        if shift == 63 && byte > 1 {
            return Err(INVALID);
        }
        value |= u64::from(byte & 127) << shift;
        if byte & 128 == 0 {
            return Ok(value);
        }
    }
    Err(INVALID)
}

pub(super) fn profile(
    systems: &[DrmSystem],
    format: OutputFormat,
    target: TargetProtection,
) -> Result<(), ConversionError> {
    if systems.is_empty() || systems.windows(2).any(|pair| pair[0] >= pair[1]) {
        return Err(super::UNSUPPORTED);
    }
    let supported = match (format, target) {
        (OutputFormat::Dash, TargetProtection::Cbcs) => !systems.contains(&DrmSystem::FairPlay),
        (OutputFormat::Hls, TargetProtection::Cbcs)
        | (OutputFormat::HlsTs, TargetProtection::HlsSampleAes) => systems == [DrmSystem::FairPlay],
        _ => false,
    };
    if !supported {
        return Err(super::UNSUPPORTED);
    }
    Ok(())
}

impl crate::conversion::Converter {
    /// Admit the source and public DRM selection without requiring output secrets.
    /// This does not assert license service behavior or device playback compatibility.
    pub fn validate_drm_source(
        &self,
        source: crate::conversion::VerificationSource<'_>,
        systems: &[DrmSystem],
        format: OutputFormat,
        target: TargetProtection,
    ) -> Result<(), ConversionError> {
        profile(systems, format, target)?;
        self.backend.check_cancelled()?;
        let frozen = crate::conversion::FrozenSource::capture(source.input, &self.backend.stop)?;
        let command = self.using_frozen_source(&frozen);
        command.validate_drm_source_inner(
            crate::conversion::VerificationSource {
                input: frozen.entrypoint(),
                ..source
            },
            systems,
            format,
            target,
        )?;
        command.check_frozen_source()
    }
    pub(in crate::conversion) fn validate_drm_source_inner(
        &self,
        source: crate::conversion::VerificationSource<'_>,
        systems: &[DrmSystem],
        format: OutputFormat,
        target: TargetProtection,
    ) -> Result<(), ConversionError> {
        profile(systems, format, target)?;
        self.backend.check_cancelled()?;
        let input = crate::inspection::inspect(source.input).map_err(|_| super::UNSUPPORTED)?;
        if input.format == InputFormat::Mp4 {
            super::clear_mp4(source.input, &self.backend.stop)?;
        } else if input.format != InputFormat::Hls {
            return Err(super::UNSUPPORTED);
        }
        let admitted = self.validate_source(
            source,
            crate::conversion::SourceValidationPurpose::Conversion,
        )?;
        if admitted.track_protection.is_empty()
            || admitted
                .track_protection
                .iter()
                .any(|value| *value != crate::conversion::SourceProtection::Clear)
            || (input.format == InputFormat::Hls && admitted.actual_format != OutputFormat::HlsTs)
            || !admitted.eligible_formats.contains(&format)
        {
            return Err(super::UNSUPPORTED);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn drm_profiles_are_explicit_and_do_not_accept_duplicate_or_unordered_systems() {
        use DrmSystem::{FairPlay, PlayReady, Widevine};
        for systems in [vec![Widevine], vec![PlayReady], vec![Widevine, PlayReady]] {
            assert!(profile(&systems, OutputFormat::Dash, TargetProtection::Cbcs).is_ok());
        }
        assert!(profile(&[FairPlay], OutputFormat::Hls, TargetProtection::Cbcs).is_ok());
        assert!(
            profile(
                &[FairPlay],
                OutputFormat::HlsTs,
                TargetProtection::HlsSampleAes
            )
            .is_ok()
        );
        for (systems, format, target) in [
            (vec![], OutputFormat::Dash, TargetProtection::Cbcs),
            (vec![FairPlay], OutputFormat::Dash, TargetProtection::Cbcs),
            (
                vec![Widevine, Widevine],
                OutputFormat::Dash,
                TargetProtection::Cbcs,
            ),
            (
                vec![PlayReady, Widevine],
                OutputFormat::Dash,
                TargetProtection::Cbcs,
            ),
            (vec![Widevine], OutputFormat::Hls, TargetProtection::Cbcs),
            (vec![FairPlay], OutputFormat::HlsTs, TargetProtection::Cbcs),
            (vec![PlayReady], OutputFormat::Dash, TargetProtection::Cenc),
        ] {
            assert!(profile(&systems, format, target).is_err());
        }
    }
    #[test]
    fn cancelled_source_admission_does_not_open_files_or_start_tools() {
        use crate::{
            conversion::{Converter, HlsInputKeys, InputKeys, VerificationSource},
            events::NoSink,
            toolrun::{Secrets, ToolRunner},
        };
        use std::sync::Arc;
        let stop = crate::concurrency::Stop::new();
        stop.set();
        let converter = Converter::new(
            Arc::new(ToolRunner::new(Arc::new(Secrets::new()), Arc::new(NoSink))),
            "must-not-start-packager",
            "must-not-start-ffmpeg",
        )
        .with_stop(stop);
        assert_eq!(
            converter.validate_drm_source(
                VerificationSource {
                    input: std::path::Path::new("missing-source"),
                    input_keys: &InputKeys::default(),
                    hls_keys: &HlsInputKeys::default()
                },
                &[DrmSystem::PlayReady],
                OutputFormat::Dash,
                TargetProtection::Cbcs
            ),
            Err(ConversionError("cancelled"))
        );
    }
}
