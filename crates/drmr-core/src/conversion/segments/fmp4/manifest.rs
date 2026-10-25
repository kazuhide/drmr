//! Native cipher rewriting and fresh manifests use the original resource map.
pub(super) mod dash_output;
mod initialization;
use super::{ConversionError, ConversionRequest, Converter, Presentation, proof};
use crate::{
    conversion::{self, SegmentIv, native, native_media::SegmentTransformer},
    inspection::{OutputFormat, TargetProtection, TrackKind},
};
use std::{
    io::Write as _,
    path::{Path, PathBuf},
};
use zeroize::Zeroizing;

pub(super) fn validate_request(
    converter: &Converter,
    request: &ConversionRequest<'_>,
) -> Result<(), ConversionError> {
    if !matches!(request.format, OutputFormat::Hls | OutputFormat::Dash) {
        return Err(ConversionError("processing-container-change-not-supported"));
    }
    if request.target == TargetProtection::HlsSampleAes {
        return Err(ConversionError("target-requires-ts-output"));
    }
    if request.target == TargetProtection::HlsAes128 && request.format != OutputFormat::Hls {
        return Err(ConversionError("target-requires-hls-output"));
    }
    if request.target == TargetProtection::Cenc && request.format == OutputFormat::Hls {
        return Err(ConversionError("cenc-hls-output-not-supported"));
    }
    if matches!(
        request.target,
        TargetProtection::Cenc | TargetProtection::Cbcs
    ) != request.output_key.is_some()
        || (request.target == TargetProtection::HlsAes128) != request.hls_output_key.is_some()
    {
        return Err(ConversionError("output-key-role-mismatch"));
    }
    if request
        .hls_output_key
        .and_then(conversion::HlsOutputKey::associated_iv)
        .is_some()
    {
        return Err(ConversionError("output-iv-not-used"));
    }
    if request.format == OutputFormat::Hls && request.target == TargetProtection::Cbcs {
        let uri = request
            .hls_key_uri
            .ok_or(ConversionError("hls-key-uri-required"))?;
        if uri.is_empty()
            || uri.len() > 4096
            || uri.contains('"')
            || uri.chars().any(char::is_control)
        {
            return Err(ConversionError("invalid-hls-key-uri"));
        }
    }
    if let Some(signaling) = &converter.output_signaling {
        signaling.validate_profile(request.format, request.target)?;
        signaling.validate_uri(request.hls_key_uri.or_else(|| {
            request
                .hls_output_key
                .map(conversion::HlsOutputKey::key_uri)
        }))?;
    }
    Ok(())
}

pub(super) struct OutputPart {
    pub initialization: PathBuf,
    pub resources: Vec<PathBuf>,
    pub codecs: Vec<String>,
    pub resolution: Option<(u64, u64)>,
    pub bandwidth: u32,
}

/// A complete resource fits in the two-second DASH buffer, or its shorter coded interval.
/// Tiny finite playback edits do not turn their retained coded frames into an infinite rate.
pub(in crate::conversion::segments::fmp4) fn bandwidth(
    paths: &[PathBuf],
    media: &native::SegmentedMedia,
) -> Result<u32, ConversionError> {
    if paths.len() != media.boundaries.len() {
        return Err(ConversionError("native-sample-evidence-incomplete"));
    }
    let mut peak = 1_u128;
    for (path, boundary) in paths.iter().zip(&media.boundaries) {
        let track = boundary
            .tracks
            .first()
            .ok_or(ConversionError("native-sample-evidence-incomplete"))?;
        let coded = u64::try_from(
            track
                .end_dts
                .checked_sub(track.first_dts)
                .ok_or(ConversionError("native-sample-evidence-invalid"))?,
        )
        .map_err(|_| ConversionError("native-sample-evidence-invalid"))?;
        let interval = coded.min(u64::from(track.timescale) * 2);
        if interval == 0 {
            return Err(ConversionError("native-sample-evidence-invalid"));
        }
        let bytes = conversion::files::regular_input(path)?
            .metadata()
            .map_err(|_| ConversionError("input-unreadable"))?
            .len();
        peak = peak.max(
            (u128::from(bytes) * 8 * u128::from(track.timescale)).div_ceil(u128::from(interval)),
        );
    }
    u32::try_from(peak).map_err(|_| ConversionError("dash-bandwidth-not-representable"))
}

