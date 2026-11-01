//! Development-only bounded entrypoints to the production codec parsers.
use super::{ConversionError, hevc as parse_hevc, native::fuzz_samples, vp9 as parse_vp9};

pub const MAX_INPUT_BYTES: usize = 65_536;
const MAX_CONFIG_BYTES: usize = 16_384;
const MAX_SAMPLES: usize = 16;
const FRAMING: ConversionError = ConversionError("fuzz-input-framing-invalid");

/// Check a raw hvcC configuration or a bounded sequence using native sample evidence.
pub fn hevc(input: &[u8]) -> Result<usize, ConversionError> {
    let (mode, configuration, rest) = envelope(input)?;
    let codec = if mode & 2 == 0 { *b"hvc1" } else { *b"hev1" };
    let (configuration, width) = parse_hevc::Configuration::from_hvcc(configuration)?;
    configuration.validate_entry(codec)?;
    configuration.codec_string(codec)?;
    if mode & 1 == 0 {
        return Ok(0);
    }
    fuzz_samples::hevc(configuration, width, codec, &samples(rest)?)
}

/// Check a raw vpcC configuration or a bounded reference/superframe sequence.
pub fn vp9(input: &[u8]) -> Result<usize, ConversionError> {
    let (mode, configuration, mut rest) = envelope(input)?;
    let configuration = parse_vp9::Configuration::from_vpcc(configuration)?;
    configuration.codec_string();
    if mode & 1 == 0 {
        return Ok(0);
    }
    let width = u32::from(word(&mut rest)?);
    let height = u32::from(word(&mut rest)?);
    fuzz_samples::vp9(configuration, width, height, &samples(rest)?)
}

fn envelope(input: &[u8]) -> Result<(u8, &[u8], &[u8]), ConversionError> {
    if input.len() > MAX_INPUT_BYTES {
        return Err(ConversionError("fuzz-input-size-limit"));
    }
    let (&mode, mut rest) = input.split_first().ok_or(FRAMING)?;
    if mode & !3 != 0 {
        return Err(FRAMING);
    }
    let length = if mode & 1 != 0 {
        usize::from(word(&mut rest)?)
    } else {
        rest.len()
    };
    if length > MAX_CONFIG_BYTES {
        return Err(ConversionError("fuzz-configuration-size-limit"));
    }
    let configuration = take(&mut rest, length)?;
    Ok((mode, configuration, rest))
}

fn samples(mut rest: &[u8]) -> Result<Vec<(u8, &[u8])>, ConversionError> {
    let mut result = Vec::new();
    while !rest.is_empty() {
        if result.len() == MAX_SAMPLES {
            return Err(ConversionError("fuzz-sample-count-limit"));
        }
        let flags = take(&mut rest, 1)?[0];
        if flags & !3 != 0 {
            return Err(FRAMING);
        }
        let length = usize::from(word(&mut rest)?);
        if length == 0 {
            return Err(FRAMING);
        }
        result.push((flags, take(&mut rest, length)?));
    }
    if result.is_empty() {
        return Err(FRAMING);
    }
    Ok(result)
}

fn word(rest: &mut &[u8]) -> Result<u16, ConversionError> {
    Ok(u16::from_le_bytes(
        take(rest, 2)?.try_into().map_err(|_| FRAMING)?,
    ))
}

fn take<'a>(rest: &mut &'a [u8], length: usize) -> Result<&'a [u8], ConversionError> {
    let (value, tail) = rest.split_at_checked(length).ok_or(FRAMING)?;
    *rest = tail;
    Ok(value)
}

macro_rules! seed {
    ($name:literal, $samples:expr) => {
        (
            include_bytes!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../fuzz/corpus/",
                $name
            ))
            .as_slice(),
            $samples,
        )
    };
}

const HEVC_SEEDS: &[(&[u8], Option<usize>)] = &[
    seed!("hevc/hevc-main-config", Some(0)),
    seed!("hevc/hevc-main-sequence", Some(16)),
    seed!("hevc/hevc-main10-config", Some(0)),
    seed!("hevc/hevc-main10-sequence", Some(16)),
    seed!("hevc/empty-configuration", None),
    seed!("hevc/truncated-envelope", None),
];

