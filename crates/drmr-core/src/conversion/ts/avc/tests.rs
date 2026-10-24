use super::*;

pub(super) fn bbb() -> (Vec<u8>, Vec<u8>) {
    (
        vec![
            0x67, 0x64, 0x00, 0x15, 0xac, 0x4c, 0x22, 0x0a, 0x0c, 0xfc, 0xf8, 0x08, 0x80, 0x00,
            0x00, 0x03, 0x00, 0x80, 0x00, 0x00, 0x18, 0x07, 0x8b, 0x17, 0x08, 0xc0,
        ],
        vec![0x68, 0xe8, 0x43, 0x04, 0x4b, 0x22, 0xc0],
    )
}

#[test]
fn original_bbb_sps_derives_coded_crop_colour_and_clock() {
    let (sps, pps) = bbb();
    let config = Configuration::parse(&sps, &pps).unwrap();
    let m = config.metadata();
    assert_eq!(
        (m.width, m.height, m.coded_width, m.coded_height),
        (320, 180, 320, 192)
    );
    assert_eq!(
        (m.chroma_format_idc, m.bit_depth_luma, m.bit_depth_chroma),
        (1, 8, 8)
    );
    assert_eq!((m.profile_idc, m.level_idc), (100, 21));
    assert!(m.frame_mbs_only);
    let clock = m.clock.unwrap();
    assert_eq!((clock.num_units_in_tick, clock.time_scale), (1, 48));
}

#[test]
fn eof_exp_golomb_emulation_prevention_and_trailing_bits_fail_closed() {
    let (sps, pps) = bbb();
    for n in 0..sps.len() {
        assert!(
            Configuration::parse(&sps[..n], &pps).is_err(),
            "SPS prefix {n}"
        );
    }
    for n in 0..pps.len() {
        assert!(
            Configuration::parse(&sps, &pps[..n]).is_err(),
            "PPS prefix {n}"
        );
    }
    for nal in [
        vec![0x67, 0, 0, 0],
        vec![0x67, 0, 0, 1],
        vec![0x67, 0, 0, 2],
        vec![0x67, 0, 0, 3],
        vec![0x67, 0, 0, 3, 4],
    ] {
        assert!(bits::rbsp(&nal, 7).is_err());
    }
    for value in 0..=3 {
        assert_eq!(
            &*bits::rbsp(&[0x67, 0, 0, 3, value], 7).unwrap(),
            &[0, 0, value]
        );
    }
    assert!(bits::Bits::new(&[0; 8]).ue(u32::MAX).is_err());
    assert!(bits::Bits::new(&[0x80, 0]).trailing().is_err());
    bits::Bits::new(&[0x80]).trailing().unwrap();
}

#[test]
fn avcc_reserved_fields_ids_and_extensions_are_bound_to_the_sps() {
    let (sps, pps) = bbb();
    let mut avcc = vec![1, sps[1], sps[2], sps[3], 0xff, 0xe1];
    avcc.extend(u16::try_from(sps.len()).unwrap().to_be_bytes());
    avcc.extend(&sps);
    avcc.push(1);
    avcc.extend(u16::try_from(pps.len()).unwrap().to_be_bytes());
    avcc.extend(&pps);
    avcc.extend([0xfd, 0xf8, 0xf8, 0]);
    assert_eq!(Configuration::from_avcc(&avcc).unwrap().1, 4);
    for (at, value) in [
        (1, 77),
        (2, 4),
        (4, 0xfe),
        (5, 0xc1),
        (avcc.len() - 4, 0xfe),
        (avcc.len() - 3, 0xf9),
        (avcc.len() - 1, 1),
    ] {
        let mut changed = avcc.clone();
        changed[at] = value;
        assert!(Configuration::from_avcc(&changed).is_err(), "byte {at}");
    }
    let mut sps = sps;
    sps[2] |= 1;
    assert!(Configuration::parse(&sps, &pps).is_err());
}

