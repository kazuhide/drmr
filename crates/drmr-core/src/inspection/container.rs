//! Read only MP4 metadata boxes; media payloads are skipped rather than buffered.

use super::{
    Evidence, Finding, InputFormat, Inspection, InspectionError, Pattern, Protection, Track,
    TrackKind,
};
use crate::mp4::{self, SampleEntry};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

const MAX_METADATA_BYTES: u64 = 16 * 1024 * 1024;
const MAX_TOP_LEVEL_BOXES: usize = 100_000;

fn metadata_bytes(path: &Path) -> Result<Vec<u8>, InspectionError> {
    let mut file = File::open(path).map_err(|_| InspectionError("input-unreadable"))?;
    let length = file
        .metadata()
        .map_err(|_| InspectionError("input-unreadable"))?
        .len();
    let mut position = 0_u64;
    let mut bytes = Vec::new();
    let mut moov_count = 0;
    let mut count = 0;
    while position < length {
        count += 1;
        if count > MAX_TOP_LEVEL_BOXES {
            return Err(InspectionError("mp4-box-count-limit"));
        }
        let mut fixed = [0_u8; 8];
        file.read_exact(&mut fixed)
            .map_err(|_| InspectionError("invalid-mp4-box"))?;
        let size32 = u32::from_be_bytes(fixed[..4].try_into().expect("four bytes"));
        let kind = &fixed[4..];
        let mut header = fixed.to_vec();
        let size = match size32 {
            0 => length - position,
            1 => {
                let mut large = [0_u8; 8];
                file.read_exact(&mut large)
                    .map_err(|_| InspectionError("invalid-mp4-box"))?;
                header.extend(large);
                u64::from_be_bytes(large)
            }
            size => u64::from(size),
        };
        if size < header.len() as u64 || size > length - position {
            return Err(InspectionError("invalid-mp4-box"));
        }
        if kind == b"moov" {
            moov_count += 1;
            if moov_count > 1 {
                return Err(InspectionError("multiple-mp4-movie-boxes"));
            }
            if size > MAX_METADATA_BYTES {
                return Err(InspectionError("mp4-metadata-size-limit"));
            }
            // A size-zero movie extends to EOF; give the standalone metadata its exact size.
            if size32 == 0 {
                header[..4]
                    .copy_from_slice(&u32::try_from(size).expect("bounded size").to_be_bytes());
            }
            bytes.extend(header);
            let remaining =
                usize::try_from(size - if size32 == 1 { 16 } else { 8 }).expect("bounded size");
            let start = bytes.len();
            bytes.resize(start + remaining, 0);
            file.read_exact(&mut bytes[start..])
                .map_err(|_| InspectionError("invalid-mp4-box"))?;
        }
        position += size;
        file.seek(SeekFrom::Start(position))
            .map_err(|_| InspectionError("input-unreadable"))?;
    }
    if moov_count == 0 {
        return Err(InspectionError("mp4-initialization-missing"));
    }
    Ok(bytes)
}

fn kind(format: [u8; 4]) -> TrackKind {
    match &format {
        b"avc1" | b"avc3" | b"hvc1" | b"hev1" | b"av01" | b"vp09" | b"encv" => TrackKind::Video,
        b"mp4a" | b"ac-3" | b"ec-3" | b"Opus" | b"fLaC" | b"enca" => TrackKind::Audio,
        b"wvtt" | b"stpp" => TrackKind::Text,
        _ => TrackKind::Unknown,
    }
}

fn protection(entry: &SampleEntry, has_sinf: bool) -> Protection {
    if !has_sinf && !matches!(&entry.format, b"encv" | b"enca") {
        return if kind(entry.format) == TrackKind::Unknown {
            Protection::Unknown
        } else {
            Protection::Clear
        };
    }
    let (Some(scheme), Some(tenc)) = (&entry.scheme, &entry.tenc) else {
        return Protection::Unknown;
    };
    if tenc.version > 1 || tenc.default_is_protected != 1 {
        return Protection::Unknown;
    }
    let valid_iv = match tenc.default_per_sample_iv_size {
        0 => tenc
            .default_constant_iv
            .as_ref()
            .is_some_and(|iv| matches!(iv.len(), 8 | 16)),
        8 | 16 => true,
        _ => false,
    };
    if !valid_iv {
        return Protection::Unknown;
    }
    match &scheme.scheme_type {
        b"cenc" => Protection::Cenc,
        b"cbcs" => Protection::Cbcs,
        b"cens" => Protection::Cens,
        b"cbc1" => Protection::Cbc1,
        _ => Protection::Unknown,
    }
}

pub(super) fn inspect_mp4(path: &Path) -> Result<Inspection, InspectionError> {
    let bytes = metadata_bytes(path)?;
    let boxes = mp4::parse_boxes(&bytes).map_err(|_| InspectionError("invalid-mp4-metadata"))?;
    let entries =
        mp4::sample_entries(&bytes).map_err(|_| InspectionError("invalid-mp4-metadata"))?;
    if entries.is_empty() {
        return Err(InspectionError("mp4-sample-descriptions-missing"));
    }
    let walked = mp4::walk(&boxes);
    let descriptions: Vec<_> = walked
        .iter()
        .filter(|item| item.kind() == *b"stsd")
        .flat_map(|item| item.children.iter())
        .collect();
    let mut findings = vec![Finding::new("media-samples-not-verified")];
    let mut tracks = Vec::new();
    for (index, (entry, raw)) in entries.iter().zip(descriptions).enumerate() {
        let protected = protection(
            entry,
            raw.children.iter().any(|child| child.kind() == *b"sinf"),
        );
        if protected == Protection::Unknown {
            findings.push(Finding::new("unrecognized-or-incomplete-sample-protection"));
        }
        let codec = entry.original_format.unwrap_or(entry.format);
        tracks.push(Track {
            id: format!("sample-description-{}", index + 1),
            kind: kind(codec),
            codec: Some(mp4::fourcc_text(&codec)),
            protection: protected,
            key_ids: entry
                .tenc
                .as_ref()
                .filter(|item| item.default_is_protected == 1)
                .map_or_else(Vec::new, |item| vec![hex::encode(item.default_kid)]),
            pattern: entry.tenc.as_ref().map(|item| Pattern {
                encrypted_blocks: item.default_crypt_byte_block,
                clear_blocks: item.default_skip_byte_block,
            }),
        });
    }
    let mut systems = Vec::new();
    for item in walked.iter().filter(|item| item.kind() == *b"pssh") {
        match mp4::Pssh::parse(item.header.payload(&bytes)) {
            Some(pssh) => systems.push(match pssh.system_id {
                crate::playready::WIDEVINE_SYSTEM_ID => "widevine".into(),
                crate::playready::PLAYREADY_SYSTEM_ID => "playready".into(),
                value => hex::encode(value),
            }),
            None => findings.push(Finding::new("invalid-pssh")),
        }
    }
    if walked
        .iter()
        .any(|item| matches!(&item.kind(), b"sgpd" | b"sbgp"))
    {
        findings.push(Finding::new("sample-groups-not-verified"));
    }
    Ok(Inspection::new(
        InputFormat::Mp4,
        Evidence::ContainerMetadata,
        tracks,
        systems,
        findings,
    )
    .with_details(super::PeriodCount::not_applicable()))
}
