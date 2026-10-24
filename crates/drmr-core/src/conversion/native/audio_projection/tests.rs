use super::*;
use crate::{
    concurrency::Stop,
    conversion::{
        backend::Backend,
        files,
        native::{ManifestAacClock, tests},
        ts,
        verify::StreamView,
    },
};
use std::path::{Path, PathBuf};

struct Fixture {
    _owner: files::Stage,
    original: PathBuf,
    raw: PathBuf,
    expected: AudioPresentation,
    clock: Option<ManifestAacClock>,
    backend: Backend,
}
impl Fixture {
    fn new() -> Self {
        let owner = files::Stage::new(None).unwrap();
        let source = tests::short_audio_transport(owner.path());
        let stop = Stop::new();
        let mut scanner = ts::NativeScanner::new();
        scanner.segment(&source, &stop).unwrap();
        let (_, tracks) = scanner.finish().unwrap();
        let track = tracks.values().next().unwrap();
        let (converter, _) = tests::converter();
        let demuxed = owner.path().join("demuxed.mp4");
        converter
            .backend
            .demux_transport(
                &source,
                crate::inspection::TrackKind::Audio,
                &demuxed,
                Some(track),
            )
            .unwrap();
        let clock = ManifestAacClock::from_transport(track).unwrap();
        let timing = track.timing().unwrap();
        let expected = AudioPresentation::new(
            audio_presentation::AudioEvidence {
                sample_rate: 44_100,
                coded_frames: timing.count,
                raw_first_pts: timing.first_pts,
                raw_coded_end: timing.end_pts,
            },
            timing.count * 1024 - 17,
        )
        .unwrap();
        let original = owner.path().join("original.mp4");
        crate::conversion::copy_private(&demuxed, &original).unwrap();
        audio_presentation::restore(&original, expected, &stop).unwrap();
        let raw = owner.path().join("raw.mp4");
        crate::conversion::copy_private(&original, &raw).unwrap();
        audio_presentation::strip_claim(&raw, expected.claim(), &stop).unwrap();
        Self {
            _owner: owner,
            original,
            raw,
            expected,
            clock,
            backend: converter.backend,
        }
    }
    fn compare(&self) -> Result<crate::conversion::VerificationReport, ConversionError> {
        self.backend.native_verify_audio_projection(
            StreamView::new(&self.original, 0, None).with_clock(self.clock),
            StreamView::new(&self.raw, 0, None).with_clock(self.clock),
            self.expected,
        )
    }
}
fn payload_offset(bytes: &[u8], kind: [u8; 4]) -> usize {
    let nodes = crate::mp4::parse_boxes(bytes).unwrap();
    let node = crate::mp4::walk(&nodes)
        .into_iter()
        .find(|node| node.kind() == kind)
        .unwrap();
    node.header.offset + node.header.header_size
}
fn write(path: &Path, bytes: &[u8]) {
    std::fs::write(path, bytes).unwrap();
}

#[test]
fn original_samples_and_controls_are_compared_with_the_separate_raw_projection() {
    let fixture = Fixture::new();
    assert!(fixture.compare().unwrap().passed);
    let original = std::fs::read(&fixture.original).unwrap();
    let raw = std::fs::read(&fixture.raw).unwrap();
    let mut changed = raw.clone();
    let payload = payload_offset(&changed, *b"mdat");
    changed[payload] ^= 1;
    write(&fixture.raw, &changed);
    let mismatch = fixture.compare().unwrap();
    assert!(!mismatch.passed && !mismatch.checks.packet_payloads_equal);
    assert!(mismatch.checks.packet_timing_equal);
    write(&fixture.raw, &raw);
    let mut changed = raw.clone();
    let header = payload_offset(&changed, *b"tkhd");
    assert_eq!(changed[header], 0);
    changed[header + 36..header + 38].copy_from_slice(&512_u16.to_be_bytes());
    write(&fixture.raw, &changed);
    let mismatch = fixture.compare().unwrap();
    assert!(!mismatch.passed && !mismatch.checks.stream_metadata.is_pass());
    assert!(mismatch.checks.packet_payloads_equal && mismatch.checks.packet_timing_equal);
    write(&fixture.raw, &raw);
    assert!(fixture.compare().unwrap().passed);
    assert_eq!(std::fs::read(&fixture.original).unwrap(), original);
}