#[test]
fn avcc_high_bit_depth_extensions_and_all_valid_length_widths_are_supported() {
    for (data, chroma) in [
        (
            "016e000cffe1001a676e000ca6cd941419f9f011000003000100000300300f14299601000668ebe3cb22c0fdfafa00",
            1,
        ),
        (
            "017a000cffe1001a677a000cb6cd941419f8dc0440000003004000000c03c50a658001000668ebe3cb22c0fefafa00",
            2,
        ),
        (
            "01f4000cffe1001b67f4000c90d9b282833f1b80880000030008000003018078a14cb001000668ebe3c44844fffafa00",
            3,
        ),
    ] {
        let data = hex::decode(data).unwrap();
        let (config, width) = Configuration::from_avcc(&data).unwrap();
        assert_eq!(width, 4);
        assert_eq!(
            (
                config.metadata().bit_depth_luma,
                config.metadata().bit_depth_chroma,
                config.metadata().chroma_format_idc
            ),
            (10, 10, chroma)
        );
    }
    let (s, p) = parameter_sets(true, false, None, 0);
    let c = Configuration::parse(&s, &p).unwrap();
    let nal = header(true, false, 0, 0, 0);
    for width in [1, 2, 4] {
        let mut sample = Vec::new();
        sample.extend_from_slice(
            &u32::try_from(nal.len()).unwrap().to_be_bytes()[4 - usize::from(width)..],
        );
        sample.extend_from_slice(&nal);
        assert_eq!(
            c.length_prefixed_access_unit(&sample, width)
                .unwrap()
                .slice_count,
            1
        );
        assert!(
            c.length_prefixed_access_unit(&sample[..sample.len() - 1], width)
                .is_err()
        );
        assert!(
            c.length_prefixed_access_unit(&vec![0; usize::from(width)], width)
                .is_err()
        );
    }
}

#[test]
fn profile_chroma_and_bit_depth_constraints_are_not_only_bitstream_shapes() {
    for (data, invalid_profile) in [
        (
            "016e000cffe1001a676e000ca6cd941419f9f011000003000100000300300f14299601000668ebe3cb22c0fdfafa00",
            100,
        ),
        (
            "017a000cffe1001a677a000cb6cd941419f8dc0440000003004000000c03c50a658001000668ebe3cb22c0fefafa00",
            110,
        ),
        (
            "01f4000cffe1001b67f4000c90d9b282833f1b80880000030008000003018078a14cb001000668ebe3c44844fffafa00",
            122,
        ),
    ] {
        let data = hex::decode(data).unwrap();
        let (valid, _) = Configuration::from_avcc(&data).unwrap();
        let (sps, pps) = valid.parameter_sets();
        let mut changed = sps.to_vec();
        changed[1] = invalid_profile;
        assert!(Configuration::parse(&changed, pps).is_err());
    }
}

fn annex(data: &[u8]) -> Vec<&[u8]> {
    let mut marks = Vec::new();
    let mut at = 0;
    while at + 3 <= data.len() {
        let width = if data.get(at..at + 4) == Some(&[0, 0, 0, 1]) {
            4
        } else if data.get(at..at + 3) == Some(&[0, 0, 1]) {
            3
        } else {
            at += 1;
            continue;
        };
        marks.push((at, at + width));
        at += width;
    }
    marks
        .iter()
        .enumerate()
        .map(|(index, (_, begin))| {
            let mut end = marks.get(index + 1).map_or(data.len(), |m| m.0);
            while data[end - 1] == 0 {
                end -= 1;
            }
            &data[*begin..end]
        })
        .collect()
}

#[test]
fn complete_original_and_bframe_headers_support_the_existing_avc_profiles() {
    use std::{path::Path, process::Command};
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../drmr/tests/fixtures/bbb/clip.mp4");
    for (profile, pixels, bframes) in [
        ("baseline", "yuv420p", "0"),
        ("main", "yuv420p", "3"),
        ("high", "yuv420p", "3"),
        ("high10", "yuv420p10le", "3"),
        ("high422", "yuv422p10le", "3"),
        ("high444", "yuv444p10le", "3"),
        ("copy", "", "0"),
    ] {
        let mut ffmpeg = Command::new("ffmpeg");
        ffmpeg
            .args(["-v", "error", "-threads", "1", "-i"])
            .arg(&source)
            .args(["-map", "0:v:0", "-frames:v", "12"]);
        if profile == "copy" {
            ffmpeg.args(["-c:v", "copy"]);
        } else {
            ffmpeg.args([
                "-c:v",
                "libx264",
                "-threads",
                "1",
                "-profile:v",
                profile,
                "-pix_fmt",
                pixels,
                "-bf",
                bframes,
            ]);
        }
        let result = ffmpeg
            .args([
                "-bsf:v",
                "h264_mp4toannexb,h264_metadata=aud=insert",
                "-f",
                "h264",
                "pipe:1",
            ])
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{profile}: fixture generation failed"
        );
        let units = annex(&result.stdout);
        let sps = units
            .iter()
            .find(|u| u[0] & 31 == 7)
            .unwrap_or_else(|| panic!("{profile}: missing fixture SPS"));
        let pps = units
            .iter()
            .find(|u| u[0] & 31 == 8)
            .unwrap_or_else(|| panic!("{profile}: missing fixture PPS"));
        let config = Configuration::parse(sps, pps).unwrap_or_else(|e| panic!("{profile}: {e:?}"));
        assert_eq!(
            (config.metadata().width, config.metadata().height),
            (320, 180)
        );
        assert!(!config.metadata().clock.unwrap().fixed_frame_rate);
        let mut first = 0;
        let mut frames = 0;
        for end in (1..=units.len()).filter(|at| *at == units.len() || units[*at][0] & 31 == 9) {
            config
                .access_unit(&units[first..end])
                .unwrap_or_else(|e| panic!("{profile} frame {frames}: {e:?}"));
            frames += 1;
            first = end;
        }
        assert_eq!(frames, 12);
    }
}

