//! Bounded native MP4 sample evidence from the original file's declared media ranges.
//!
//! Payload callbacks are provisional until the complete scan returns successfully.
//! Each sample is visited in decode order for its track, in chunks of at most 64 KiB.
use super::ConversionError;
use super::InputKeys;
use crate::concurrency::Stop;
use crate::inspection::TrackKind;
use std::ops::Range;
use std::{
    io::{Read as _, Seek as _, SeekFrom},
    path::Path,
};
mod aac;
mod auxiliary;
pub(in crate::conversion) use aac::AacConfiguration;
mod controls;
pub(in crate::conversion) use controls::census as metadata_controls;
mod crypto;
mod encrypt;
mod entry;
mod fragment;
mod groups;
mod init;
mod progressive;
mod progressive_copy;
pub(in crate::conversion) use progressive_copy::copy_progressive;
mod read;
mod track_ids;
pub(in crate::conversion) use track_ids::canonicalize_tool_ids;
mod sample_window;
mod segment;
mod write;
pub(in crate::conversion) use segment::{
    SegmentEvidence, SegmentInput, SegmentReceipt, SegmentTrackEvidence, SegmentTransformer,
};
#[cfg(test)]
mod payload_window_tests;
#[cfg(test)]
mod segment_tests;
#[cfg(test)]
mod test_files;
#[cfg(test)]
mod tests;

pub(in crate::conversion) const CHUNK_BYTES: usize = 64 * 1024;
const MAX_METADATA_BYTES: usize = 16 * 1024 * 1024;
const MAX_PARSED_NODES: usize = MAX_METADATA_BYTES / std::mem::size_of::<crate::mp4::Mp4Box>();
const MAX_RETAINED_RANGES_BYTES: usize = 64 * 1024 * 1024;
const BAD: ConversionError = ConversionError("native-mp4-invalid");
const UNSUPPORTED: ConversionError = ConversionError("native-mp4-profile-not-supported");

/// A rate-one playback edit. A media time of -1 is an explicit empty edit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::conversion) struct Edit {
    pub duration: u64,
    pub media_time: i64,
    pub rate_integer: i16,
    pub rate_fraction: i16,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::conversion) struct SemanticBox {
    pub kind: [u8; 4],
    pub payload: Vec<u8>,
}

/// Container display/audio controls, separate from codec bitstream configuration.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::conversion) struct SampleEntryMetadata {
    pub width: Option<u16>,
    pub height: Option<u16>,
    pub channels: Option<u16>,
    pub sample_size: Option<u16>,
    pub sample_rate: Option<u32>,
    pub language: Option<String>,
    pub display_matrix: [i32; 9],
    pub display_width: u32,
    pub display_height: u32,
    pub track_flags: u32,
    pub layer: i16,
    pub alternate_group: i16,
    pub volume: i16,
    /// Known semantic boxes such as pasp, colr, clap, fiel and chan.
    pub boxes: Vec<SemanticBox>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::conversion) enum Scheme {
    Clear,
    Cenc,
    Cbcs,
}

/// Public protection declarations. No key or initialization-vector bytes are retained here.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::conversion) struct Protection {
    pub scheme: Scheme,
    pub key_id: Option<[u8; 16]>,
    pub per_sample_iv_size: u8,
    pub crypt_byte_block: u8,
    pub skip_byte_block: u8,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::conversion) enum MediaHeader {
    Audio {
        balance: i16,
    },
    Video {
        graphics_mode: u16,
        opcolor: [u16; 3],
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::conversion) struct TrackDescriptor {
    pub index: usize,
    pub track_id: u32,
    pub kind: TrackKind,
    pub timescale: u32,
    pub media_duration: u64,
    /// Raw tkhd duration in movie timescale units, separate from media/edit clocks.
    pub tkhd_duration: u64,
    pub sample_count: u64,
    pub codec: [u8; 4],
    pub decoder_config_kind: [u8; 4],
    /// AVC/HEVC/AV1 configuration payload, or AAC `AudioSpecificConfig` extracted from esds.
    pub decoder_config: Vec<u8>,
    /// Original configuration box, including its header and descriptor framing.
    pub decoder_config_box: Vec<u8>,
    pub sample_entry_metadata: SampleEntryMetadata,
    pub media_header: MediaHeader,
    pub edits: Vec<Edit>,
    pub protection: Protection,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::conversion) struct Movie {
    pub timescale: u32,
    pub duration: u64,
    pub rate: i32,
    pub volume: i16,
    pub matrix: [i32; 9],
    /// Optional mehd duration of the complete fragmented movie, in movie timescale units.
    /// Zero denotes an unknown duration; it does not replace the raw mvhd declaration.
    pub fragment_duration: Option<u64>,
    pub tracks: Vec<TrackDescriptor>,
    pub fragmented: bool,
}

/// Exact raw media clocks and original ciphertext range; presentation edits are separate.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::conversion) struct SampleInfo {
    pub ordinal: u64,
    pub dts: i64,
    pub pts: i64,
    pub duration: u32,
    pub size: u32,
    pub range: Range<u64>,
    pub flags: u32,
    pub is_sync: bool,
    pub roll_distance: Option<i16>,
    pub preroll_distance: Option<i16>,
    pub protection: SampleProtection,
    pub chunk_offset: u32,
    pub is_last_chunk: bool,
}