fn codecs(media: &native::SegmentedMedia) -> Result<Vec<String>, ConversionError> {
    media
        .media
        .tracks
        .iter()
        .map(|track| {
            let descriptor = &track.descriptor;
            match &descriptor.codec {
                b"avc1" | b"avc3" => {
                    let config = &descriptor.decoder_config;
                    if config.len() < 4 || config[0] != 1 {
                        return Err(ConversionError("native-codec-profile-not-supported"));
                    }
                    Ok(format!(
                        "{}.{:02x}{:02x}{:02x}",
                        std::str::from_utf8(&descriptor.codec)
                            .map_err(|_| ConversionError("native-codec-profile-not-supported"))?,
                        config[1],
                        config[2],
                        config[3]
                    ))
                }
                b"mp4a" => Ok(format!(
                    "mp4a.40.{}",
                    conversion::native_media::AacConfiguration::parse(&descriptor.decoder_config)?
                        .object_type
                )),
                _ => Err(ConversionError("native-codec-profile-not-supported")),
            }
        })
        .collect()
}

fn key_tag(
    converter: &Converter,
    request: &ConversionRequest<'_>,
) -> Result<Option<String>, ConversionError> {
    if request.target != TargetProtection::Cbcs || request.format != OutputFormat::Hls {
        return Ok(None);
    }
    let uri = request
        .hls_key_uri
        .ok_or(ConversionError("hls-key-uri-required"))?;
    let fairplay = converter
        .output_signaling
        .as_ref()
        .is_some_and(|value| value.systems().contains(&conversion::DrmSystem::FairPlay));
    Ok(Some(if fairplay {
        format!(
            "#EXT-X-KEY:METHOD=SAMPLE-AES,URI=\"{uri}\",KEYFORMAT=\"com.apple.streamingkeydelivery\",KEYFORMATVERSIONS=\"1\""
        )
    } else {
        format!("#EXT-X-KEY:METHOD=SAMPLE-AES,URI=\"{uri}\",KEYFORMAT=\"identity\"")
    }))
}

fn aes_tag(key: &conversion::HlsOutputKey, iv: &SegmentIv) -> String {
    format!(
        "#EXT-X-KEY:METHOD=AES-128,URI=\"{}\",IV=0x{}",
        key.key_uri(),
        hex::encode(iv.0.as_ref())
    )
}

pub(super) fn transform(
    converter: &Converter,
    source: &Presentation,
    admitted: &[native::SegmentedMedia],
    request: &ConversionRequest<'_>,
    output: &Path,
    work: &Path,
) -> Result<(), ConversionError> {
    let _scope = converter
        .backend
        .scope(conversion::measurement::Phase::Packaging);
    let clear = work.join("clear");
    if request.target == TargetProtection::HlsAes128 {
        conversion::files::create_dir(&clear)?;
    }
    let package_root = if request.target == TargetProtection::HlsAes128 {
        clear.as_path()
    } else {
        output
    };
    let mut parts = Vec::with_capacity(source.parts.len());
    for (ordinal, part) in source.parts.iter().enumerate() {
        let directory = package_root.join(format!("track-{ordinal:04}"));
        parts.push(transform_part(
            converter,
            part,
            &admitted[ordinal],
            request,
            &directory,
        )?);
    }
    if request.format == OutputFormat::Dash {
        return dash_output::write(converter, output, source, &parts, request);
    }
    let hls_parts = source
        .parts
        .iter()
        .zip(&parts)
        .enumerate()
        .map(|(ordinal, (source, part))| {
            hls_part(
                converter,
                source,
                part,
                request,
                &output.join(format!("track-{ordinal:04}")),
            )
        })
        .collect::<Result<Vec<_>, ConversionError>>()?;
    super::super::manifest_hls::write(output, &hls_parts, &converter.backend.stop)
}

fn transform_part(
    converter: &Converter,
    part: &super::source::Part,
    admitted: &native::SegmentedMedia,
    request: &ConversionRequest<'_>,
    directory: &Path,
) -> Result<OutputPart, ConversionError> {
    converter.backend.check_cancelled()?;
    conversion::files::create_dir(directory)?;
    let initialization = directory.join("init.mp4");
    let transformer = {
        let _permit = proof::permit(converter)?;
        SegmentTransformer::new(
            &part.initialization,
            &initialization,
            if request.target == TargetProtection::HlsAes128 {
                TargetProtection::Clear
            } else {
                request.target
            },
            request.output_key,
            &converter.backend.stop,
        )?
    };
    let resources = converter.run_units(
        part.resources.len(),
        admitted.media.tracks.len(),
        resource_metadata(part, directory)?,
        |index| {
            let _permit = proof::permit(converter)?;
            let resource = &part.resources[index];
            let path = directory.join(format!("seg_{}.m4s", resource.dash.number));
            let evidence = transformer.rewrite_segment(
                &resource.path,
                &path,
                request.input_keys,
                &converter.backend.stop,
            )?;
            if admitted.boundaries.get(index) != Some(&evidence) {
                return Err(ConversionError("native-source-changed"));
            }
            Ok(path)
        },
    )?;
    drop(transformer);
    let (codecs, resolution) = {
        let _permit = proof::permit(converter)?;
        initialization::finish(converter, &initialization, request)?;
        let descriptor = &admitted.media.tracks[0].descriptor;
        let profile = native::mp4_profile(descriptor)?;
        let resolution = if descriptor.kind == TrackKind::Video {
            Some((
                profile["width"]
                    .as_u64()
                    .ok_or(ConversionError("presentation-metadata-mismatch"))?,
                profile["height"]
                    .as_u64()
                    .ok_or(ConversionError("presentation-metadata-mismatch"))?,
            ))
        } else {
            None
        };
        (codecs(admitted)?, resolution)
    };
    let bandwidth = bandwidth(&resources, admitted)?;
    Ok(OutputPart {
        initialization,
        resources,
        codecs,
        resolution,
        bandwidth,
    })
}

