use super::*;

#[derive(Default)]
struct Writer {
    bits: Vec<bool>,
}
impl Writer {
    fn bit(&mut self, value: bool) {
        self.bits.push(value);
    }
    fn u(&mut self, value: u32, width: u32) {
        for shift in (0..width).rev() {
            self.bit(value & (1 << shift) != 0);
        }
    }
    fn ue(&mut self, value: u32) {
        let word = u64::from(value) + 1;
        let width = 64 - word.leading_zeros();
        for _ in 1..width {
            self.bit(false);
        }
        for shift in (0..width).rev() {
            self.bit(word & (1 << shift) != 0);
        }
    }
    fn se(&mut self, value: i32) {
        self.ue(if value <= 0 {
            value.unsigned_abs() * 2
        } else {
            value.cast_unsigned() * 2 - 1
        });
    }
    fn align(&mut self) {
        self.bit(true);
        while !self.bits.len().is_multiple_of(8) {
            self.bit(false);
        }
    }
    fn bytes(self) -> Vec<u8> {
        assert!(self.bits.len().is_multiple_of(8));
        self.bits
            .chunks(8)
            .map(|bits| {
                bits.iter()
                    .fold(0, |value, bit| (value << 1) | u8::from(*bit))
            })
            .collect()
    }
    fn nal(mut self, kind: u8) -> Vec<u8> {
        self.align();
        escaped(kind, &self.bytes())
    }
}
fn escaped(kind: u8, bytes: &[u8]) -> Vec<u8> {
    let mut nal = vec![kind << 1, 1];
    let mut zeros = 0;
    for &byte in bytes {
        if zeros == 2 && byte <= 3 {
            nal.push(3);
            zeros = 0;
        }
        nal.push(byte);
        zeros = if byte == 0 { zeros + 1 } else { 0 };
    }
    nal
}
fn ptl(b: &mut Writer, depth: u8) {
    b.u(if depth == 8 { 1 } else { 2 }, 8);
    b.u(if depth == 8 { 0x6000_0000 } else { 0x2000_0000 }, 32);
    b.u(0x9000_0000, 32);
    b.u(0, 16);
    b.u(60, 8);
}
fn parameters(depth: u8) -> [Vec<u8>; 3] {
    parameters_with(depth, Features::default())
}
#[derive(Default, Clone, Copy)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "These independent syntax switches generate combinations for parser fixtures."
)]
struct Features {
    tiles: bool,
    dependent: bool,
    long_term: bool,
    weighted: bool,
}
fn parameters_with(depth: u8, features: Features) -> [Vec<u8>; 3] {
    let mut v = Writer::default();
    v.u(0, 4);
    v.u(3, 2);
    v.u(0, 6);
    v.u(0, 3);
    v.bit(true);
    v.u(65535, 16);
    ptl(&mut v, depth);
    v.bit(false);
    v.ue(4);
    v.ue(2);
    v.ue(0);
    v.u(0, 6);
    v.ue(0);
    v.bit(false);
    v.bit(false);
    let mut s = Writer::default();
    s.u(0, 4);
    s.u(0, 3);
    s.bit(true);
    ptl(&mut s, depth);
    s.ue(0);
    s.ue(1);
    s.ue(128);
    s.ue(128);
    s.bit(false);
    s.ue(u32::from(depth - 8));
    s.ue(u32::from(depth - 8));
    s.ue(4);
    s.bit(false);
    s.ue(4);
    s.ue(2);
    s.ue(0);
    s.ue(0);
    s.ue(3);
    s.ue(0);
    s.ue(3);
    s.ue(0);
    s.ue(0);
    s.bit(false);
    s.bit(true);
    s.bit(true);
    s.bit(false);
    s.ue(0);
    s.bit(features.long_term);
    if features.long_term {
        s.ue(1);
        s.u(9, 8);
        s.bit(true);
    }
    s.bit(true);
    s.bit(true);
    s.bit(false);
    s.bit(false);
    let mut p = Writer::default();
    p.ue(0);
    p.ue(0);
    p.bit(features.dependent);
    p.bit(false);
    p.u(0, 3);
    p.bit(true);
    p.bit(true);
    p.ue(0);
    p.ue(0);
    p.se(0);
    p.bit(false);
    p.bit(false);
    p.bit(false);
    p.se(0);
    p.se(0);
    p.bit(false);
    p.bit(features.weighted);
    p.bit(features.weighted);
    p.bit(false);
    p.bit(features.tiles);
    p.bit(!features.tiles);
    if features.tiles {
        p.ue(1);
        p.ue(0);
        p.bit(true);
        p.bit(true);
    }
    p.bit(true);
    p.bit(false);
    p.bit(false);
    p.bit(features.long_term);
    p.ue(0);
    p.bit(false);
    p.bit(false);
    [v.nal(32), s.nal(33), p.nal(34)]
}
pub(super) fn hvcc(depth: u8) -> Vec<u8> {
    hvcc_with(depth, Features::default())
}
fn hvcc_with(depth: u8, features: Features) -> Vec<u8> {
    let mut data = vec![1];
    let mut identity = Writer::default();
    ptl(&mut identity, depth);
    data.extend(identity.bytes());
    data.extend([
        0xf0,
        0,
        0xfc,
        0xfd,
        0xf8 | (depth - 8),
        0xf8 | (depth - 8),
        0,
        0,
        0x0f,
        3,
    ]);
    for (kind, nal) in (32..=34).zip(parameters_with(depth, features)) {
        data.push(0x80 | kind);
        data.extend(1_u16.to_be_bytes());
        data.extend(u16::try_from(nal.len()).unwrap().to_be_bytes());
        data.extend(nal);
    }
    data
}
#[test]
fn main_and_main10_configuration_are_parsed_from_declared_parameter_sets() {
    for depth in [8, 10] {
        let (config, width) = Configuration::from_hvcc(&super::test_configuration(depth)).unwrap();
        assert_eq!(width, 4);
        assert_eq!(config.metadata().width, 128);
        assert_eq!(config.metadata().height, 128);
        assert_eq!(config.metadata().bit_depth_luma, depth);
        assert!(config.metadata().progressive);
        assert!(
            config
                .codec_string(*b"hvc1")
                .unwrap()
                .starts_with(if depth == 8 {
                    "hvc1.1.6.L60.90"
                } else {
                    "hvc1.2.4.L60.90"
                })
        );
    }
}
pub(super) fn idr_slice(first: bool, address: u32, entry_point: bool) -> (Vec<u8>, usize) {
    let mut b = Writer::default();
    b.bit(first);
    b.bit(false);
    b.ue(0);
    if !first {
        b.u(address, 2);
    }
    b.ue(2);
    b.bit(false);
    b.bit(false);
    b.se(0);
    b.bit(false);
    b.ue(u32::from(entry_point));
    if entry_point {
        b.ue(31);
        b.u(0, 32);
    }
    b.align();
    let header = escaped(20, &b.bytes());
    let extent = header.len();
    let mut nal = header;
    nal.extend([0x55; 64]);
    (nal, extent)
}
fn packed(units: &[&[u8]], width: u8) -> Vec<u8> {
    let mut result = Vec::new();
    for unit in units {
        let size = u32::try_from(unit.len()).unwrap().to_be_bytes();
        result.extend_from_slice(&size[4 - usize::from(width)..]);
        result.extend_from_slice(unit);
    }
    result
}
#[test]
fn complete_slice_headers_keep_emulation_prevention_bytes_clear() {
    let (config, width) = Configuration::from_hvcc(&hvcc(8)).unwrap();
    let (first, extent) = super::test_slice();
    assert!(first[..extent].windows(3).any(|bytes| bytes == [0, 0, 3]));
    assert_eq!(config.slice_header_bytes(&first).unwrap(), extent);
    for end in 0..=extent {
        assert!(config.slice_header_bytes(&first[..end]).is_err(), "{end}");
    }
    let (second, _) = idr_slice(false, 2, false);
    assert!(
        config
            .length_prefixed_access_unit(&packed(&[&first], width), width, *b"hvc1")
            .is_ok()
    );
    assert!(
        config
            .length_prefixed_access_unit(&packed(&[&first, &second], width), width, *b"hvc1")
            .is_err()
    );
    let (first, _) = idr_slice(true, 0, false);
    let unit = config
        .length_prefixed_access_unit(&packed(&[&first, &second], width), width, *b"hvc1")
        .unwrap();
    assert_eq!(unit.slice_count, 2);
    assert!(unit.is_sync);
    assert_eq!(unit.random_access, RandomAccess::Idr);
    assert!(
        config
            .length_prefixed_access_unit(&packed(&[&second], width), width, *b"hvc1")
            .is_err()
    );
    assert!(
        config
            .length_prefixed_access_unit(&packed(&[&first, &first], width), width, *b"hvc1")
            .is_err()
    );
    assert!(
        config
            .length_prefixed_access_unit(
                &packed(&[&first, &second, &second], width),
                width,
                *b"hvc1"
            )
            .is_err()
    );
}
#[test]
fn stable_hev1_parameters_are_framing_and_other_configuration_meaning_is_retained() {
    let source = hvcc(8);
    let (config, width) = Configuration::from_hvcc(&source).unwrap();
    let (slice, _) = idr_slice(true, 0, true);
    let params = parameters(8);
    let with_params = packed(&[&params[0], &params[1], &params[2], &slice], width);
    assert!(
        config
            .length_prefixed_access_unit(&with_params, width, *b"hvc1")
            .is_err()
    );
    assert!(
        config
            .length_prefixed_access_unit(&with_params, width, *b"hev1")
            .is_ok()
    );
    let mut first = Sha256::new();
    config
        .units_with_width(&with_params, width, *b"hev1", &mut first)
        .unwrap();
    let mut second = Sha256::new();
    config
        .units_with_width(&packed(&[&slice], width), width, *b"hvc1", &mut second)
        .unwrap();
    assert_eq!(first.finalize(), second.finalize());
    let mut moved = source.clone();
    moved[21] &= !3;
    moved[23] &= !128;
    let (moved, _) = Configuration::from_hvcc(&moved).unwrap();
    assert!(config.semantic_eq(&moved));
    assert!(moved.validate_entry(*b"hvc1").is_err());
    let mut changed = source;
    changed[19] = 1;
    let (changed, _) = Configuration::from_hvcc(&changed).unwrap();
    assert!(!config.semantic_eq(&changed));
}
#[test]
fn real_main_and_main10_samples_include_b_frames_multiple_slices_cra_and_leading_pictures() {
    use super::super::{InputKeys, native_media};
    for (name, depth, codec) in [
        ("hevc-main.mp4", 8, *b"hvc1"),
        ("hevc-main10.mp4", 10, *b"hev1"),
    ] {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../drmr/tests/fixtures/bbb")
            .join(name);
        let mut config = None;
        let mut samples = 0;
        let mut cra = 0;
        let mut leading = 0;
        let mut multiple = 0;
        let mut reordered = false;
        native_media::scan(
            &path,
            &InputKeys::default(),
            &crate::concurrency::Stop::new(),
            &mut |track, info, bytes| {
                if track.kind != crate::inspection::TrackKind::Video {
                    return Ok(());
                }
                if config.is_none() {
                    config = Some(Configuration::from_hvcc(&track.decoder_config)?);
                }
                let (configuration, width) = config.as_ref().unwrap();
                assert!(info.is_last_chunk);
                let unit = configuration.length_prefixed_access_unit(bytes, *width, codec)?;
                assert_eq!(unit.is_sync, info.is_sync, "{name} sample{}", info.ordinal);
                for nal in units(bytes, *width)? {
                    if bits::header(nal)?.kind <= 31 {
                        let extent = configuration.slice_header_bytes(nal)?;
                        assert!(extent >= 3 && extent < nal.len());
                    }
                }
                samples += 1;
                cra += u32::from(unit.random_access == RandomAccess::Cra);
                leading += u32::from(unit.leading_picture);
                multiple += u32::from(unit.slice_count > 1);
                reordered |= info.pts != info.dts;
                Ok(())
            },
        )
        .unwrap_or_else(|error| panic!("{name} after {samples} samples: {error}"));
        let (config, _) = config.unwrap();
        assert_eq!(config.metadata().bit_depth_luma, depth);
        assert_eq!(samples, 240);
        assert!(reordered);
        if depth == 8 {
            assert_eq!(multiple, 240);
        } else {
            assert!(cra > 0);
            assert!(leading > 0);
        }
    }
}
fn coded(mut b: Writer, kind: u8) -> Vec<u8> {
    b.align();
    let mut nal = escaped(kind, &b.bytes());
    nal.extend([0x55; 64]);
    nal
}
fn inter_slice(kind: u32, features: Features) -> Vec<u8> {
    let mut b = Writer::default();
    b.bit(true);
    b.ue(0);
    b.ue(kind);
    b.u(10, 8);
    b.bit(false);
    b.ue(1);
    b.ue(0);
    b.ue(0);
    b.bit(true);
    if features.long_term {
        b.ue(1);
        b.ue(0);
        b.bit(false);
    }
    b.bit(true);
    b.bit(false);
    b.bit(false);
    b.bit(false);
    if features.long_term {
        b.bit(true);
        b.u(1, 1);
        if kind == 0 {
            b.bit(true);
            b.u(0, 1);
        }
    }
    if kind == 0 {
        b.bit(false);
    }
    b.bit(false);
    if kind == 0 {
        b.bit(true);
    }
    if features.weighted {
        b.ue(0);
        b.se(0);
        for _ in 0..if kind == 0 { 2 } else { 1 } {
            b.bit(true);
            b.bit(true);
            b.se(0);
            b.se(0);
            for _ in 0..2 {
                b.se(0);
                b.se(0);
            }
        }
    }
    b.ue(0);
    b.se(0);
    b.bit(false);
    b.ue(1);
    b.ue(0);
    b.u(0, 1);
    coded(b, 1)
}
#[test]
fn p_and_b_slices_parse_weighted_predictions_and_short_and_long_term_references() {
    let features = Features {
        long_term: true,
        weighted: true,
        ..Features::default()
    };
    let (configuration, width) = Configuration::from_hvcc(&hvcc_with(10, features)).unwrap();
    for kind in [0, 1] {
        let nal = inter_slice(kind, features);
        let result = configuration
            .length_prefixed_access_unit(&packed(&[&nal], width), width, *b"hvc1")
            .unwrap();
        assert!(!result.is_sync);
        assert_eq!(result.slice_count, 1);
        let extent = configuration.slice_header_bytes(&nal).unwrap();
        assert!(extent > 3 && extent < nal.len());
        for end in 0..=extent {
            assert!(
                configuration.slice_header_bytes(&nal[..end]).is_err(),
                "kind{kind} end{end}"
            );
        }
    }
}
fn dependent_idr(address: u32) -> Vec<u8> {
    let mut b = Writer::default();
    b.bit(false);
    b.bit(false);
    b.ue(0);
    b.bit(true);
    b.u(address, 2);
    b.ue(0);
    coded(b, 20)
}
#[test]
fn dependent_slices_use_tile_scan_order_and_cannot_start_a_picture() {
    let features = Features {
        tiles: true,
        dependent: true,
        ..Features::default()
    };
    let (configuration, width) = Configuration::from_hvcc(&hvcc_with(8, features)).unwrap();
    let (first, _) = idr_slice(true, 0, false);
    let middle = dependent_idr(2);
    let last = dependent_idr(1);
    let result = configuration
        .length_prefixed_access_unit(&packed(&[&first, &middle, &last], width), width, *b"hvc1")
        .unwrap();
    assert_eq!(result.slice_count, 3);
    assert!(result.is_sync);
    assert!(configuration.slice_header_bytes(&middle).unwrap() < middle.len());
    assert!(
        configuration
            .length_prefixed_access_unit(&packed(&[&middle, &last], width), width, *b"hvc1")
            .is_err()
    );
    assert!(
        configuration
            .length_prefixed_access_unit(&packed(&[&first, &last, &middle], width), width, *b"hvc1")
            .is_err()
    );
}
fn sei_nal(kind: u8, payloads: &[(u8, Vec<u8>)]) -> Vec<u8> {
    let mut bytes = Vec::new();
    for (kind, payload) in payloads {
        bytes.push(*kind);
        let mut length = payload.len();
        while length >= 255 {
            bytes.push(255);
            length -= 255;
        }
        bytes.push(u8::try_from(length).unwrap());
        bytes.extend_from_slice(payload);
    }
    bytes.push(0x80);
    escaped(kind, &bytes)
}
#[test]
fn hdr_and_decoded_hash_sei_are_checked_and_never_disappear_from_evidence() {
    let (configuration, width) = Configuration::from_hvcc(&hvcc(10)).unwrap();
    let (slice, _) = idr_slice(true, 0, true);
    let prefix = sei_nal(
        39,
        &[(137, vec![0; 24]), (144, vec![0; 4]), (147, vec![18])],
    );
    let suffix = sei_nal(40, &[(132, vec![0; 49])]);
    let input = packed(&[&prefix, &slice, &suffix], width);
    assert!(
        configuration
            .length_prefixed_access_unit(&input, width, *b"hvc1")
            .is_ok()
    );
    let mut first = Sha256::new();
    configuration
        .units_with_width(&input, width, *b"hvc1", &mut first)
        .unwrap();
    let mut second = Sha256::new();
    configuration
        .units_with_width(&packed(&[&slice], width), width, *b"hvc1", &mut second)
        .unwrap();
    assert_ne!(first.finalize(), second.finalize());
    for (kind, size) in [(137, 23), (144, 3), (147, 2), (132, 48)] {
        let wrong = sei_nal(if kind == 132 { 40 } else { 39 }, &[(kind, vec![0; size])]);
        let input = if kind == 132 {
            packed(&[&slice, &wrong], width)
        } else {
            packed(&[&wrong, &slice], width)
        };
        assert!(
            configuration
                .length_prefixed_access_unit(&input, width, *b"hvc1")
                .is_err(),
            "{kind}"
        );
    }
    let unknown = sei_nal(39, &[(45, vec![0; 3])]);
    assert_eq!(
        configuration
            .length_prefixed_access_unit(&packed(&[&unknown, &slice], width), width, *b"hvc1")
            .unwrap_err()
            .0,
        "native-hevc-sei-payload-not-supported"
    );
}
#[test]
fn optional_configuration_sei_arrays_are_validated_and_part_of_the_identity() {
    let mut base = hvcc(8);
    let (configuration, _) = Configuration::from_hvcc(&base).unwrap();
    let nal = sei_nal(39, &[(5, vec![1; 16])]);
    base[22] += 1;
    base.push(39);
    base.extend(1_u16.to_be_bytes());
    base.extend(u16::try_from(nal.len()).unwrap().to_be_bytes());
    base.extend(&nal);
    let (with_sei, _) = Configuration::from_hvcc(&base).unwrap();
    assert!(!configuration.semantic_eq(&with_sei));
    let end = base.len();
    base[end - 2] ^= 1;
    let (changed, _) = Configuration::from_hvcc(&base).unwrap();
    assert!(!with_sei.semantic_eq(&changed));
    base[end - 1] = 0;
    assert!(Configuration::from_hvcc(&base).is_err());
}
#[test]
fn malformed_nal_headers_lengths_rbsp_and_multiple_pictures_fail_closed() {
    let (configuration, width) = Configuration::from_hvcc(&hvcc(8)).unwrap();
    let (slice, _) = idr_slice(true, 0, true);
    for mutate in [0, 1, 2, 3] {
        let mut nal = slice.clone();
        match mutate {
            0 => nal[0] |= 128,
            1 => nal[1] = 0,
            2 => nal[1] |= 8,
            _ => nal[0] = 22 << 1,
        }
        assert!(
            configuration
                .length_prefixed_access_unit(&packed(&[&nal], width), width, *b"hvc1")
                .is_err()
        );
    }
    for data in [
        vec![],
        vec![0, 0, 0, 0],
        vec![0, 0, 0, 3, 0x28, 1],
        vec![0, 0, 0, 5, 0x28, 1, 0, 0, 1],
    ] {
        assert!(
            configuration
                .length_prefixed_access_unit(&data, width, *b"hvc1")
                .is_err()
        );
    }
    let eos = escaped(36, &[128]);
    assert!(
        configuration
            .length_prefixed_access_unit(&packed(&[&slice, &eos, &eos], width), width, *b"hvc1")
            .is_err()
    );
    assert!(
        configuration
            .length_prefixed_access_unit(&packed(&[&slice, &eos, &slice], width), width, *b"hvc1")
            .is_err()
    );
}
#[test]
fn malformed_configuration_cannot_be_qualified_by_a_codec_tag() {
    let config = hvcc(8);
    for end in 0..config.len() {
        assert!(Configuration::from_hvcc(&config[..end]).is_err(), "{end}");
    }
    for (at, value) in [
        (0, 0),
        (13, 0),
        (15, 0),
        (16, 0),
        (17, 0),
        (18, 0),
        (21, 0x0e),
    ] {
        let mut bad = config.clone();
        bad[at] = value;
        assert!(Configuration::from_hvcc(&bad).is_err(), "{at}");
    }
    let mut extra = config;
    extra.push(0);
    assert!(Configuration::from_hvcc(&extra).is_err());
}