/// Actual sample protection after resolving its auxiliary entry; IV bytes remain private.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::conversion) enum SampleProtection {
    Clear,
    Encrypted {
        scheme: Scheme,
        key_id: [u8; 16],
        crypt_byte_block: u8,
        skip_byte_block: u8,
        encrypted_bytes: u32,
    },
}

fn retain_range(ranges: &mut Vec<Range<u64>>, range: Range<u64>) -> Result<(), ConversionError> {
    let limit = MAX_RETAINED_RANGES_BYTES / std::mem::size_of::<Range<u64>>();
    if ranges.len() == limit {
        return Err(ConversionError("resource-budget"));
    }
    if ranges.len() == ranges.capacity() {
        let capacity = ranges.capacity().max(1).saturating_mul(2).min(limit);
        ranges
            .try_reserve_exact(capacity - ranges.len())
            .map_err(|_| ConversionError("resource-budget"))?;
    }
    ranges.push(range);
    Ok(())
}
fn catalogue(
    reader: &mut read::Reader<'_>,
    layout: &read::Layout,
    init: &mut init::Initialization,
) -> Result<(), ConversionError> {
    let mut ranges = Vec::new();
    let mut counts = vec![0_u64; init.tracks.len()];
    let mut next = vec![None; init.tracks.len()];
    if init.movie.fragmented {
        for track in &init.tracks {
            progressive::empty(track, &layout.movie)?;
            if !track.auxiliary.is_empty() {
                let protection = track.encryption.as_ref().ok_or(BAD)?;
                auxiliary::Auxiliary::new(
                    &track.auxiliary,
                    &layout.movie,
                    protection,
                    0,
                    layout.movie_offset,
                    0,
                )?
                .finish()?;
            }
        }
        for range in &layout.moofs {
            let fragment = fragment::parse(reader, range, layout, init)?;
            for part in &fragment.tracks {
                if next[part.index].is_some_and(|previous| previous != part.first) {
                    return Err(ConversionError("fragment-timeline-gap-or-overlap"));
                }
                next[part.index] = Some(part.end);
                counts[part.index] = counts[part.index].checked_add(part.count).ok_or(BAD)?;
                for run in &part.runs {
                    retain_range(&mut ranges, run.range.clone())?;
                }
                let track = &init.tracks[part.index];
                if let Some(protection) = &track.encryption {
                    let mut auxiliary = auxiliary::Auxiliary::new(
                        &part.auxiliary,
                        &fragment.bytes,
                        protection,
                        usize::try_from(part.count).map_err(|_| BAD)?,
                        fragment.source_base,
                        part.auxiliary_base,
                    )?;
                    if auxiliary.constant() {
                        auxiliary.finish_constant()?;
                    } else {
                        let mut ordinal = 0;
                        for run in &part.runs {
                            run.walk(&fragment.bytes, reader.stop, &mut ordinal, &mut |info| {
                                auxiliary.next(info.size)?;
                                Ok(())
                            })?;
                        }
                        auxiliary.finish()?;
                    }
                } else if !part.auxiliary.is_empty() {
                    return Err(BAD);
                }
            }
        }
    } else {
        for track in &init.tracks {
            let table =
                progressive::Tables::parse(track, &layout.movie, &layout.mdats, reader.stop)?;
            counts[track.descriptor.index] = table.count as u64;
            table.chunks(reader.stop, |chunk| retain_range(&mut ranges, chunk.range))?;
            if let Some(protection) = &track.encryption {
                let mut auxiliary = auxiliary::Auxiliary::new(
                    &track.auxiliary,
                    &layout.movie,
                    protection,
                    table.count,
                    layout.movie_offset,
                    0,
                )?;
                if auxiliary.constant() {
                    auxiliary.finish_constant()?;
                } else {
                    table.walk(reader.stop, |info| {
                        auxiliary.next(info.size)?;
                        Ok(())
                    })?;
                    auxiliary.finish()?;
                }
            } else if !track.auxiliary.is_empty() {
                return Err(BAD);
            }
        }
    }
    read::disjoint(&mut ranges)?;
    for (index, count) in counts.into_iter().enumerate() {
        init.tracks[index].descriptor.sample_count = count;
        init.movie.tracks[index].sample_count = count;
    }
    Ok(())
}