fn resource_metadata(part: &super::source::Part, directory: &Path) -> Result<u64, ConversionError> {
    let bad = ConversionError("resource-budget");
    let mut bytes = part
        .resources
        .capacity()
        .checked_mul(std::mem::size_of::<super::source::Resource>())
        .ok_or(bad)?;
    for resource in &part.resources {
        // Include retained input strings and the growing PathBuf returned for
        // each generated segment (u64 number plus fixed filename punctuation).
        bytes = bytes
            .checked_add(resource.path.capacity())
            .and_then(|bytes| bytes.checked_add(resource.duration.capacity()))
            .and_then(|bytes| {
                bytes.checked_add(
                    directory
                        .as_os_str()
                        .len()
                        .checked_add(32)?
                        .checked_mul(2)?,
                )
            })
            .ok_or(bad)?;
    }
    u64::try_from(bytes).map_err(|_| bad)
}

fn hls_part(
    converter: &Converter,
    source: &super::source::Part,
    part: &OutputPart,
    request: &ConversionRequest<'_>,
    directory: &Path,
) -> Result<super::super::manifest_hls::HlsPart, ConversionError> {
    let mut initialization = part.initialization.clone();
    let mut initial_key_tag = key_tag(converter, request)?;
    let mut resources = Vec::with_capacity(part.resources.len());
    if let Some(key) = request.hls_output_key {
        conversion::files::create_dir(directory)?;
        let iv = SegmentIv::random()?;
        initialization = directory.join("init.mp4");
        conversion::aes128::encrypt_segment_with_stop(
            &part.initialization,
            &initialization,
            &key.key,
            &iv,
            &converter.backend.stop,
        )?;
        initial_key_tag = Some(aes_tag(key, &iv));
    }
    for (resource, original) in part.resources.iter().zip(&source.resources) {
        let (path, key_tag) = if let Some(key) = request.hls_output_key {
            let iv = SegmentIv::random()?;
            let path = directory.join(
                resource
                    .file_name()
                    .ok_or(ConversionError("output-staging-failed"))?,
            );
            conversion::aes128::encrypt_segment_with_stop(
                resource,
                &path,
                &key.key,
                &iv,
                &converter.backend.stop,
            )?;
            (path, Some(aes_tag(key, &iv)))
        } else {
            (resource.clone(), None)
        };
        resources.push(super::super::manifest_hls::HlsResource {
            path,
            duration: original.duration.clone(),
            key_tag,
        });
    }
    Ok(super::super::manifest_hls::HlsPart {
        initialization: Some(initialization),
        resources,
        codecs: part.codecs.clone(),
        resolution: part.resolution,
        metadata: source.metadata.clone(),
        initial_key_tag,
    })
}

fn write(path: &Path, text: &str) -> Result<(), ConversionError> {
    if text.len() as u64 > crate::inspection::MAX_MANIFEST_BYTES {
        return Err(ConversionError("manifest-size-limit"));
    }
    let mut output = conversion::files::create_new(path)?;
    output
        .write_all(text.as_bytes())
        .and_then(|()| output.sync_all())
        .map_err(|_| ConversionError("output-staging-failed"))
}

fn xml(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn protection(
    converter: &Converter,
    request: &ConversionRequest<'_>,
) -> Result<Zeroizing<String>, ConversionError> {
    let Some(key) = request.output_key else {
        return Ok(Zeroizing::new(String::new()));
    };
    if let Some(signaling) = &converter.output_signaling {
        return Ok(signaling.segment_records(key)?.content_protection);
    }
    let kid = key.0.id_hex();
    Ok(Zeroizing::new(format!(
        "<ContentProtection schemeIdUri=\"urn:mpeg:dash:mp4protection:2011\" value=\"{}\" cenc:default_KID=\"{}-{}-{}-{}-{}\"/>",
        request.target.as_str(),
        &kid[..8],
        &kid[8..12],
        &kid[12..16],
        &kid[16..20],
        &kid[20..]
    )))
}