#[test]
fn actual_claim_bytes_must_match_before_raw_interpretation() {
    let fixture = Fixture::new();
    let original = std::fs::read(&fixture.original).unwrap();
    let raw = std::fs::read(&fixture.raw).unwrap();
    let mut changed = original.clone();
    let at = payload_offset(&changed, *b"elst") + 8;
    let duration = u64::from_be_bytes(changed[at..at + 8].try_into().unwrap());
    changed[at..at + 8].copy_from_slice(&(duration + 1).to_be_bytes());
    write(&fixture.original, &changed);
    assert_eq!(
        fixture.compare().unwrap_err().0,
        "audio-presentation-range-not-supported"
    );
    let mut changed = original.clone();
    let at = payload_offset(&changed, *b"elst") + 44;
    changed[at..at + 2].copy_from_slice(&2_u16.to_be_bytes());
    write(&fixture.original, &changed);
    assert_eq!(
        fixture.compare().unwrap_err().0,
        "native-mp4-profile-not-supported"
    );
    write(&fixture.original, &original);
    let mut changed = original.clone();
    let at = payload_offset(&changed, *b"elst") + 28;
    let duration = u64::from_be_bytes(changed[at..at + 8].try_into().unwrap());
    changed[at..at + 8].copy_from_slice(&(duration + 100).to_be_bytes());
    write(&fixture.original, &changed);
    assert_eq!(fixture.compare().unwrap_err(), BAD);
    write(&fixture.original, &original);
    assert_eq!(
        fixture
            .backend
            .native_verify_audio_projection(
                StreamView::new(&fixture.raw, 0, None).with_clock(fixture.clock),
                StreamView::new(&fixture.raw, 0, None).with_clock(fixture.clock),
                fixture.expected
            )
            .unwrap_err(),
        BAD
    );
    assert_eq!(
        fixture
            .backend
            .native_validate_mp4(&fixture.original, 0, None)
            .unwrap_err()
            .0,
        "native-presentation-window-not-supported"
    );
    assert!(fixture.compare().unwrap().passed);
    assert_eq!(std::fs::read(&fixture.raw).unwrap(), raw);
}

fn absolute_fragment_base(bytes: &[u8]) -> Vec<u8> {
    let nodes = crate::mp4::parse_boxes(bytes).unwrap();
    let find = |kind| {
        crate::mp4::walk(&nodes)
            .into_iter()
            .find(|node| node.kind() == kind)
            .unwrap()
    };
    let moof = find(*b"moof");
    let traf = find(*b"traf");
    let tfhd = find(*b"tfhd");
    let trun = find(*b"trun");
    let mut changed = bytes.to_vec();
    for node in [moof, traf, tfhd] {
        let at = node.header.offset;
        let size = u32::from_be_bytes(changed[at..at + 4].try_into().unwrap());
        assert!(size > 1);
        changed[at..at + 4].copy_from_slice(&(size + 8).to_be_bytes());
    }
    let header = tfhd.header.offset + tfhd.header.header_size;
    assert_eq!(&changed[header..header + 4], &0x20000_u32.to_be_bytes());
    changed[header..header + 4].copy_from_slice(&1_u32.to_be_bytes());
    let offset = trun.header.offset + trun.header.header_size + 8;
    let old = u32::from_be_bytes(changed[offset..offset + 4].try_into().unwrap());
    changed[offset..offset + 4].copy_from_slice(&(old + 8).to_be_bytes());
    changed.splice(
        header + 8..header + 8,
        u64::try_from(moof.header.offset).unwrap().to_be_bytes(),
    );
    changed
}

#[test]
fn a_native_readable_fragment_still_needs_the_recognized_claim_profile() {
    let fixture = Fixture::new();
    let original = std::fs::read(&fixture.original).unwrap();
    let changed = absolute_fragment_base(&original);
    write(&fixture.original, &changed);
    let inspected = native_media::inspect(&fixture.original, &Stop::new()).unwrap();
    assert_eq!(
        inspected.tracks[0].sample_count,
        fixture.expected.evidence.coded_frames
    );
    assert_eq!(
        fixture.compare().unwrap_err().0,
        "audio-presentation-range-not-supported"
    );
    assert_eq!(std::fs::read(&fixture.original).unwrap(), changed);
    write(&fixture.original, &original);
    assert!(fixture.compare().unwrap().passed);
}