/// Read real IDs from the bounded moov retained by `audio::movie`, without sample scanning.
/// This selects tool-only compatibility copies; source admission remains unchanged.
pub(super) fn metadata_track_ids(bytes: &[u8]) -> Result<Vec<u32>, ConversionError> {
    if bytes.len() > MAX_METADATA_BYTES {
        return Err(ConversionError("resource-budget"));
    }
    let nodes = crate::mp4::parse_boxes(bytes).map_err(|_| BAD)?;
    let movie = read::one(&nodes, *b"moov")?;
    movie
        .children
        .iter()
        .filter(|node| node.kind() == *b"trak")
        .map(|track| {
            init::track_header(read::one(&track.children, *b"tkhd")?.header.payload(bytes))
                .map(|(id, _)| id)
        })
        .collect()
}

/// Read bounded metadata and validate every sample's declared range and timeline.
/// This does not read or decrypt sample payloads and does not establish media verification.
pub(in crate::conversion) fn inspect(input: &Path, stop: &Stop) -> Result<Movie, ConversionError> {
    let mut reader = read::Reader::open(input, stop, false)?;
    let layout = reader.layout()?;
    let mut init = init::parse(&layout)?;
    catalogue(&mut reader, &layout, &mut init)?;
    reader.finish()?;
    Ok(init.movie)
}

