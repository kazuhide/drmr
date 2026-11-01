use super::{tests::config, *};
#[derive(Default)]
struct Writer(Vec<bool>);
impl Writer {
    fn flag(&mut self, value: bool) {
        self.0.push(value);
    }
    fn u(&mut self, value: u32, width: u32) {
        for shift in (0..width).rev() {
            self.flag(value & (1 << shift) != 0);
        }
    }
    fn finish(mut self) -> Vec<u8> {
        while !self.0.len().is_multiple_of(8) {
            self.flag(false);
        }
        self.0
            .chunks(8)
            .map(|bits| {
                bits.iter()
                    .fold(0, |value, bit| (value << 1) | u8::from(*bit))
            })
            .collect()
    }
    fn prefix(&mut self, profile: u8) {
        self.u(2, 2);
        self.flag(profile & 1 != 0);
        self.flag(profile & 2 != 0);
        if profile == 3 {
            self.flag(false);
        }
    }
    fn dimensions(&mut self, width: u32, height: u32) {
        self.u(width - 1, 16);
        self.u(height - 1, 16);
        self.flag(false);
    }
    fn tail(self) -> (Vec<u8>, usize) {
        self.tail_mode(false)
    }
    fn tail_mode(mut self, resilient: bool) -> (Vec<u8>, usize) {
        if !resilient {
            self.flag(true);
            self.flag(true);
        }
        self.u(0, 2);
        self.u(0, 6);
        self.u(0, 3);
        self.flag(false);
        self.u(4, 8);
        self.flag(false);
        self.flag(false);
        self.flag(false);
        self.flag(false);
        self.flag(false);
        self.u(1, 16);
        let mut bytes = self.finish();
        let clear = bytes.len();
        bytes.push(0);
        bytes.extend([0x55; 64]);
        (bytes, clear)
    }
}
pub(super) fn key(
    profile: u8,
    depth: u8,
    chroma: u8,
    show: bool,
    width: u32,
    height: u32,
) -> (Vec<u8>, usize) {
    key_color(profile, depth, chroma, show, width, height, (0, false))
}
fn key_color(
    profile: u8,
    depth: u8,
    chroma: u8,
    show: bool,
    width: u32,
    height: u32,
    color: (u32, bool),
) -> (Vec<u8>, usize) {
    let (space, range) = color;
    let mut b = Writer::default();
    b.prefix(profile);
    b.flag(false);
    b.flag(false);
    b.flag(show);
    b.flag(false);
    b.u(0x0049_8342, 24);
    if profile >= 2 {
        b.flag(depth == 12);
    }
    b.u(space, 3);
    if space != 7 {
        b.flag(range);
    }
    if profile & 1 != 0 && space != 7 {
        b.flag(chroma == 2);
        b.flag(false);
        b.flag(false);
    }
    if profile & 1 != 0 && space == 7 {
        b.flag(false);
    }
    b.dimensions(width, height);
    b.tail()
}
fn inter(show: bool, slot: u32) -> (Vec<u8>, usize) {
    let mut b = Writer::default();
    b.prefix(0);
    b.flag(false);
    b.flag(true);
    b.flag(show);
    b.flag(false);
    if !show {
        b.flag(false);
    }
    b.u(0, 2);
    b.u(1, 8);
    for _ in 0..3 {
        b.u(slot, 3);
        b.flag(false);
    }
    b.flag(true);
    b.flag(false);
    b.flag(false);
    b.flag(true);
    b.tail()
}
fn intra_only(resilient: bool, reset: u32) -> Vec<u8> {
    let mut b = Writer::default();
    b.prefix(0);
    b.flag(false);
    b.flag(true);
    b.flag(false);
    b.flag(resilient);
    b.flag(true);
    if !resilient {
        b.u(reset, 2);
    }
    b.u(0x0049_8342, 24);
    b.u(1, 8);
    b.dimensions(128, 128);
    b.tail_mode(resilient).0
}
pub(super) fn existing(profile: u8, slot: u32, padding: usize) -> Vec<u8> {
    let mut b = Writer::default();
    b.prefix(profile);
    b.flag(true);
    b.u(slot, 3);
    let mut bytes = b.finish();
    bytes.resize(bytes.len() + padding, 0);
    bytes
}
pub(super) fn superframe(frames: &[&[u8]], magnitude: usize) -> Vec<u8> {
    let marker = 0xc0
        | (u8::try_from(magnitude - 1).unwrap() << 3)
        | (u8::try_from(frames.len()).unwrap() - 1);
    let mut bytes = Vec::new();
    for frame in frames {
        bytes.extend_from_slice(frame);
    }
    bytes.push(marker);
    for frame in frames {
        bytes.extend_from_slice(&u32::try_from(frame.len()).unwrap().to_le_bytes()[..magnitude]);
    }
    bytes.push(marker);
    bytes
}
fn parser() -> Parser {
    Parser::new(
        Configuration::from_vpcc(&super::test_configuration()).unwrap(),
        128,
        128,
    )
    .unwrap()
}
#[test]
fn one_frame_superframe_indexes_and_all_eight_reference_slots_are_real_state() {
    let mut parser = parser();
    let (frame, clear) = super::test_key_frame();
    let sample = parser
        .sample(&super::test_superframe(&[&frame], 1))
        .unwrap();
    assert_eq!(sample.frames.len(), 1);
    assert_eq!(sample.index_bytes, 3);
    assert_eq!(sample.frames[0].header_bytes, clear);
    assert!(sample.is_sync);
    for slot in 0..8 {
        let frame = existing(0, slot, 2);
        let sample = parser.sample(&frame).unwrap();
        assert_eq!(sample.displayed_pictures, 1);
        assert!(!sample.is_sync);
        assert!(sample.frames[0].show_existing);
        assert_eq!(sample.frames[0].header_bytes, frame.len());
    }
    let clear = super::test_show_existing_frame();
    let sample = parser.sample(&clear).unwrap();
    assert_eq!(sample.frames[0].header_bytes, clear.len());
}
#[test]
fn hidden_key_then_show_existing_is_independently_sync_and_preserves_both_spans() {
    let mut parser = parser();
    let (hidden, clear) = key(0, 8, 1, false, 128, 128);
    let shown = existing(0, 7, 0);
    let sample = parser.sample(&superframe(&[&hidden, &shown], 2)).unwrap();
    assert!(sample.is_sync);
    assert_eq!(sample.displayed_pictures, 1);
    assert_eq!(sample.frames.len(), 2);
    assert_eq!(sample.frames[0].offset, 0);
    assert_eq!(sample.frames[0].header_bytes, clear);
    assert!(!sample.frames[0].show_frame);
    assert_eq!(sample.frames[1].offset, hidden.len());
    assert!(sample.frames[1].show_frame);
    assert!(sample.frames[1].show_existing);
}
#[test]
fn hidden_inter_and_reference_sized_visible_inter_remain_one_mp4_sample() {
    let mut parser = parser();
    let (first, _) = key(0, 8, 1, true, 128, 128);
    parser.sample(&first).unwrap();
    let (hidden, _) = inter(false, 7);
    let (shown, _) = inter(true, 0);
    let sample = parser.sample(&superframe(&[&hidden, &shown], 1)).unwrap();
    assert!(!sample.is_sync);
    assert_eq!(sample.frames.len(), 2);
    assert_eq!(sample.displayed_pictures, 1);
    assert_eq!(
        (
            sample.width,
            sample.height,
            sample.render_width,
            sample.render_height
        ),
        (128, 128, 128, 128)
    );
}
#[test]
fn failure_latches_and_unknown_references_never_gain_synthetic_dimensions() {
    let mut parser = parser();
    assert!(parser.sample(&existing(0, 0, 0)).is_err());
    let (first, _) = key(0, 8, 1, true, 128, 128);
    assert!(parser.sample(&first).is_err());
    assert!(self::parser().sample(&inter(true, 0).0).is_err());
}
#[test]
fn intra_only_refresh_masks_and_frame_context_initialization_control_independence() {
    for (resilient, reset) in [(true, 0), (false, 2), (false, 3)] {
        let hidden = intra_only(resilient, reset);
        let shown = existing(0, 0, 0);
        let sample = parser().sample(&superframe(&[&hidden, &shown], 1)).unwrap();
        assert!(sample.is_sync);
        assert!(sample.frames[0].is_intra);
        assert!(!sample.frames[0].is_key);
    }
    let hidden = intra_only(true, 0);
    let absent = existing(0, 7, 0);
    assert_eq!(
        parser()
            .sample(&superframe(&[&hidden, &absent], 1))
            .unwrap_err()
            .0,
        "native-vp9-reference-frame-required"
    );
    let hidden = intra_only(false, 0);
    let shown = existing(0, 0, 0);
    assert_eq!(
        parser()
            .sample(&superframe(&[&hidden, &shown], 1))
            .unwrap_err()
            .0,
        "native-vp9-frame-context-required"
    );
}
#[test]
fn geometry_changes_and_zero_or_multiple_displayed_pictures_are_explicit_refusals() {
    let (frame, _) = key(0, 8, 1, true, 128, 128);
    let (smaller, _) = key(0, 8, 1, true, 64, 128);
    let mut parser = parser();
    parser.sample(&frame).unwrap();
    assert_eq!(
        parser.sample(&smaller).unwrap_err().0,
        "native-vp9-dynamic-dimensions-not-supported"
    );
    let mut parser = self::parser();
    let (hidden, _) = key(0, 8, 1, false, 128, 128);
    assert!(parser.sample(&hidden).is_err());
    let mut parser = self::parser();
    assert_eq!(
        parser
            .sample(&superframe(&[&frame, &frame], 1))
            .unwrap_err()
            .0,
        "native-vp9-display-count-not-supported"
    );
    let (too_large, _) = key(0, 8, 1, true, 256, 128);
    assert!(self::parser().sample(&too_large).is_err());
}
#[test]
fn valid_profiles_depths_and_chroma_are_cross_checked_against_every_key_header() {
    for (profile, depth, chroma) in [
        (0, 8, 0),
        (1, 8, 2),
        (1, 8, 3),
        (2, 10, 1),
        (2, 12, 1),
        (3, 10, 2),
        (3, 12, 3),
    ] {
        let mut parser = Parser::new(
            Configuration::from_vpcc(&config(profile, depth, chroma)).unwrap(),
            128,
            128,
        )
        .unwrap();
        let (frame, _) = key(profile, depth, chroma, true, 128, 128);
        assert!(parser.sample(&frame).unwrap().is_sync);
    }
    let (frame, _) = key(2, 12, 1, true, 128, 128);
    let mut parser = Parser::new(
        Configuration::from_vpcc(&config(2, 10, 1)).unwrap(),
        128,
        128,
    )
    .unwrap();
    assert!(parser.sample(&frame).is_err());
}
#[test]
fn rgb_profile_constraints_reserved_bits_and_zero_header_padding_are_checked() {
    for (profile, depth) in [(1, 8), (3, 10), (3, 12)] {
        let mut configuration = config(profile, depth, 3);
        configuration[6] |= 1;
        configuration[7] = 1;
        configuration[8] = 13;
        configuration[9] = 0;
        let (frame, _) = key_color(profile, depth, 3, true, 128, 128, (7, true));
        assert!(
            Parser::new(Configuration::from_vpcc(&configuration).unwrap(), 128, 128)
                .unwrap()
                .sample(&frame)
                .unwrap()
                .is_sync
        );
    }
    let (mut frame, clear) = key(1, 8, 3, true, 128, 128);
    frame[clear - 1] |= 1;
    let mut p = Parser::new(
        Configuration::from_vpcc(&config(1, 8, 3)).unwrap(),
        128,
        128,
    )
    .unwrap();
    assert!(p.sample(&frame).is_err());
    let (mut frame, _) = key(3, 10, 3, true, 128, 128);
    frame[0] |= 8;
    let mut p = Parser::new(
        Configuration::from_vpcc(&config(3, 10, 3)).unwrap(),
        128,
        128,
    )
    .unwrap();
    assert!(p.sample(&frame).is_err());
    let (mut frame, _) = key(1, 8, 3, true, 128, 128);
    frame[4] |= 2;
    let mut p = Parser::new(
        Configuration::from_vpcc(&config(1, 8, 3)).unwrap(),
        128,
        128,
    )
    .unwrap();
    assert!(p.sample(&frame).is_err());
}
#[test]
fn superframe_markers_magnitudes_lengths_and_size_coverage_are_all_checked() {
    use super::superframe::split;
    let frames: [&[u8]; 8] = [&[0x20]; 8];
    let sample = superframe(&frames, 4);
    let layout = split(&sample).unwrap();
    assert_eq!(layout.frames.len(), 8);
    assert_eq!(layout.index_bytes, 34);
    for magnitude in 1..=4 {
        let sample = superframe(&[&[0x20]], magnitude);
        let layout = split(&sample).unwrap();
        assert_eq!(layout.frames.len(), 1);
        assert_eq!(layout.index_bytes, magnitude + 2);
    }
    let source = superframe(&[&[1, 2], &[3, 4, 5]], 1);
    for (at, value) in [(5, 0), (6, 0), (7, 0), (8, 0xc0)] {
        let mut bytes = source.clone();
        bytes[at] = value;
        assert!(split(&bytes).is_err(), "{at}");
    }
    for bytes in [
        vec![0xc0],
        vec![0xc7, 0xc7],
        vec![0xc0, 0, 0xc0],
        vec![0xc0, 2, 0xc0],
    ] {
        assert!(split(&bytes).is_err());
    }
}
#[test]
fn every_coded_frame_tail_avoids_the_reserved_superframe_marker_pattern() {
    // Section 9.2.3 requires frame padding to keep each coded tail out of 0b110.
    // A valid outer index must not hide an invalid inner frame tail.
    for tail in [0xc0, 0xc7, 0xcf, 0xdf] {
        let (mut hidden, _) = key(0, 8, 1, false, 128, 128);
        *hidden.last_mut().unwrap() = tail;
        let shown = existing(0, 0, 0);
        let sample = superframe(&[&hidden, &shown], 2);
        assert_eq!(
            parser().sample(&sample).unwrap_err(),
            BAD,
            "tail {tail:02x}"
        );

        let (mut visible, _) = key(0, 8, 1, true, 128, 128);
        *visible.last_mut().unwrap() = tail;
        assert_eq!(
            parser().sample(&superframe(&[&visible], 2)).unwrap_err(),
            BAD,
            "single indexed frame tail {tail:02x}"
        );
    }
}
#[test]
fn malformed_headers_reserved_bits_sync_padding_and_partition_bounds_refuse() {
    let (source, clear) = key(0, 8, 1, true, 128, 128);
    for end in 0..=clear + 1 {
        assert!(parser().sample(&source[..end]).is_err(), "{end}");
    }
    for (at, value) in [(0, 0), (1, 0), (2, 0), (3, 0)] {
        let mut bytes = source.clone();
        bytes[at] = value;
        assert!(parser().sample(&bytes).is_err(), "{at}");
    }
    let mut bad_padding = existing(0, 0, 1);
    *bad_padding.last_mut().unwrap() = 1;
    let mut parser = parser();
    parser.sample(&source).unwrap();
    assert!(parser.sample(&bad_padding).is_err());
}
#[test]
fn large_coded_width_may_select_fewer_than_the_theoretical_maximum_tile_columns() {
    let mut b = Writer::default();
    b.prefix(0);
    b.flag(false);
    b.flag(false);
    b.flag(true);
    b.flag(false);
    b.u(0x0049_8342, 24);
    b.u(0, 3);
    b.flag(false);
    b.dimensions(32768, 128);
    b.flag(true);
    b.flag(true);
    b.u(0, 2);
    b.u(0, 6);
    b.u(0, 3);
    b.flag(false);
    b.u(4, 8);
    for _ in 0..3 {
        b.flag(false);
    }
    b.flag(false);
    // Eight columns satisfy the 4096 pixel maximum even though more columns are possible.
    b.flag(false);
    b.flag(false);
    b.u(1, 16);
    let mut bytes = b.finish();
    bytes.push(0);
    for _ in 0..7 {
        bytes.extend(1_u32.to_be_bytes());
        bytes.push(0x55);
    }
    bytes.push(0x55);
    let mut configuration = config(0, 8, 1);
    configuration[5] = 62;
    let sample = Parser::new(
        Configuration::from_vpcc(&configuration).unwrap(),
        32768,
        128,
    )
    .unwrap()
    .sample(&bytes)
    .unwrap();
    assert_eq!(sample.width, 32768);
    assert!(sample.is_sync);
    let mut zero_tile = bytes.clone();
    let header = sample.frames[0].header_bytes;
    zero_tile[header + 1..header + 5].fill(0);
    assert!(
        Parser::new(
            Configuration::from_vpcc(&configuration).unwrap(),
            32768,
            128
        )
        .unwrap()
        .sample(&zero_tile)
        .is_err()
    );
    let mut oversized_tile = bytes;
    oversized_tile[header + 1..header + 5].copy_from_slice(&u32::MAX.to_be_bytes());
    assert!(
        Parser::new(
            Configuration::from_vpcc(&configuration).unwrap(),
            32768,
            128
        )
        .unwrap()
        .sample(&oversized_tile)
        .is_err()
    );
}
