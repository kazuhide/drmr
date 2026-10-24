//! Effective presentation controls are distinct from encoded sample identity.
use super::super::{
    ConversionError,
    native_media::{MediaHeader, Movie, TrackDescriptor},
    ts,
};
use super::samples::Ratio;
const BAD: ConversionError = ConversionError("native-presentation-metadata-invalid");
const IDENTITY: [i32; 9] = [0x10000, 0, 0, 0, 0x10000, 0, 0, 0, 0x4000_0000];

fn word(bytes: &[u8], at: usize) -> Result<u32, ConversionError> {
    Ok(u32::from_be_bytes(
        bytes
            .get(at..at + 4)
            .ok_or(BAD)?
            .try_into()
            .map_err(|_| BAD)?,
    ))
}
pub(super) fn aspect_ratio(track: &TrackDescriptor) -> Result<Ratio, ConversionError> {
    if let Some(pasp) = track
        .sample_entry_metadata
        .boxes
        .iter()
        .find(|value| value.kind == *b"pasp")
    {
        let numerator = word(&pasp.payload, 0)?;
        let denominator = word(&pasp.payload, 4)?;
        if numerator == 0 || denominator == 0 {
            return Err(BAD);
        }
        return Ratio::new(i128::from(numerator), u64::from(denominator));
    }
    if track.decoder_config_kind == *b"avcC" {
        let (config, _) = ts::avc::Configuration::from_avcc(&track.decoder_config)?;
        let (numerator, denominator) = config.metadata().sample_aspect_ratio.unwrap_or((1, 1));
        return Ratio::new(i128::from(numerator), u64::from(denominator));
    }
    Ratio::new(1, 1)
}

fn color_matches(bytes: &[u8], video: &ts::avc::Metadata) -> Result<bool, ConversionError> {
    let Some(kind) = bytes.get(..4) else {
        return Err(BAD);
    };
    if !matches!(kind, b"nclx" | b"nclc") {
        return Ok(false);
    }
    let component = |at| -> Result<u16, ConversionError> {
        Ok(u16::from_be_bytes(
            bytes
                .get(at..at + 2)
                .ok_or(BAD)?
                .try_into()
                .map_err(|_| BAD)?,
        ))
    };
    Ok(
        component(4)? == u16::from(video.colour_primaries.unwrap_or(2))
            && component(6)? == u16::from(video.transfer_characteristics.unwrap_or(2))
            && component(8)? == u16::from(video.matrix_coefficients.unwrap_or(2))
            && (kind != b"nclx"
                || ((bytes.get(10).ok_or(BAD)? & 0x80) != 0) == video.full_range.unwrap_or(false)),
    )
}

fn video_controls(
    track: &TrackDescriptor,
    video: &ts::avc::Metadata,
) -> Result<bool, ConversionError> {
    let entry = &track.sample_entry_metadata;
    let (numerator, denominator) = video.sample_aspect_ratio.unwrap_or((1, 1));
    let expected = Ratio::new(i128::from(numerator), u64::from(denominator))?;
    let width = u128::from(video.width) * u128::from(numerator) * 65_536 / u128::from(denominator);
    let width = u32::try_from(width).map_err(|_| BAD)?;
    let height = video.height.checked_mul(65_536).ok_or(BAD)?;
    if aspect_ratio(track)? != expected
        || entry.display_width != width
        || entry.display_height != height
        || entry.volume != 0
        || entry.alternate_group != 0
    {
        return Ok(false);
    }
    for extra in &entry.boxes {
        match &extra.kind {
            b"pasp" => {}
            b"colr" if color_matches(&extra.payload, video)? => {}
            b"fiel" if extra.payload == [1, 0] => {}
            _ => return Ok(false),
        }
    }
    Ok(true)
}
pub(super) fn transport_controls(
    movie: &Movie,
    track: &TrackDescriptor,
    native: &ts::NativeEvidence,
) -> Result<bool, ConversionError> {
    let entry = &track.sample_entry_metadata;
    let neutral_media_header = matches!(
        &track.media_header,
        MediaHeader::Audio { balance: 0 }
            | MediaHeader::Video {
                graphics_mode: 0,
                opcolor: [0, 0, 0]
            }
    );
    if movie.rate != 0x10000
        || movie.volume != 0x100
        || movie.matrix != IDENTITY
        || entry.display_matrix != IDENTITY
        || entry.track_flags & !4 != 3
        || entry.layer != 0
        || !neutral_media_header
    {
        return Ok(false);
    }
    match native.metadata()? {
        ts::canonical::TrackMetadata::Video(video) => video_controls(track, &video),
        ts::canonical::TrackMetadata::Audio { .. } => Ok(entry.volume == 0x100
            && matches!(entry.alternate_group, 0 | 1)
            && entry.display_width == 0
            && entry.display_height == 0
            && entry.boxes.is_empty()),
    }
}