fn protected_bytes(
    encryption: &crypto::SampleEncryption,
    track: &crypto::TrackEncryption,
) -> Result<u32, ConversionError> {
    let mut total = 0_u64;
    for span in &encryption.subsamples {
        let bytes = if track.scheme == Scheme::Cenc {
            u64::from(span.encrypted)
        } else {
            let crypt = u64::from(track.crypt.max(1));
            let skip = u64::from(track.skip);
            let blocks = u64::from(span.encrypted) / 16;
            ((blocks / (crypt + skip)) * crypt + (blocks % (crypt + skip)).min(crypt)) * 16
        };
        total = total.checked_add(bytes).ok_or(BAD)?;
    }
    u32::try_from(total).map_err(|_| BAD)
}
struct Payload<'a> {
    track: &'a init::Track,
    info: SampleInfo,
    encryption: Option<crypto::SampleEncryption>,
}
fn buffered_sample(sample: &SampleInfo, length: u64, spans: &[crypto::Subsample]) -> bool {
    sample.range.end <= length
        && sample.range.end.checked_sub(sample.range.start) == Some(u64::from(sample.size))
        && spans
            .iter()
            .flat_map(|span| [span.clear, span.encrypted])
            .filter(|size| *size != 0)
            .take(2)
            .count()
            == 2
}
fn payload(
    reader: &mut read::Reader<'_>,
    keys: &InputKeys,
    mut sample: Payload<'_>,
    visitor: &mut impl FnMut(&TrackDescriptor, &SampleInfo, &[u8]) -> Result<(), ConversionError>,
) -> Result<(), ConversionError> {
    read::check_stop(reader.stop)?;
    let mut cipher = match (&sample.track.encryption, &sample.encryption) {
        (Some(track), Some(encryption)) => {
            let key = keys
                .keys
                .iter()
                .find(|key| key.id == track.kid)
                .ok_or(ConversionError("input-key-required"))?;
            sample.info.protection = SampleProtection::Encrypted {
                scheme: track.scheme,
                key_id: track.kid,
                crypt_byte_block: track.crypt,
                skip_byte_block: track.skip,
                encrypted_bytes: protected_bytes(encryption, track)?,
            };
            Some(crypto::Cipher::new(
                &key.bytes,
                &encryption.iv,
                track.scheme,
                track.crypt,
                track.skip,
            )?)
        }
        (None, None) => None,
        _ => return Err(BAD),
    };
    reader
        .file
        .seek(SeekFrom::Start(sample.info.range.start))
        .map_err(|_| BAD)?;
    let clear = crypto::Subsample {
        clear: sample.info.size,
        encrypted: 0,
    };
    let subsamples = sample
        .encryption
        .as_ref()
        .map_or(std::slice::from_ref(&clear), |encryption| {
            encryption.subsamples.as_slice()
        });
    let mut input = read::SampleInput::new(
        &mut reader.file,
        &mut reader.sample_window,
        usize::try_from(sample.info.size).map_err(|_| BAD)?,
        sample.encryption.is_some() && buffered_sample(&sample.info, reader.length, subsamples),
    )?;
    if sample_window::eligible(&sample)
        && let Some(bytes) = input.complete()
    {
        sample_window::decrypt(bytes, subsamples, cipher.as_mut().ok_or(BAD)?, reader.stop)?;
        sample.info.chunk_offset = 0;
        sample.info.is_last_chunk = true;
        return visitor(&sample.track.descriptor, &sample.info, bytes);
    }
    let buffer = reader.scratch.chunk(
        usize::try_from(sample.info.size)
            .map_err(|_| BAD)?
            .min(CHUNK_BYTES),
    )?;
    let mut position = 0_u32;
    for span in subsamples {
        if let Some(cipher) = &mut cipher {
            cipher.begin_subsample();
        }
        for (encrypted, total) in [(false, span.clear), (true, span.encrypted)] {
            let mut remaining = total;
            while remaining != 0 {
                read::check_stop(reader.stop)?;
                let length = usize::try_from(remaining)
                    .map_err(|_| BAD)?
                    .min(CHUNK_BYTES);
                let bytes = &mut buffer.bytes[..length];
                input.read_exact(bytes).map_err(|_| BAD)?;
                if encrypted {
                    cipher
                        .as_mut()
                        .ok_or(BAD)?
                        .transform(bytes, remaining as usize == length)?;
                }
                let end = position
                    .checked_add(u32::try_from(length).map_err(|_| BAD)?)
                    .filter(|end| *end <= sample.info.size)
                    .ok_or(BAD)?;
                sample.info.chunk_offset = position;
                sample.info.is_last_chunk = end == sample.info.size;
                visitor(&sample.track.descriptor, &sample.info, bytes)?;
                position = end;
                remaining -= u32::try_from(length).map_err(|_| BAD)?;
            }
        }
    }
    if position != sample.info.size
        || sample.info.range.end - sample.info.range.start != u64::from(position)
    {
        return Err(BAD);
    }
    Ok(())
}

