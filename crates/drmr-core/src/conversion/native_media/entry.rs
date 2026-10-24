//! Strict sample-entry configuration, display controls and protection declarations.
use super::{
    BAD, ConversionError, SampleEntryMetadata, Scheme, SemanticBox, UNSUPPORTED,
    crypto::TrackEncryption,
    init,
    read::{count, full, integer, one, optional, signed16},
};
use crate::{inspection::TrackKind, mp4::Mp4Box};
use zeroize::Zeroizing;

pub(super) struct ParsedEntry {
    pub codec: [u8; 4],
    pub decoder_config_kind: [u8; 4],
    pub decoder_config: Vec<u8>,
    pub decoder_config_box: Vec<u8>,
    pub sample_entry_metadata: SampleEntryMetadata,
    pub encryption: Option<TrackEncryption>,
}

fn track_encryption(data: &[u8], scheme: Scheme) -> Result<TrackEncryption, ConversionError> {
    let version = *data.first().ok_or(BAD)?;
    if version > 1 {
        return Err(UNSUPPORTED);
    }
    full(data, version, 0)?;
    if data.len() < 24 || data[4] != 0 || data[6] != 1 {
        return Err(BAD);
    }
    let pattern = if version == 0 {
        if data[5] != 0 {
            return Err(BAD);
        }
        0
    } else {
        data[5]
    };
    if (scheme == Scheme::Cenc && pattern != 0) || (pattern >> 4 == 0 && pattern & 15 != 0) {
        return Err(UNSUPPORTED);
    }
    let iv_size = data[7];
    let kid = data[8..24].try_into().map_err(|_| BAD)?;
    let constant_iv = if iv_size == 0 {
        let size = usize::from(*data.get(24).ok_or(BAD)?);
        if !matches!(size, 8 | 16) || data.len() != 25 + size {
            return Err(BAD);
        }
        Some(Zeroizing::new(data[25..].to_vec()))
    } else {
        if !matches!(iv_size, 8 | 16) || data.len() != 24 {
            return Err(BAD);
        }
        None
    };
    if scheme == Scheme::Cbcs
        && (iv_size == 8 || constant_iv.as_ref().is_some_and(|iv| iv.len() != 16))
    {
        return Err(UNSUPPORTED);
    }
    Ok(TrackEncryption {
        scheme,
        kid,
        iv_size,
        constant_iv,
        crypt: pattern >> 4,
        skip: pattern & 15,
    })
}

fn encryption(
    entry: &Mp4Box,
    data: &[u8],
) -> Result<([u8; 4], Option<TrackEncryption>), ConversionError> {
    let protected = matches!(&entry.kind(), b"encv" | b"enca");
    let sinf = optional(&entry.children, *b"sinf")?;
    if !protected {
        if sinf.is_some() {
            return Err(BAD);
        }
        return Ok((entry.kind(), None));
    }
    let sinf = sinf.ok_or(BAD)?;
    if sinf.children.len() != 3 {
        return Err(UNSUPPORTED);
    }
    let codec = one(&sinf.children, *b"frma")?
        .header
        .payload(data)
        .try_into()
        .map_err(|_| BAD)?;
    let scheme_header = one(&sinf.children, *b"schm")?.header.payload(data);
    full(scheme_header, 0, 0)?;
    if scheme_header.len() != 12 || integer(scheme_header, 8)? != 0x0001_0000 {
        return Err(UNSUPPORTED);
    }
    let scheme = match scheme_header.get(4..8) {
        Some(b"cenc") => Scheme::Cenc,
        Some(b"cbcs") => Scheme::Cbcs,
        _ => return Err(UNSUPPORTED),
    };
    let information = one(&sinf.children, *b"schi")?;
    if information.children.len() != 1 {
        return Err(UNSUPPORTED);
    }
    let tenc = one(&information.children, *b"tenc")?.header.payload(data);
    Ok((codec, Some(track_encryption(tenc, scheme)?)))
}

fn codec_kind(codec: [u8; 4]) -> Result<([u8; 4], bool), ConversionError> {
    Ok(match &codec {
        b"avc1" | b"avc3" => (*b"avcC", true),
        b"hvc1" | b"hev1" => (*b"hvcC", true),
        b"av01" => (*b"av1C", true),
        b"vp09" => (*b"vpcC", true),
        b"mp4a" => (*b"esds", false),
        b"ac-3" => (*b"dac3", false),
        b"ec-3" => (*b"dec3", false),
        b"Opus" => (*b"dOps", false),
        b"fLaC" => (*b"dfLa", false),
        _ => return Err(UNSUPPORTED),
    })
}