const VP9_SEEDS: &[(&[u8], Option<usize>)] = &[
    seed!("vp9/vp9-p0-8-config", Some(0)),
    seed!("vp9/vp9-p0-8-sequence", Some(16)),
    seed!("vp9/vp9-p0-8-superframe", Some(1)),
    seed!("vp9/vp9-p1-8-config", Some(0)),
    seed!("vp9/vp9-p1-8-sequence", Some(16)),
    seed!("vp9/vp9-p1-8-superframe", Some(1)),
    seed!("vp9/vp9-p2-10-config", Some(0)),
    seed!("vp9/vp9-p2-10-sequence", Some(16)),
    seed!("vp9/vp9-p2-10-superframe", Some(1)),
    seed!("vp9/vp9-p2-12-config", Some(0)),
    seed!("vp9/vp9-p2-12-sequence", Some(16)),
    seed!("vp9/vp9-p2-12-superframe", Some(1)),
    seed!("vp9/vp9-p3-10-config", Some(0)),
    seed!("vp9/vp9-p3-10-sequence", Some(16)),
    seed!("vp9/vp9-p3-10-superframe", Some(1)),
    seed!("vp9/vp9-p3-12-config", Some(0)),
    seed!("vp9/vp9-p3-12-sequence", Some(16)),
    seed!("vp9/vp9-p3-12-superframe", Some(1)),
    seed!("vp9/empty-configuration", None),
    seed!("vp9/truncated-envelope", None),
];

/// Replay the exact accepted and refused inputs before coverage-guided mutations.
pub fn replay_hevc_seeds() -> (usize, usize) {
    replay(hevc, HEVC_SEEDS)
}

/// Replay every supported VP9 profile/depth pair and explicit superframe index.
pub fn replay_vp9_seeds() -> (usize, usize) {
    replay(vp9, VP9_SEEDS)
}

fn replay(
    parser: fn(&[u8]) -> Result<usize, ConversionError>,
    seeds: &[(&[u8], Option<usize>)],
) -> (usize, usize) {
    let mut accepted = 0;
    let mut refused = 0;
    for &(input, expected) in seeds {
        if let Some(samples) = expected {
            assert_eq!(parser(input).unwrap(), samples);
            accepted += 1;
        } else {
            assert!(parser(input).is_err());
            refused += 1;
        }
    }
    (accepted, refused)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepted_hevc_seeds_reach_real_configuration_and_persistent_sample_checks() {
        assert_eq!(replay_hevc_seeds(), (4, 2));
    }

    #[test]
    fn accepted_vp9_seeds_reach_all_profile_depth_pairs_state_and_superframe_index() {
        assert_eq!(replay_vp9_seeds(), (18, 2));
    }

    #[test]
    fn invalid_framing_and_input_size_are_refused_before_unbounded_work() {
        for check in [hevc, vp9] {
            assert!(check(&[]).is_err());
            assert!(check(&[1, 255, 255]).is_err());
            assert_eq!(
                check(&vec![0; MAX_INPUT_BYTES + 1]).unwrap_err().0,
                "fuzz-input-size-limit"
            );
        }
    }

    #[test]
    fn native_chunked_evidence_accepts_the_same_valid_sequences() {
        for (parser, seeds, geometry) in
            [(hevc as fn(&[u8]) -> _, HEVC_SEEDS, 0), (vp9, VP9_SEEDS, 4)]
        {
            for &(input, expected) in seeds.iter().filter(|(_, count)| *count == Some(16)) {
                let mut chunked = input.to_vec();
                let mut offset =
                    3 + usize::from(u16::from_le_bytes([input[1], input[2]])) + geometry;
                while offset < chunked.len() {
                    chunked[offset] |= 2;
                    offset += 3 + usize::from(u16::from_le_bytes([
                        chunked[offset + 1],
                        chunked[offset + 2],
                    ]));
                }
                assert_eq!(parser(&chunked).unwrap(), expected.unwrap());
            }
        }
    }

    #[test]
    fn malformed_stateful_sequences_refuse_incomplete_excess_and_sync_mismatch() {
        for (parser, seeds, geometry) in
            [(hevc as fn(&[u8]) -> _, HEVC_SEEDS, 0), (vp9, VP9_SEEDS, 4)]
        {
            let input = seeds
                .iter()
                .find(|(_, count)| *count == Some(16))
                .unwrap()
                .0;
            let offset = 3 + usize::from(u16::from_le_bytes([input[1], input[2]])) + geometry;
            assert!(parser(&input[..offset]).is_err());
            assert!(parser(&input[..input.len() - 1]).is_err());
            let mut mismatch = input.to_vec();
            mismatch[offset] ^= 1;
            assert!(parser(&mismatch).is_err());
            let length =
                3 + usize::from(u16::from_le_bytes([input[offset + 1], input[offset + 2]]));
            let mut excess = input.to_vec();
            excess.extend_from_slice(&input[offset..offset + length]);
            assert_eq!(parser(&excess).unwrap_err().0, "fuzz-sample-count-limit");
        }
    }
}