fn visit_fragments(
    reader: &mut read::Reader<'_>,
    layout: &read::Layout,
    init: &init::Initialization,
    track: &init::Track,
    keys: &InputKeys,
    metadata: &mut impl FnMut(&Movie, MetadataPart<'_>) -> Result<(), ConversionError>,
    visitor: &mut impl FnMut(&TrackDescriptor, &SampleInfo, &[u8]) -> Result<(), ConversionError>,
) -> Result<(), ConversionError> {
    let stop = reader.stop;
    let mut ordinal = 0_u64;
    for range in &layout.moofs {
        let fragment = fragment::parse(reader, range, layout, init)?;
        metadata(&init.movie, MetadataPart::Fragment(&fragment.bytes))?;
        for part in fragment
            .tracks
            .iter()
            .filter(|part| part.index == track.descriptor.index)
        {
            let first_ordinal = ordinal;
            let mut auxiliary = track
                .encryption
                .as_ref()
                .map(|protection| {
                    auxiliary::Auxiliary::new(
                        &part.auxiliary,
                        &fragment.bytes,
                        protection,
                        usize::try_from(part.count).map_err(|_| BAD)?,
                        fragment.source_base,
                        part.auxiliary_base,
                    )
                })
                .transpose()?;
            for run in &part.runs {
                run.walk(&fragment.bytes, stop, &mut ordinal, &mut |mut info| {
                    (info.roll_distance, info.preroll_distance) = part
                        .groups
                        .at(info.ordinal.checked_sub(first_ordinal).ok_or(BAD)?);
                    let encryption = auxiliary
                        .as_mut()
                        .map(|aux| aux.next(info.size))
                        .transpose()?;
                    payload(
                        reader,
                        keys,
                        Payload {
                            track,
                            info,
                            encryption,
                        },
                        visitor,
                    )
                })?;
            }
            if let Some(auxiliary) = auxiliary {
                auxiliary.finish()?;
            }
        }
    }
    if ordinal != track.descriptor.sample_count {
        return Err(BAD);
    }
    Ok(())
}
fn visit_progressive(
    reader: &mut read::Reader<'_>,
    layout: &read::Layout,
    track: &init::Track,
    keys: &InputKeys,
    visitor: &mut impl FnMut(&TrackDescriptor, &SampleInfo, &[u8]) -> Result<(), ConversionError>,
) -> Result<(), ConversionError> {
    let stop = reader.stop;
    let tables = progressive::Tables::parse(track, &layout.movie, &layout.mdats, stop)?;
    let mut auxiliary = track
        .encryption
        .as_ref()
        .map(|protection| {
            auxiliary::Auxiliary::new(
                &track.auxiliary,
                &layout.movie,
                protection,
                tables.count,
                layout.movie_offset,
                0,
            )
        })
        .transpose()?;
    tables.walk(stop, |info| {
        let encryption = auxiliary
            .as_mut()
            .map(|aux| aux.next(info.size))
            .transpose()?;
        payload(
            reader,
            keys,
            Payload {
                track,
                info,
                encryption,
            },
            visitor,
        )
    })?;
    if let Some(auxiliary) = auxiliary {
        auxiliary.finish()?;
    }
    Ok(())
}

/// Visit every original sample, decrypting only its declared protected regions.
/// No callback result may be published as complete evidence before this returns Ok.
pub(in crate::conversion) fn scan(
    input: &Path,
    keys: &InputKeys,
    stop: &Stop,
    visitor: &mut impl FnMut(&TrackDescriptor, &SampleInfo, &[u8]) -> Result<(), ConversionError>,
) -> Result<Movie, ConversionError> {
    scan_with_metadata(input, keys, stop, &mut |_, _| Ok(()), visitor)
}

/// Metadata comes from the same guarded bytes used to interpret and visit samples.
#[derive(Clone, Copy)]
pub(in crate::conversion) enum MetadataPart<'a> {
    Movie(&'a [u8]),
    Fragment(&'a [u8]),
}

pub(in crate::conversion) fn scan_with_metadata(
    input: &Path,
    keys: &InputKeys,
    stop: &Stop,
    metadata: &mut impl FnMut(&Movie, MetadataPart<'_>) -> Result<(), ConversionError>,
    visitor: &mut impl FnMut(&TrackDescriptor, &SampleInfo, &[u8]) -> Result<(), ConversionError>,
) -> Result<Movie, ConversionError> {
    let mut reader = read::Reader::open(input, stop, true)?;
    let layout = reader.layout()?;
    let mut init = init::parse(&layout)?;
    catalogue(&mut reader, &layout, &mut init)?;
    metadata(&init.movie, MetadataPart::Movie(&layout.movie))?;
    if init
        .movie
        .tracks
        .iter()
        .any(|track| track.sample_count == 0)
    {
        return Err(BAD);
    }
    for track in &init.tracks {
        if let Some(protection) = &track.encryption
            && !keys.keys.iter().any(|key| key.id == protection.kid)
        {
            return Err(ConversionError("input-key-required"));
        }
    }
    for track in &init.tracks {
        if init.movie.fragmented {
            visit_fragments(&mut reader, &layout, &init, track, keys, metadata, visitor)?;
        } else {
            visit_progressive(&mut reader, &layout, track, keys, visitor)?;
        }
    }
    reader.finish()?;
    Ok(init.movie)
}