fn color(data: &[u8]) -> Result<(), ConversionError> {
    match data.get(..4) {
        Some(b"nclc") if data.len() == 10 => Ok(()),
        Some(b"nclx") if data.len() == 11 && data[10].trailing_zeros() >= 7 => Ok(()),
        Some(b"prof" | b"rICC") => {
            let profile = data.get(4..).ok_or(BAD)?;
            if profile.len() < 132
                || usize::try_from(integer(profile, 0)?).map_err(|_| BAD)? != profile.len()
                || profile.get(36..40) != Some(b"acsp")
            {
                return Err(BAD);
            }
            let total = usize::try_from(integer(profile, 128)?).map_err(|_| BAD)?;
            let end = total
                .checked_mul(12)
                .and_then(|n| n.checked_add(132))
                .ok_or(BAD)?;
            for tag in profile.get(132..end).ok_or(BAD)?.chunks_exact(12) {
                let offset = usize::try_from(integer(tag, 4)?).map_err(|_| BAD)?;
                let size = usize::try_from(integer(tag, 8)?).map_err(|_| BAD)?;
                if offset < end
                    || offset
                        .checked_add(size)
                        .is_none_or(|end| end > profile.len())
                {
                    return Err(BAD);
                }
            }
            Ok(())
        }
        Some(b"nclc" | b"nclx") => Err(BAD),
        _ => Err(UNSUPPORTED),
    }
}

fn semantic(kind: [u8; 4], data: &[u8]) -> Result<(), ConversionError> {
    let valid = match &kind {
        b"pasp" => data.len() == 8 && integer(data, 0)? != 0 && integer(data, 4)? != 0,
        b"colr" => return color(data),
        b"clap" => {
            data.len() == 32
                && [4, 12, 20, 28]
                    .into_iter()
                    .all(|at| integer(data, at).is_ok_and(|n| n != 0))
        }
        b"fiel" => {
            data.len() == 2 && matches!(data[0], 1 | 2) && matches!(data[1], 0 | 1 | 6 | 9 | 14)
        }
        b"chan" => {
            full(data, 0, 0)?;
            count(data, 12, 16, 20)?;
            true
        }
        b"mdcv" => data.len() == 24,
        b"clli" => data.len() == 4,
        _ => return Err(UNSUPPORTED),
    };
    if valid { Ok(()) } else { Err(BAD) }
}

fn semantic_boxes(
    entry: &Mp4Box,
    config: [u8; 4],
    data: &[u8],
) -> Result<Vec<SemanticBox>, ConversionError> {
    let mut boxes = Vec::new();
    for child in &entry.children {
        let kind = child.kind();
        if kind == config || kind == *b"sinf" {
            continue;
        }
        if kind == *b"btrt" {
            if child.header.payload(data).len() != 12 {
                return Err(BAD);
            }
            continue;
        }
        semantic(kind, child.header.payload(data))?;
        boxes.push(SemanticBox {
            kind,
            payload: child.header.payload(data).to_vec(),
        });
    }
    boxes.sort_unstable_by_key(|item| item.kind);
    if boxes.windows(2).any(|pair| pair[0].kind == pair[1].kind) {
        return Err(BAD);
    }
    Ok(boxes)
}