#[derive(Default)]
struct Writer(Vec<bool>);
impl Writer {
    fn bits(&mut self, value: u32, width: u32) {
        for index in (0..width).rev() {
            self.0.push(value & (1 << index) != 0);
        }
    }
    fn flag(&mut self, value: bool) {
        self.0.push(value);
    }
    fn ue(&mut self, value: u32) {
        let code = u64::from(value) + 1;
        let width = 64 - code.leading_zeros();
        for _ in 1..width {
            self.flag(false);
        }
        for index in (0..width).rev() {
            self.flag(code & (1 << index) != 0);
        }
    }
    fn se(&mut self, value: i32) {
        self.ue(if value <= 0 {
            value.unsigned_abs() * 2
        } else {
            u32::try_from(value).unwrap() * 2 - 1
        });
    }
    fn bytes(mut self) -> Vec<u8> {
        self.flag(true);
        while !self.0.len().is_multiple_of(8) {
            self.flag(false);
        }
        self.0
            .as_chunks::<8>()
            .0
            .iter()
            .map(|chunk| {
                chunk
                    .iter()
                    .fold(0, |byte, bit| (byte << 1) | u8::from(*bit))
            })
            .collect()
    }
    fn nal(self, header: u8) -> Vec<u8> {
        let mut nal = vec![header];
        let mut zeros = 0;
        for byte in self.bytes() {
            if zeros == 2 && byte <= 3 {
                nal.push(3);
                zeros = 0;
            }
            nal.push(byte);
            zeros = if byte == 0 { zeros + 1 } else { 0 };
        }
        nal
    }
}
fn parameter_sets(
    frame_only: bool,
    pic_struct: bool,
    scaling: Option<i32>,
    pps_sps_id: u32,
) -> (Vec<u8>, Vec<u8>) {
    let mut s = Writer::default();
    s.bits(100, 8);
    s.bits(0, 8);
    s.bits(30, 8);
    s.ue(0);
    s.ue(1);
    s.ue(0);
    s.ue(0);
    s.flag(false);
    s.flag(scaling.is_some());
    if let Some(delta) = scaling {
        for index in 0..8 {
            s.flag(index == 0);
            if index == 0 {
                s.se(delta);
            }
        }
    }
    s.ue(0);
    s.ue(0);
    s.ue(0);
    s.ue(2);
    s.flag(false);
    s.ue(19);
    s.ue(if frame_only { 11 } else { 5 });
    s.flag(frame_only);
    if !frame_only {
        s.flag(false);
    }
    s.flag(true);
    s.flag(true);
    s.ue(0);
    s.ue(0);
    s.ue(0);
    s.ue(if frame_only { 6 } else { 3 });
    s.flag(true);
    s.flag(true);
    s.bits(1, 8);
    s.flag(false);
    s.flag(true);
    s.bits(5, 3);
    s.flag(false);
    s.flag(true);
    s.bits(1, 8);
    s.bits(1, 8);
    s.bits(1, 8);
    s.flag(true);
    s.ue(0);
    s.ue(0);
    s.flag(true);
    s.bits(1, 32);
    s.bits(48, 32);
    s.flag(false);
    s.flag(false);
    s.flag(false);
    s.flag(pic_struct);
    s.flag(false);
    let mut p = Writer::default();
    p.ue(0);
    p.ue(pps_sps_id);
    p.flag(false);
    p.flag(false);
    p.ue(0);
    p.ue(0);
    p.ue(0);
    p.flag(false);
    p.bits(0, 2);
    p.se(0);
    p.se(0);
    p.se(0);
    p.flag(true);
    p.flag(false);
    p.flag(false);
    (s.nal(0x67), p.nal(0x68))
}
fn header(frame_only: bool, field: bool, pps_id: u32, first_mb: u32, poc: u32) -> Vec<u8> {
    let mut s = Writer::default();
    s.ue(first_mb);
    s.ue(2);
    s.ue(pps_id);
    s.bits(0, 4);
    if !frame_only {
        s.flag(field);
        if field {
            s.flag(false);
        }
    }
    s.ue(0);
    s.bits(poc, 4);
    s.flag(false);
    s.flag(false);
    s.se(0);
    s.ue(0);
    s.se(0);
    s.se(0);
    s.bits(0xff, 8);
    s.nal(0x65)
}
#[test]
fn parameter_relationships_scaling_lists_and_every_picture_header_are_validated() {
    let (sps, pps) = parameter_sets(true, false, None, 0);
    let config = Configuration::parse(&sps, &pps).unwrap();
    assert_eq!(config.metadata().colour_primaries, Some(1));
    assert_eq!(config.metadata().sample_aspect_ratio, Some((1, 1)));
    let first_slice = header(true, false, 0, 0, 0);
    let second_slice = header(true, false, 0, 1, 0);
    assert_eq!(
        config
            .access_unit(&[&first_slice, &second_slice])
            .unwrap()
            .slice_count,
        2
    );
    config
        .access_unit(&[&[9, 0x10], &first_slice, &second_slice])
        .unwrap();
    assert!(
        config
            .access_unit(&[&[9, 0x70], &first_slice, &second_slice])
            .is_err()
    );
    for bad in [
        header(true, false, 1, 1, 0),
        header(true, false, 0, 1, 1),
        header(true, false, 0, 0, 0),
    ] {
        assert!(config.access_unit(&[&first_slice, &bad]).is_err());
    }
    let (_, pps) = parameter_sets(true, false, None, 1);
    assert!(Configuration::parse(&sps, &pps).is_err());
    for delta in [-8, -129, 128] {
        let (sps, pps) = parameter_sets(true, false, Some(delta), 0);
        assert_eq!(Configuration::parse(&sps, &pps).is_ok(), delta == -8);
    }
    let (sps, pps) = parameter_sets(false, false, None, 0);
    let config = Configuration::parse(&sps, &pps).unwrap();
    let frame = header(false, false, 0, 0, 0);
    let field = header(false, true, 0, 1, 0);
    assert!(config.access_unit(&[&frame]).is_ok());
    assert_eq!(
        config.access_unit(&[&frame, &field]).unwrap_err(),
        PROGRESSIVE
    );
}
#[test]
fn timing_sei_rejects_fields_duplicates_and_malformed_envelopes() {
    let (s, p) = parameter_sets(true, true, None, 0);
    let c = Configuration::parse(&s, &p).unwrap();
    let frame = header(true, false, 0, 0, 0);
    let recovery = [6, 6, 1, 0xc4, 0x80];
    let recovered = c.access_unit(&[&recovery, &frame]).unwrap();
    assert!(recovered.is_idr);
    assert_eq!(
        recovered.recovery_point,
        Some(RecoveryPoint {
            frame_count: 0,
            exact_match: true,
            broken_link: false,
            changing_slice_group_idc: 0
        })
    );
    assert!(c.access_unit(&[&recovery, &recovery, &frame]).is_err());
    let progressive = [6, 1, 1, 0x04, 0x80];
    let field = [6, 1, 1, 0x14, 0x80];
    assert_eq!(
        c.access_unit(&[&progressive, &frame])
            .unwrap()
            .picture_structure,
        Some(0)
    );
    assert_eq!(c.access_unit(&[&field, &frame]).unwrap_err(), PROGRESSIVE);
    assert!(
        c.access_unit(&[&progressive, &progressive, &frame])
            .is_err()
    );
    for repeat in [vec![6, 1, 1, 0x72, 0x80], vec![6, 1, 1, 0x81, 0x80]] {
        assert_eq!(
            c.access_unit(&[&repeat, &frame]).unwrap_err().0,
            "ts-avc-picture-duration-not-supported"
        );
    }
    for bad in [
        vec![6, 255, 0x80],
        vec![6, 1, 2, 0x04, 0x80],
        vec![6, 1, 1, 0x04],
        vec![6, 1, 1, 0x0c, 0x80],
    ] {
        assert!(c.access_unit(&[&bad, &frame]).is_err());
    }
}
