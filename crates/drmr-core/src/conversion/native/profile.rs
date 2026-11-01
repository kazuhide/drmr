//! Codec/container metadata is read from declared configuration, never guessed.
use super::super::{ConversionError, native_media::TrackDescriptor, ts};
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use std::{io::Read as _, path::Path};

fn base(codec: &str, kind: &str, count: u64, scale: u32) -> Value {
    json!({"codec_name":codec,"codec_type":kind,"nb_read_packets":count.to_string(),
        "time_base":format!("1/{scale}"),"sample_rate":null,"channels":null,
        "channel_layout":null,"width":null,"height":null,"pix_fmt":null,
        "sample_aspect_ratio":null,"color_range":null,"color_space":null,
        "color_transfer":null,"color_primaries":null,"chroma_location":null,
        "field_order":null,"bits_per_raw_sample":null,"extradata_hash":null,
        "display_matrices":[],"language":"und"})
}
fn avc(value: &mut Value, metadata: &ts::avc::Metadata) {
    value["width"] = metadata.width.into();
    value["height"] = metadata.height.into();
    let chroma = match metadata.chroma_format_idc {
        0 => "gray",
        1 => "yuv420p",
        2 => "yuv422p",
        _ => "yuv444p",
    };
    let pix = if metadata.bit_depth_luma == 8 {
        chroma.into()
    } else {
        format!("{chroma}{}le", metadata.bit_depth_luma)
    };
    value["pix_fmt"] = Value::String(pix);
    value["bits_per_raw_sample"] = metadata.bit_depth_luma.to_string().into();
    value["field_order"] = if metadata.frame_mbs_only {
        "progressive".into()
    } else {
        Value::Null
    };
    if let Some((x, y)) = metadata.sample_aspect_ratio {
        value["sample_aspect_ratio"] = format!("{x}:{y}").into();
    }
    // Numeric identifiers preserve every explicitly signaled value, including
    // identifiers that an external decoder might label as unknown.
    value["color_range"] = metadata
        .full_range
        .map(|full| if full { "pc" } else { "tv" })
        .into();
    value["color_primaries"] = metadata.colour_primaries.into();
    value["color_transfer"] = metadata.transfer_characteristics.into();
    value["color_space"] = metadata.matrix_coefficients.into();
    value["chroma_location"] = metadata
        .chroma_location
        .map(|(top, bottom)| json!([top, bottom]))
        .unwrap_or_default();
}