fn metadata(
    entry: &Mp4Box,
    track: &Mp4Box,
    mdhd: &[u8],
    visual: bool,
    config: [u8; 4],
    data: &[u8],
) -> Result<SampleEntryMetadata, ConversionError> {
    let value = entry.header.payload(data);
    let tkhd = one(&track.children, *b"tkhd")?.header.payload(data);
    let version = *tkhd.first().ok_or(BAD)?;
    let flags = full(tkhd, version, 15)?;
    let extra = if version == 0 { 0 } else { 12 };
    let mut metadata = SampleEntryMetadata {
        width: None,
        height: None,
        channels: None,
        sample_size: None,
        sample_rate: None,
        language: init::language(mdhd)?,
        display_matrix: init::matrix(tkhd, 40 + extra)?,
        display_width: integer(tkhd, 76 + extra)?,
        display_height: integer(tkhd, 80 + extra)?,
        track_flags: flags,
        layer: signed16(tkhd, 32 + extra)?,
        alternate_group: signed16(tkhd, 34 + extra)?,
        volume: signed16(tkhd, 36 + extra)?,
        boxes: semantic_boxes(entry, config, data)?,
    };
    if visual {
        if value.len() < 78 {
            return Err(BAD);
        }
        let width = signed16(value, 24)?.cast_unsigned();
        let height = signed16(value, 26)?.cast_unsigned();
        if width == 0 || height == 0 {
            return Err(BAD);
        }
        metadata.width = Some(width);
        metadata.height = Some(height);
    } else {
        if value.len() < 28 || value.get(8..10) != Some(&[0, 0]) {
            return Err(UNSUPPORTED);
        }
        let rate = integer(value, 24)?;
        if rate & 0xffff != 0 {
            return Err(UNSUPPORTED);
        }
        metadata.channels = Some(signed16(value, 16)?.cast_unsigned());
        metadata.sample_size = Some(signed16(value, 18)?.cast_unsigned());
        metadata.sample_rate = Some(rate >> 16);
    }
    Ok(metadata)
}

pub(super) fn parse(
    table: &Mp4Box,
    track: &Mp4Box,
    mdhd: &[u8],
    kind: TrackKind,
    data: &[u8],
) -> Result<ParsedEntry, ConversionError> {
    let stsd = one(&table.children, *b"stsd")?;
    let payload = stsd.header.payload(data);
    full(payload, 0, 0)?;
    if integer(payload, 4)? != 1 || stsd.children.len() != 1 {
        return Err(UNSUPPORTED);
    }
    let entry = &stsd.children[0];
    let value = entry.header.payload(data);
    if value.get(6..8) != Some(&[0, 1]) || value.get(..6) != Some(&[0; 6]) {
        return Err(UNSUPPORTED);
    }
    let (codec, encryption) = encryption(entry, data)?;
    let (decoder_config_kind, visual) = codec_kind(codec)?;
    if visual != (kind == TrackKind::Video)
        || matches!(&entry.kind(), b"encv" | b"enca") && (entry.kind() == *b"encv") != visual
    {
        return Err(BAD);
    }
    let config = one(&entry.children, decoder_config_kind)?;
    let decoder_config = if decoder_config_kind == *b"esds" {
        init::audio_specific_config(config.header.payload(data))?
    } else {
        config.header.payload(data).to_vec()
    };
    if decoder_config.is_empty() {
        return Err(BAD);
    }
    Ok(ParsedEntry {
        codec,
        decoder_config_kind,
        decoder_config,
        decoder_config_box: config.header.bytes(data).to_vec(),
        sample_entry_metadata: metadata(entry, track, mdhd, visual, decoder_config_kind, data)?,
        encryption,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mp4;
    fn boxed(kind: [u8; 4], payload: &[u8]) -> Vec<u8> {
        let mut bytes = u32::try_from(payload.len() + 8)
            .unwrap()
            .to_be_bytes()
            .to_vec();
        bytes.extend(kind);
        bytes.extend(payload);
        bytes
    }
    #[test]
    fn competing_codec_configuration_is_rejected_even_when_expected_configuration_exists() {
        let mut value = vec![0; 78];
        value.extend(boxed(*b"avcC", &[1, 2, 3, 4]));
        value.extend(boxed(*b"hvcC", &[1, 2, 3, 4]));
        let data = boxed(*b"avc1", &value);
        let nodes = mp4::parse_boxes(&data).unwrap();
        assert!(semantic_boxes(&nodes[0], *b"avcC", &data).is_err());
    }
    #[test]
    fn semantic_metadata_shapes_and_reserved_bits_are_checked() {
        semantic(*b"pasp", &[0, 0, 0, 1, 0, 0, 0, 1]).unwrap();
        assert!(semantic(*b"pasp", &[0; 8]).is_err());
        assert!(semantic(*b"clap", &[0; 32]).is_err());
        let mut color = b"nclx\x00\x01\x00\x01\x00\x01\x80".to_vec();
        semantic(*b"colr", &color).unwrap();
        color[10] |= 1;
        assert!(semantic(*b"colr", &color).is_err());
        assert!(semantic(*b"fiel", &[3, 0]).is_err());
        assert!(semantic(*b"chan", &[0; 15]).is_err());
        assert!(semantic(*b"mdcv", &[0; 23]).is_err());
        assert!(semantic(*b"clli", &[0; 3]).is_err());
    }
}
