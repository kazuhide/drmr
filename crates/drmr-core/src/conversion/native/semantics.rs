//! Effective presentation controls are distinct from encoded sample identity.
use super::super::{
    ConversionError,
    native_media::{MediaHeader, Movie, TrackDescriptor},
    ts,
};
use super::samples::Ratio;
const BAD: ConversionError = ConversionError("native-presentation-metadata-invalid");
const IDENTITY: [i32; 9] = [0x10000, 0, 0, 0, 0x10000, 0, 0, 0, 0x4000_0000];

/// Apply the track matrix before the movie matrix, retaining every exact coefficient.
/// Columns 0/1 use signed 16.16, while column 2 uses signed 2.30.
pub(super) fn display_matrix(
    movie: &[i32; 9],
    track: &[i32; 9],
) -> Result<[i128; 9], ConversionError> {
    // A common 2^30 denominator makes the product's denominator exactly 2^60.
    // Never round to the destination matrix's coarser storage precision.
    let fixed =
        |matrix: &[i32; 9], at: usize| i128::from(matrix[at]) << if at % 3 == 2 { 0 } else { 14 };
    let mut result = [0_i128; 9];
    for row in 0..3 {
        for column in 0..3 {
            for inner in 0..3 {
                let product = fixed(track, row * 3 + inner)
                    .checked_mul(fixed(movie, inner * 3 + column))
                    .ok_or(BAD)?;
                result[row * 3 + column] =
                    result[row * 3 + column].checked_add(product).ok_or(BAD)?;
            }
        }
    }
    Ok(result)
}

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn composed_display_matrix_keeps_track_then_movie_order_and_signed_fractions() {
        let mut scale = IDENTITY;
        scale[0] = -0x18000;
        scale[4] = 0x8000;
        let mut translated = IDENTITY;
        translated[6] = 0x20000;
        translated[7] = -0x10000;
        let mut composed = scale;
        composed[6] = -0x30000;
        composed[7] = -0x8000;
        assert_eq!(
            display_matrix(&scale, &translated).unwrap(),
            display_matrix(&IDENTITY, &composed).unwrap()
        );
        assert_ne!(
            display_matrix(&translated, &scale).unwrap(),
            display_matrix(&IDENTITY, &composed).unwrap()
        );
    }

    #[test]
    fn composed_display_matrix_retains_bits_below_stored_precision_and_perspective() {
        let mut fractional = IDENTITY;
        fractional[0] = 0x10001;
        let mut composed = IDENTITY;
        composed[0] = 0x10002;
        assert_eq!(
            display_matrix(&fractional, &fractional).unwrap()[0],
            display_matrix(&IDENTITY, &composed).unwrap()[0] + (1_i128 << 28)
        );
        let identity = display_matrix(&IDENTITY, &IDENTITY).unwrap();
        for index in 0..9 {
            let mut changed = IDENTITY;
            changed[index] += 1;
            assert_ne!(display_matrix(&IDENTITY, &changed).unwrap(), identity);
            assert_ne!(display_matrix(&changed, &IDENTITY).unwrap(), identity);
        }
        display_matrix(&[i32::MIN; 9], &[i32::MAX; 9]).unwrap();
    }
}