fn hevc(value: &mut Value, metadata: &super::super::hevc::Metadata) {
    value["width"] = metadata.width.into();
    value["height"] = metadata.height.into();
    value["pix_fmt"] = match metadata.bit_depth_luma {
        8 => "yuv420p".into(),
        depth => format!("yuv420p{depth}le").into(),
    };
    value["bits_per_raw_sample"] = metadata.bit_depth_luma.to_string().into();
    value["field_order"] = if metadata.progressive {
        "progressive".into()
    } else {
        Value::Null
    };
    if let Some((x, y)) = metadata.sample_aspect_ratio {
        value["sample_aspect_ratio"] = format!("{x}:{y}").into();
    }
    value["color_range"] = metadata
        .full_range
        .map(|full| if full { "pc" } else { "tv" })
        .into();
    value["color_primaries"] = metadata.colour_primaries.into();
    value["color_transfer"] = metadata.transfer_characteristics.into();
    value["color_space"] = metadata.matrix_coefficients.into();
    value["chroma_location"] = metadata
        .chroma_location
        .map(|(top, bottom)| json!([top, bottom]))
        .unwrap_or_default();
}
fn vp9(value: &mut Value, metadata: &super::super::vp9::Metadata) {
    let chroma = match (metadata.matrix_coefficients, metadata.chroma_subsampling) {
        (0, _) => "gbrp",
        (_, 0 | 1) => "yuv420p",
        (_, 2) => "yuv422p",
        _ => "yuv444p",
    };
    value["pix_fmt"] = if metadata.bit_depth == 8 {
        chroma.into()
    } else {
        format!("{chroma}{}le", metadata.bit_depth).into()
    };
    value["bits_per_raw_sample"] = metadata.bit_depth.to_string().into();
    value["field_order"] = "progressive".into();
    value["color_range"] = if metadata.full_range { "pc" } else { "tv" }.into();
    value["color_primaries"] = metadata.colour_primaries.into();
    value["color_transfer"] = metadata.transfer_characteristics.into();
    value["color_space"] = metadata.matrix_coefficients.into();
    value["chroma_subsampling"] = metadata.chroma_subsampling.into();
}
pub(in crate::conversion) fn transport_profile(
    native: &ts::NativeEvidence,
) -> Result<Value, ConversionError> {
    native.validate()?;
    let timing = native.timing()?;
    native.evidence()?;
    let mut value = match native.metadata()? {
        ts::canonical::TrackMetadata::Video(metadata) => {
            let mut value = base("h264", "video", timing.count, 90_000);
            avc(&mut value, &metadata);
            value
        }
        ts::canonical::TrackMetadata::Audio {
            sample_rate,
            channels,
            ..
        } => {
            let mut value = base("aac", "audio", timing.count, 90_000);
            value["sample_rate"] = sample_rate.to_string().into();
            value["channels"] = channels.into();
            value
        }
    };
    value["extradata_hash"] = match &native.config {
        ts::CanonicalConfig::Video { sps, pps } => {
            let mut hash = Sha256::new();
            hash.update(sps);
            hash.update(pps);
            format!("SHA256:{:x}", hash.finalize()).into()
        }
        ts::CanonicalConfig::Audio { .. } => Value::Null,
    };
    Ok(value)
}
pub(in crate::conversion) fn mp4_profile(
    track: &TrackDescriptor,
) -> Result<Value, ConversionError> {
    let codec = match &track.codec {
        b"avc1" | b"avc3" => "h264",
        b"hvc1" | b"hev1" => "hevc",
        b"av01" => "av1",
        b"vp09" => "vp9",
        b"mp4a" => "aac",
        b"ac-3" => "ac3",
        b"ec-3" => "eac3",
        b"Opus" => "opus",
        b"fLaC" => "flac",
        _ => return Err(ConversionError("native-codec-configuration-not-supported")),
    };
    let kind = match track.kind {
        crate::inspection::TrackKind::Audio => "audio",
        crate::inspection::TrackKind::Video => "video",
        _ => return Err(ConversionError("track-role-mismatch")),
    };
    let mut value = base(codec, kind, track.sample_count, track.timescale);
    let entry = &track.sample_entry_metadata;
    value["width"] = entry.width.into();
    value["height"] = entry.height.into();
    value["channels"] = entry.channels.into();
    value["sample_rate"] = entry.sample_rate.map(|rate| rate.to_string()).into();
    value["language"] = entry.language.as_deref().unwrap_or("und").into();
    value["extradata_hash"] = format!("SHA256:{:x}", Sha256::digest(&track.decoder_config)).into();
    if track.decoder_config_kind == *b"avcC" {
        let (config, width) = ts::avc::Configuration::from_avcc(&track.decoder_config)?;
        avc(&mut value, config.metadata());
        value["nal_length_size"] = width.to_string().into();
    } else if track.decoder_config_kind == *b"hvcC" {
        let (config, width) = super::super::hevc::Configuration::from_hvcc(&track.decoder_config)?;
        config.validate_entry(track.codec)?;
        hevc(&mut value, config.metadata());
        value["nal_length_size"] = width.to_string().into();
        value["extradata_hash"] =
            format!("SHA256:{}", hex::encode(config.semantic_identity())).into();
    } else if track.decoder_config_kind == *b"vpcC" {
        if track.protection.scheme == super::super::native_media::Scheme::Cbcs {
            return Err(super::super::vp9::PROTECTION_NOT_SUPPORTED);
        }
        let config = super::super::vp9::Configuration::from_vpcc(&track.decoder_config)?;
        vp9(&mut value, config.metadata());
        value["extradata_hash"] =
            format!("SHA256:{}", hex::encode(config.semantic_identity())).into();
    } else if track.decoder_config_kind == *b"esds" {
        let config = super::super::native_media::AacConfiguration::parse(&track.decoder_config)?;
        value["sample_rate"] = config.sample_rate.to_string().into();
        value["channels"] = config.channels.into();
    }
    Ok(value)
}

impl super::super::backend::Backend {
    pub(in crate::conversion) fn native_profile(
        &self,
        path: &Path,
        index: usize,
    ) -> Result<Value, ConversionError> {
        let _permit = self
            .runner
            .native_permit(
                crate::resources::Claim {
                    cpu_threads: 1,
                    memory_bytes: 128 * 1024 * 1024,
                },
                &self.stop,
                self.timeout,
            )
            .map_err(|error| match error {
                crate::resources::ReservationError::Cancelled => ConversionError("cancelled"),
                crate::resources::ReservationError::Timeout => {
                    ConversionError("media-tool-timeout")
                }
                crate::resources::ReservationError::Insufficient => {
                    ConversionError("resource-budget")
                }
            })?;
        let mut input = super::super::files::regular_input(path)?;
        let mut first = [0_u8; 1];
        input
            .read_exact(&mut first)
            .map_err(|_| ConversionError("input-unreadable"))?;
        let value = if first[0] == 0x47 {
            let mut scanner = ts::NativeScanner::new();
            scanner.segment(path, &self.stop)?;
            let (admission, native) = scanner.finish()?;
            let id = admission
                .tracks
                .get(index)
                .ok_or(ConversionError("stream-selector-mismatch"))?
                .pid;
            transport_profile(
                native
                    .get(&id)
                    .ok_or(ConversionError("ts-original-evidence-required"))?,
            )?
        } else {
            let movie = super::super::native_media::inspect(path, &self.stop)?;
            mp4_profile(
                movie
                    .tracks
                    .get(index)
                    .ok_or(ConversionError("stream-selector-mismatch"))?,
            )?
        };
        self.runner
            .register_workload(super::super::execution::observe_profile(&value, path)?);
        Ok(value)
    }
}
