//! Check final VP9 cipher ranges without using the production range digest or writer.
use super::{TrackDescriptor, scan};
use crate::{
    concurrency::Stop,
    conversion::{
        Aes128Key, CheckOutcome, ContentKey, ConversionRequest, Converter, DecodeEvidence,
        HlsInputKeys, HlsOutputKey, InputKeys, OutputKey, ProcessingMode, SourceValidationPurpose,
        ValidationDepth, VerificationReport, VerificationRequest, VerificationSource,
        conformance::ManifestValidationTools,
        key_file,
        plan::{
            self,
            bundle::{BundleOutputIntent, BundlePlanRequest, ManifestValidation},
        },
        vp9,
    },
    events::Collector,
    inspection::{OutputFormat, TargetProtection},
    mp4::{self, Mp4Box},
    toolrun::{Secrets, ToolRunner},
};
use aes::cipher::{BlockCipherEncrypt as _, KeyInit as _};
use std::{
    collections::BTreeMap,
    fs,
    io::Write as _,
    ops::Range,
    path::{Path, PathBuf},
    process::Command,
    sync::Arc,
};

const KID: &str = "000102030405060708090a0b0c0d0e0f";
const KEY: &str = "101112131415161718191a1b1c1d1e1f";
const URI: &str = "https://operator.invalid/vp9-binding-key";
const LAYOUT: &str = "native-vp9-cenc-layout-not-supported";
const SEEDS: [&str; 6] = [
    "vp9-p0-8.mp4",
    "vp9-p1-8.mp4",
    "vp9-p2-10.mp4",
    "vp9-p2-12.mp4",
    "vp9-p3-10.mp4",
    "vp9-p3-12.mp4",
];

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../drmr/tests/fixtures/bbb")
        .join(name)
}

fn command(mode: ProcessingMode, packaging: bool) -> (Converter, Arc<Collector>) {
    let events = Arc::new(Collector::default());
    let runner = Arc::new(ToolRunner::new(Arc::new(Secrets::new()), events.clone()));
    (
        Converter::new(
            runner,
            if packaging {
                "packager"
            } else {
                "absent-packager"
            },
            "absent-ffmpeg",
        )
        .with_ffprobe("absent-ffprobe")
        .with_decode(false)
        .with_processing(mode),
        events,
    )
}

fn keys() -> InputKeys {
    InputKeys::new(vec![ContentKey::from_hex(KID, KEY).unwrap()]).unwrap()
}

fn source<'a>(
    input: &'a Path,
    keys: &'a InputKeys,
    hls: &'a HlsInputKeys,
) -> VerificationSource<'a> {
    VerificationSource {
        input,
        input_keys: keys,
        hls_keys: hls,
    }
}

#[track_caller]
fn proof(report: &VerificationReport) {
    assert!(
        report.passed && report.checks.is_pass(),
        "passed={} checks={:?} failure={:?}",
        report.passed,
        report.checks,
        report.failure
    );
    assert_eq!(report.required_depth, ValidationDepth::Native);
    assert_eq!(report.decode_evidence, DecodeEvidence::not_requested());
    assert!(!report.license_playback_verified);
}

fn tools(events: &Collector, packaging: bool) {
    for event in events.records() {
        if event.get("event").and_then(serde_json::Value::as_str) == Some("command") {
            assert!(
                packaging,
                "native admission or verification ran a media tool"
            );
            assert_eq!(
                event.get("tool").and_then(serde_json::Value::as_str),
                Some("packager")
            );
        }
        let text = serde_json::to_string(&event).unwrap();
        assert!(!text.contains(KEY) && !text.contains(URI));
    }
}

fn request<'a>(
    input: &'a Path,
    input_keys: &'a InputKeys,
    output: &'a Path,
    format: OutputFormat,
    hls: &'a HlsInputKeys,
    key: &'a OutputKey,
) -> ConversionRequest<'a> {
    ConversionRequest {
        input,
        output,
        format,
        target: TargetProtection::Cenc,
        input_keys,
        hls_input_keys: hls,
        output_key: Some(key),
        hls_key_uri: (format == OutputFormat::Hls).then_some(URI),
        hls_output_key: None,
    }
}

fn entry(output: &Path, format: OutputFormat) -> PathBuf {
    match format {
        OutputFormat::Mp4 => output.to_owned(),
        OutputFormat::Hls => output.join("master.m3u8"),
        OutputFormat::Dash => output.join("manifest.mpd"),
        OutputFormat::HlsTs => unreachable!(),
    }
}

fn comparison(
    input: &Path,
    input_keys: &InputKeys,
    output: &Path,
    output_keys: &InputKeys,
) -> VerificationReport {
    let (fresh, events) = command(ProcessingMode::Auto, false);
    let hls = HlsInputKeys::default();
    let report = fresh
        .verify(VerificationRequest {
            source: source(input, input_keys, &hls),
            output: source(output, output_keys, &hls),
        })
        .unwrap();
    tools(&events, false);
    report
}

#[track_caller]
fn verify_from(input: &Path, input_keys: &InputKeys, output: &Path, output_keys: &InputKeys) {
    proof(&comparison(input, input_keys, output, output_keys));
}

#[track_caller]
fn verify(clear: &Path, output: &Path, output_keys: &InputKeys) {
    verify_from(clear, &InputKeys::default(), output, output_keys);
}

fn assert_prepared_window_difference(original: &Path, prepared: &Path, prepared_keys: &InputKeys) {
    // Independent Packager preparation omits the progressive finite edit. The
    // complete presentation oracle must retain that observable difference.
    let report = comparison(original, &InputKeys::default(), prepared, prepared_keys);
    assert!(!report.passed);
    assert_eq!(report.checks.stream_metadata, CheckOutcome::Pass);
    assert!(report.checks.packet_payloads_equal);
    assert!(!report.checks.packet_timing_equal);
}

fn run(command: &mut Command) {
    let output = command.output().unwrap();
    assert!(
        output.status.success(),
        "independent fixture preparation failed"
    );
}

struct PlainVideo {
    track: TrackDescriptor,
    packets: Vec<Vec<u8>>,
    clocks: Vec<(i64, i64, u32)>,
}

fn plain_video(input: &Path) -> PlainVideo {
    let mut packets = Vec::new();
    let mut clocks = Vec::new();
    let mut pending = Vec::new();
    let movie = scan(
        input,
        &InputKeys::default(),
        &Stop::new(),
        &mut |track, info, bytes| {
            if track.codec == *b"vp09" {
                assert_eq!(pending.len(), usize::try_from(info.chunk_offset).unwrap());
                pending.extend_from_slice(bytes);
                if info.is_last_chunk {
                    assert_eq!(pending.len(), usize::try_from(info.size).unwrap());
                    packets.push(std::mem::take(&mut pending));
                    clocks.push((info.dts, info.pts, info.duration));
                }
            }
            Ok(())
        },
    )
    .unwrap();
    assert_eq!(pending, [] as [u8; 0]);
    let track = movie
        .tracks
        .into_iter()
        .find(|track| track.codec == *b"vp09")
        .unwrap();
    assert_eq!(packets.len(), 240);
    PlainVideo {
        track,
        packets,
        clocks,
    }
}

fn parser(video: &PlainVideo) -> vp9::Parser {
    vp9::Parser::new(
        vp9::Configuration::from_vpcc(&video.track.decoder_config).unwrap(),
        u32::from(video.track.sample_entry_metadata.width.unwrap()),
        u32::from(video.track.sample_entry_metadata.height.unwrap()),
    )
    .unwrap()
}

fn word(bytes: &[u8], offset: usize) -> u32 {
    u32::from_be_bytes(bytes[offset..offset + 4].try_into().unwrap())
}

fn wide(bytes: &[u8], offset: usize) -> u64 {
    u64::from_be_bytes(bytes[offset..offset + 8].try_into().unwrap())
}

fn child(nodes: &[Mp4Box], kind: [u8; 4]) -> &Mp4Box {
    let matching: Vec<_> = nodes.iter().filter(|node| node.kind() == kind).collect();
    assert_eq!(matching.len(), 1);
    matching[0]
}

fn descendant(node: &Mp4Box, kind: [u8; 4]) -> &Mp4Box {
    let matching: Vec<_> = mp4::walk(&node.children)
        .into_iter()
        .filter(|node| node.kind() == kind)
        .collect();
    assert_eq!(matching.len(), 1);
    matching[0]
}

#[derive(Clone, Copy)]
struct Defaults {
    duration: u32,
    size: u32,
    iv_size: usize,
    scale: u32,
}

fn initialization(bytes: &[u8], nodes: &[Mp4Box]) -> BTreeMap<u32, Defaults> {
    let Some(moov) = nodes.iter().find(|node| node.kind() == *b"moov") else {
        return BTreeMap::new();
    };
    let mut tracks = BTreeMap::new();
    for trak in moov.children.iter().filter(|node| node.kind() == *b"trak") {
        if !mp4::walk(&trak.children)
            .iter()
            .any(|node| node.kind() == *b"vpcC")
        {
            continue;
        }
        let tkhd = descendant(trak, *b"tkhd").header.payload(bytes);
        let id = word(tkhd, if tkhd[0] == 0 { 12 } else { 20 });
        let tenc = descendant(trak, *b"tenc").header.payload(bytes);
        let mdhd = descendant(trak, *b"mdhd").header.payload(bytes);
        assert_eq!(tenc[6], 1);
        assert!(matches!(tenc[7], 8 | 16));
        let trex = mp4::walk(&moov.children)
            .into_iter()
            .find(|node| node.kind() == *b"trex" && word(node.header.payload(bytes), 4) == id)
            .unwrap()
            .header
            .payload(bytes);
        tracks.insert(
            id,
            Defaults {
                duration: word(trex, 12),
                size: word(trex, 16),
                iv_size: usize::from(tenc[7]),
                scale: word(mdhd, if mdhd[0] == 0 { 12 } else { 20 }),
            },
        );
    }
    tracks
}

#[derive(Debug)]
struct EncodedSample {
    file: PathBuf,
    range: Range<usize>,
    dts: u64,
    pts: i64,
    duration: u32,
    scale: u32,
    raw: Vec<u8>,
    iv: Vec<u8>,
    spans: Vec<(usize, usize)>,
    fields: Vec<usize>,
}

fn files(path: &Path) -> Vec<PathBuf> {
    if path.is_file() {
        return vec![path.to_owned()];
    }
    let mut result = Vec::new();
    for item in fs::read_dir(path).unwrap() {
        let path = item.unwrap().path();
        if path.is_dir() {
            result.extend(files(&path));
        } else if matches!(
            path.extension().and_then(|part| part.to_str()),
            Some("mp4" | "m4s")
        ) {
            result.push(path);
        }
    }
    result.sort();
    result
}

fn auxiliary(bytes: &[u8], senc: &Mp4Box, samples: &mut [EncodedSample], iv_size: usize) {
    let payload = senc.header.payload(bytes);
    let flags = word(payload, 0);
    assert!(matches!(flags, 0 | 2));
    assert_eq!(usize::try_from(word(payload, 4)).unwrap(), samples.len());
    let mut at = 8;
    for sample in samples {
        sample.iv = payload[at..at + iv_size].to_vec();
        at += iv_size;
        if flags == 0 {
            sample.spans.push((0, sample.raw.len()));
        } else {
            let count = usize::from(u16::from_be_bytes(payload[at..at + 2].try_into().unwrap()));
            at += 2;
            for _ in 0..count {
                sample
                    .fields
                    .push(senc.header.offset + senc.header.header_size + at);
                sample.spans.push((
                    usize::from(u16::from_be_bytes(payload[at..at + 2].try_into().unwrap())),
                    usize::try_from(word(payload, at + 2)).unwrap(),
                ));
                at += 6;
            }
        }
        assert_eq!(
            sample
                .spans
                .iter()
                .map(|(clear, protected)| clear + protected)
                .sum::<usize>(),
            sample.raw.len()
        );
    }
    assert_eq!(at, payload.len());
}

fn fragment_samples(
    bytes: &[u8],
    moof: &Mp4Box,
    traf: &Mp4Box,
    defaults: Defaults,
    file: &Path,
) -> Vec<EncodedSample> {
    let tfhd = child(&traf.children, *b"tfhd").header.payload(bytes);
    let flags = word(tfhd, 0) & 0x00ff_ffff;
    let mut field = 8;
    let base = if flags & 1 != 0 {
        field += 8;
        usize::try_from(wide(tfhd, 8)).unwrap()
    } else {
        moof.header.offset
    };
    field += usize::from(flags & 2 != 0) * 4;
    let duration = if flags & 8 != 0 {
        field += 4;
        word(tfhd, field - 4)
    } else {
        defaults.duration
    };
    let size = if flags & 0x10 != 0 {
        word(tfhd, field)
    } else {
        defaults.size
    };
    let tfdt = child(&traf.children, *b"tfdt").header.payload(bytes);
    let mut dts = if tfdt[0] == 0 {
        u64::from(word(tfdt, 4))
    } else {
        wide(tfdt, 4)
    };
    let mut position = base;
    let mut result = Vec::new();
    for trun in traf.children.iter().filter(|node| node.kind() == *b"trun") {
        let payload = trun.header.payload(bytes);
        let flags = word(payload, 0) & 0x00ff_ffff;
        let count = word(payload, 4);
        let mut field = 8;
        if flags & 1 != 0 {
            let offset = i32::from_be_bytes(payload[8..12].try_into().unwrap());
            position = usize::try_from(i64::try_from(base).unwrap() + i64::from(offset)).unwrap();
            field += 4;
        }
        field += usize::from(flags & 4 != 0) * 4;
        for _ in 0..count {
            let duration = if flags & 0x100 != 0 {
                field += 4;
                word(payload, field - 4)
            } else {
                duration
            };
            let size = if flags & 0x200 != 0 {
                field += 4;
                word(payload, field - 4)
            } else {
                size
            };
            field += usize::from(flags & 0x400 != 0) * 4;
            let composition = if flags & 0x800 != 0 {
                field += 4;
                if payload[0] == 1 {
                    i64::from(i32::from_be_bytes(
                        payload[field - 4..field].try_into().unwrap(),
                    ))
                } else {
                    i64::from(word(payload, field - 4))
                }
            } else {
                0
            };
            let end = position + usize::try_from(size).unwrap();
            result.push(EncodedSample {
                file: file.to_owned(),
                range: position..end,
                dts,
                pts: i64::try_from(dts).unwrap() + composition,
                duration,
                scale: defaults.scale,
                raw: bytes[position..end].to_vec(),
                iv: Vec::new(),
                spans: Vec::new(),
                fields: Vec::new(),
            });
            position = end;
            dts += u64::from(duration);
        }
        assert_eq!(field, payload.len());
    }
    auxiliary(
        bytes,
        child(&traf.children, *b"senc"),
        &mut result,
        defaults.iv_size,
    );
    result
}

fn encoded(path: &Path) -> Vec<EncodedSample> {
    let files = files(path);
    let mut defaults = BTreeMap::new();
    for file in &files {
        let bytes = fs::read(file).unwrap();
        defaults.extend(initialization(&bytes, &mp4::parse_boxes(&bytes).unwrap()));
    }
    assert_eq!(
        defaults.len(),
        1,
        "one independently inspected VP9 track is required"
    );
    let mut result = Vec::new();
    for file in &files {
        let bytes = fs::read(file).unwrap();
        for moof in mp4::parse_boxes(&bytes)
            .unwrap()
            .iter()
            .filter(|node| node.kind() == *b"moof")
        {
            for traf in moof.children.iter().filter(|node| node.kind() == *b"traf") {
                let id = word(child(&traf.children, *b"tfhd").header.payload(&bytes), 4);
                if let Some(defaults) = defaults.get(&id) {
                    result.extend(fragment_samples(&bytes, moof, traf, *defaults, file));
                }
            }
        }
    }
    result.sort_by_key(|sample| sample.dts);
    assert_eq!(result.len(), 240);
    assert!(result.windows(2).all(|pair| pair[0].dts < pair[1].dts));
    result
}

// AES-CTR is symmetric. This scalar oracle consumes only the independently read SENC map.
fn cipher(bytes: &[u8], iv: &[u8], spans: &[(usize, usize)]) -> Vec<u8> {
    let key = hex::decode(KEY).unwrap();
    let aes = aes::Aes128::new_from_slice(&key).unwrap();
    let mut counter = [0; 16];
    counter[..iv.len()].copy_from_slice(iv);
    let mut counter = u128::from_be_bytes(counter);
    let mut stream = [0; 16];
    let mut used = 16;
    let mut result = bytes.to_vec();
    let mut position = 0;
    for &(clear, protected) in spans {
        position += clear;
        for byte in &mut result[position..position + protected] {
            if used == 16 {
                stream = counter.to_be_bytes();
                aes.encrypt_block((&mut stream).into());
                counter = counter.checked_add(1).unwrap();
                used = 0;
            }
            *byte ^= stream[used];
            used += 1;
        }
        position += protected;
    }
    assert_eq!(position, bytes.len());
    result
}

fn actual_mask(sample: &EncodedSample) -> Vec<bool> {
    let mut result = vec![false; sample.raw.len()];
    let mut at = 0;
    for &(clear, protected) in &sample.spans {
        at += clear;
        result[at..at + protected].fill(true);
        at += protected;
    }
    assert_eq!(at, result.len());
    result
}

fn expected_mask(sample: &vp9::Sample, size: usize, prefix: bool) -> Vec<bool> {
    // The VP Codec binding places alignment residue before each frame's cipher
    // bytes, independently of the other frames: https://www.webmproject.org/vp9/mp4/
    let mut result = vec![false; size];
    for frame in &sample.frames {
        let blocks = (frame.size - frame.header_bytes) / 16;
        let start = frame.offset
            + if prefix {
                frame.size - blocks * 16
            } else {
                frame.header_bytes
            };
        result[start..start + blocks * 16].fill(true);
    }
    result
}

fn assert_binding(video: &PlainVideo, output: &Path, prefix: bool) -> Vec<EncodedSample> {
    let records = encoded(output);
    let mut parser = parser(video);
    let mut frames = 0;
    let mut indexes = 0;
    for (ordinal, (record, clear)) in records.iter().zip(&video.packets).enumerate() {
        assert_eq!(record.scale, video.track.timescale);
        assert_eq!(
            (
                i64::try_from(record.dts).unwrap(),
                record.pts,
                record.duration
            ),
            video.clocks[ordinal],
            "raw packet clocks at sample {ordinal}"
        );
        let plaintext = cipher(&record.raw, &record.iv, &record.spans);
        assert!(
            plaintext == *clear,
            "independent plaintext sample {ordinal}"
        );
        let sample = parser.sample(clear).unwrap();
        assert!(
            actual_mask(record) == expected_mask(&sample, clear.len(), prefix),
            "cipher ranges at sample {ordinal}: {:?}",
            record.spans
        );
        for frame in &sample.frames {
            assert_eq!(
                &record.raw[frame.offset..frame.offset + frame.header_bytes],
                &clear[frame.offset..frame.offset + frame.header_bytes]
            );
        }
        assert_eq!(
            &record.raw[clear.len() - sample.index_bytes..],
            &clear[clear.len() - sample.index_bytes..]
        );
        frames += sample.frames.len();
        indexes += usize::from(sample.index_bytes != 0);
    }
    assert!(
        frames > 240 && indexes > 0,
        "all hidden frames and superframe indexes must be exercised"
    );
    records
}

fn assert_first_golden(video: &PlainVideo, records: &[EncodedSample], prefix: bool) {
    assert_eq!(video.packets[0].len(), 19_121);
    let parsed = parser(video).sample(&video.packets[0]).unwrap();
    assert_eq!(parsed.frames.len(), 1);
    assert_eq!(parsed.frames[0].header_bytes, 18);
    assert_eq!(parsed.index_bytes, 0);
    assert_eq!(
        records[0].spans,
        if prefix {
            vec![(33, 19_088)]
        } else {
            vec![(18, 19_088), (15, 0)]
        }
    );
}

fn initialization_codec(path: &Path) -> String {
    let bytes = fs::read(path).unwrap();
    let nodes = mp4::parse_boxes(&bytes).unwrap();
    let configurations: Vec<_> = mp4::walk(&nodes)
        .into_iter()
        .filter(|node| node.kind() == *b"vpcC")
        .collect();
    assert_eq!(configurations.len(), 1);
    let payload = configurations[0].header.payload(&bytes);
    vp9::Configuration::from_vpcc(payload).unwrap();
    // Form the declaration from stored fields rather than reusing the output formatter.
    let fields = [
        payload[4],
        payload[5],
        payload[6] >> 4,
        (payload[6] >> 1) & 7,
        payload[7],
        payload[8],
        payload[9],
        payload[6] & 1,
    ];
    let codec = format!(
        "vp09.{}",
        fields
            .iter()
            .map(|field| format!("{field:02}"))
            .collect::<Vec<_>>()
            .join(".")
    );
    assert_eq!(
        codec.split('.').count(),
        9,
        "the complete codec string is required"
    );
    codec
}

fn assert_hls_codecs(root: &Path) {
    let master = fs::read_to_string(root.join("master.m3u8")).unwrap();
    let mut advertised = None;
    let mut checked = 0;
    for line in master.lines().map(str::trim) {
        if let Some(value) = line.strip_prefix("#EXT-X-STREAM-INF:") {
            let attrs = crate::m3u8::parse_attributes(value).unwrap();
            assert!(
                advertised
                    .replace(attrs.get("CODECS").unwrap().to_owned())
                    .is_none()
            );
        } else if !line.is_empty() && !line.starts_with('#') {
            let playlist = root.join(line);
            let media = fs::read_to_string(&playlist).unwrap();
            let maps: Vec<_> = media
                .lines()
                .filter_map(|line| line.strip_prefix("#EXT-X-MAP:"))
                .collect();
            assert_eq!(maps.len(), 1);
            let attrs = crate::m3u8::parse_attributes(maps[0]).unwrap();
            let expected =
                initialization_codec(&playlist.parent().unwrap().join(attrs.get("URI").unwrap()));
            assert_eq!(advertised.take().unwrap(), expected);
            checked += 1;
        }
    }
    assert!(checked > 0 && advertised.is_none());
}

fn dash_initialization(root: &Path, representation: roxmltree::Node<'_, '_>) -> PathBuf {
    let mut ancestry: Vec<_> = representation
        .ancestors()
        .filter(roxmltree::Node::is_element)
        .collect();
    ancestry.reverse();
    let mut path = root.to_owned();
    for node in &ancestry {
        if let Some(base) = node
            .children()
            .find(|child| child.is_element() && child.tag_name().name() == "BaseURL")
        {
            path.push(base.text().unwrap().trim());
        }
    }
    let initialization = ancestry
        .iter()
        .rev()
        .find_map(|node| {
            node.children()
                .filter(roxmltree::Node::is_element)
                .find_map(|child| match child.tag_name().name() {
                    "SegmentTemplate" => child.attribute("initialization"),
                    "SegmentList" => child
                        .children()
                        .find(|node| {
                            node.is_element() && node.tag_name().name() == "Initialization"
                        })
                        .and_then(|node| node.attribute("sourceURL")),
                    _ => None,
                })
        })
        .unwrap();
    let initialization = initialization
        .replace(
            "$RepresentationID$",
            representation.attribute("id").unwrap(),
        )
        .replace(
            "$Bandwidth$",
            representation.attribute("bandwidth").unwrap(),
        );
    assert!(!initialization.contains('$'));
    path.push(initialization);
    path
}

fn assert_manifest_codecs(root: &Path, format: OutputFormat) {
    if format == OutputFormat::Hls {
        assert_hls_codecs(root);
        return;
    }
    assert_eq!(format, OutputFormat::Dash);
    let text = fs::read_to_string(root.join("manifest.mpd")).unwrap();
    let document = roxmltree::Document::parse(&text).unwrap();
    let mut checked = 0;
    for representation in document
        .descendants()
        .filter(|node| node.is_element() && node.tag_name().name() == "Representation")
    {
        let expected = initialization_codec(&dash_initialization(root, representation));
        let declarations: Vec<_> = representation
            .ancestors()
            .filter(|node| {
                node.is_element()
                    && matches!(node.tag_name().name(), "Representation" | "AdaptationSet")
            })
            .filter_map(|node| node.attribute("codecs"))
            .collect();
        assert_ne!(declarations, [] as [&str; 0]);
        for declaration in declarations {
            assert_eq!(declaration, expected);
        }
        checked += 1;
    }
    assert!(checked > 0);
}

#[test]
fn all_six_direct_mp4_outputs_have_independently_verified_prefix_senc_ranges() {
    let root = crate::native::private_tempdir().unwrap();
    let (converter, events) = command(ProcessingMode::Auto, true);
    let empty = InputKeys::default();
    let hls = HlsInputKeys::default();
    let key = OutputKey::from_hex(KID, KEY).unwrap();
    for name in SEEDS {
        let input = fixture(name);
        let original = fs::read(&input).unwrap();
        let video = plain_video(&input);
        let output = root.path().join(name);
        proof(
            &converter
                .convert(request(
                    &input,
                    &empty,
                    &output,
                    OutputFormat::Mp4,
                    &hls,
                    &key,
                ))
                .unwrap()
                .verification,
        );
        let records = assert_binding(&video, &output, true);
        if name == SEEDS[0] {
            assert_first_golden(&video, &records, true);
        }
        verify(&input, &output, &keys());
        assert_eq!(fs::read(input).unwrap(), original);
    }
    tools(&events, true);
}

fn prepare_manifest(input: &Path, root: &Path, format: OutputFormat) -> PathBuf {
    prepare_manifest_protection(input, root, format, false)
}

fn prepare_manifest_protection(
    input: &Path,
    root: &Path,
    format: OutputFormat,
    protected: bool,
) -> PathBuf {
    crate::native::create_media_dir_all(root).unwrap();
    let mut command = Command::new("packager");
    command.arg("--single_threaded").arg(format!(
        "input={},stream=video,init_segment={},segment_template={},playlist_name=video.m3u8{}",
        input.display(),
        root.join("init.mp4").display(),
        root.join("segment-$Number$.m4s").display(),
        if protected { ",drm_label=VIDEO" } else { "" },
    ));
    command.args([
        "--segment_duration",
        "2",
        "--mp4_reset_initial_composition_offset_to_zero=false",
    ]);
    if protected {
        command.args([
            "--enable_raw_key_encryption",
            "--keys",
            &format!("label=VIDEO:key_id={KID}:key={KEY}"),
            "--protection_scheme",
            "cenc",
            "--clear_lead",
            "0",
            "--hls_key_uri",
            URI,
        ]);
    }
    match format {
        OutputFormat::Hls => {
            command
                .arg("--hls_master_playlist_output")
                .arg(root.join("master.m3u8"));
        }
        OutputFormat::Dash => {
            command
                .arg("--generate_static_live_mpd")
                .arg("--mpd_output")
                .arg(root.join("manifest.mpd"));
        }
        _ => unreachable!(),
    }
    run(&mut command);
    entry(root, format)
}

fn drm_keys(root: &Path) -> key_file::OperatorKeys {
    let pssh = hex::encode(
        crate::playready::build_pssh_box(
            &crate::playready::WIDEVINE_SYSTEM_ID,
            &hex::decode(format!("1210{KID}")).unwrap(),
        )
        .unwrap(),
    );
    let path = root.join("operator.json");
    let document = serde_json::json!({"schema_version": 1, "output": {"kid": KID, "key": KEY, "hls_key_uri": URI, "signaling": {"system": "multi-drm", "widevine_pssh": pssh, "playready_license_url": "https://operator.invalid/vp9-binding-license"}}});
    crate::native::create_private_new(&path)
        .unwrap()
        .write_all(document.to_string().as_bytes())
        .unwrap();
    key_file::read(&path).unwrap()
}

fn snapshot(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    fn collect(root: &Path, directory: &Path, result: &mut BTreeMap<PathBuf, Vec<u8>>) {
        for item in fs::read_dir(directory).unwrap() {
            let path = item.unwrap().path();
            if path.is_dir() {
                collect(root, &path, result);
            } else {
                result.insert(
                    path.strip_prefix(root).unwrap().to_owned(),
                    fs::read(path).unwrap(),
                );
            }
        }
    }
    let mut result = BTreeMap::new();
    collect(root, root, &mut result);
    result
}

fn stitch_drm(
    input: &Path,
    input_keys: &InputKeys,
    video: &PlainVideo,
    output: &Path,
    format: OutputFormat,
    operator: &key_file::OperatorKeys,
) -> PathBuf {
    let (converter, events) = command(ProcessingMode::Stitch, true);
    let converter = converter.with_output_signaling(operator.signaling.clone());
    proof(
        &converter
            .convert(request(
                input,
                input_keys,
                output,
                format,
                &HlsInputKeys::default(),
                operator.output.as_ref().unwrap(),
            ))
            .unwrap()
            .verification,
    );
    assert_binding(video, output, true);
    assert_manifest_codecs(output, format);
    let final_entry = entry(output, format);
    let (fresh, fresh_events) = command(ProcessingMode::Auto, false);
    fresh
        .verify_output_signaling(&final_entry, format, TargetProtection::Cenc, operator)
        .unwrap();
    tools(&events, true);
    tools(&fresh_events, false);
    final_entry
}

#[test]
fn stitch_hls_and_dash_drm_outputs_keep_prefix_ranges_after_final_signaling() {
    let root = crate::native::private_tempdir().unwrap();
    let operator = drm_keys(root.path());
    let empty = InputKeys::default();
    for name in [SEEDS[0], SEEDS[2]] {
        let input = fixture(name);
        let original = fs::read(&input).unwrap();
        let video = plain_video(&input);
        for format in [OutputFormat::Hls, OutputFormat::Dash] {
            let prepared_root = root
                .path()
                .join(format!("source-{name}-{}", format.as_str()));
            let prepared = prepare_manifest(&input, &prepared_root, format);
            let before = snapshot(&prepared_root);
            assert_prepared_window_difference(&input, &prepared, &empty);
            let output = root
                .path()
                .join(format!("output-{name}-{}", format.as_str()));
            let final_entry = stitch_drm(&prepared, &empty, &video, &output, format, &operator);
            // Compare presentation meaning against the asset actually supplied;
            // the SENC/AES oracle separately checks the original seed's packets.
            verify_from(&prepared, &empty, &final_entry, &keys());
            if name == SEEDS[0] && format == OutputFormat::Hls {
                let protected_before = snapshot(&output);
                let rekeyed = root.path().join("protected-prepared-dash");
                let final_dash = stitch_drm(
                    &final_entry,
                    &keys(),
                    &video,
                    &rekeyed,
                    OutputFormat::Dash,
                    &operator,
                );
                verify_from(&final_entry, &keys(), &final_dash, &keys());
                assert_eq!(snapshot(&output), protected_before);
            }
            assert_eq!(snapshot(&prepared_root), before);
        }
        assert_eq!(fs::read(input).unwrap(), original);
    }
}

#[test]
fn joint_vp9_aac_mp4_has_canonical_video_ranges_and_fresh_native_proof() {
    let root = crate::native::private_tempdir().unwrap();
    let input = root.path().join("joint.mp4");
    run(Command::new("ffmpeg")
        .args(["-v", "error", "-nostdin", "-i"])
        .arg(fixture(SEEDS[0]))
        .arg("-i")
        .arg(fixture("clip.mp4"))
        .args([
            "-map",
            "0:v:0",
            "-map",
            "1:a:0",
            "-c",
            "copy",
            "-map_metadata",
            "-1",
            "-shortest",
        ])
        .arg(&input));
    let original = fs::read(&input).unwrap();
    let video = plain_video(&input);
    let (converter, events) = command(ProcessingMode::Auto, true);
    let output = root.path().join("joint-cenc.mp4");
    let empty = InputKeys::default();
    let hls = HlsInputKeys::default();
    let key = OutputKey::from_hex(KID, KEY).unwrap();
    proof(
        &converter
            .convert(request(
                &input,
                &empty,
                &output,
                OutputFormat::Mp4,
                &hls,
                &key,
            ))
            .unwrap()
            .verification,
    );
    let records = assert_binding(&video, &output, true);
    assert_first_golden(&video, &records, true);
    verify(&input, &output, &keys());
    tools(&events, true);
    assert_eq!(fs::read(input).unwrap(), original);
}

pub(in crate::conversion) fn prepare_tail(input: &Path, output: &Path) {
    run(Command::new("packager")
        .arg("--single_threaded")
        .arg(format!(
            "input={},stream=video,output={},drm_label=VIDEO",
            input.display(),
            output.display()
        ))
        .args([
            "--enable_raw_key_encryption",
            "--keys",
            &format!("label=VIDEO:key_id={KID}:key={KEY}"),
            "--protection_scheme",
            "cenc",
            "--clear_lead",
            "0",
            "--segment_duration",
            "2",
            "--mp4_reset_initial_composition_offset_to_zero=false",
        ]));
}

fn rewrite_record(bytes: &mut [u8], sample: &EncodedSample, spans: &[(usize, usize)]) {
    assert_eq!(spans.len(), sample.fields.len());
    let plaintext = cipher(&sample.raw, &sample.iv, &sample.spans);
    let changed = cipher(&plaintext, &sample.iv, spans);
    bytes[sample.range.clone()].copy_from_slice(&changed);
    for (&field, &(clear, protected)) in sample.fields.iter().zip(spans) {
        bytes[field..field + 2].copy_from_slice(&u16::try_from(clear).unwrap().to_be_bytes());
        bytes[field + 2..field + 6]
            .copy_from_slice(&u32::try_from(protected).unwrap().to_be_bytes());
    }
}

fn recipher_map(sample: &EncodedSample, spans: &[(usize, usize)], output: &Path) {
    let mut bytes = fs::read(&sample.file).unwrap();
    rewrite_record(&mut bytes, sample, spans);
    fs::write(output, bytes).unwrap();
}

fn spans_from_mask(mask: &[bool]) -> Vec<(usize, usize)> {
    let mut result = Vec::new();
    let mut at = 0;
    while at < mask.len() {
        let start = at;
        while at < mask.len() && !mask[at] {
            at += 1;
        }
        let protected = at;
        while at < mask.len() && mask[at] {
            at += 1;
        }
        result.push((protected - start, at - protected));
    }
    result
}

fn prepare_independent_prefix(tail: &Path, video: &PlainVideo, output: &Path) {
    let mut bytes = fs::read(tail).unwrap();
    let mut parser = parser(video);
    for (record, clear) in encoded(tail).iter().zip(&video.packets) {
        let sample = parser.sample(clear).unwrap();
        let mut spans = spans_from_mask(&expected_mask(&sample, clear.len(), true));
        assert!(spans.len() <= record.fields.len());
        spans.resize(record.fields.len(), (0, 0));
        rewrite_record(&mut bytes, record, &spans);
    }
    fs::write(output, bytes).unwrap();
}

fn assert_bad_source(input: &Path, output: &Path) {
    let before = fs::read(input).unwrap();
    let input_keys = keys();
    let hls = HlsInputKeys::default();
    let (converter, events) = command(ProcessingMode::Auto, false);
    for purpose in [
        SourceValidationPurpose::Conversion,
        SourceValidationPurpose::Verification,
    ] {
        assert_eq!(
            converter
                .validate_source(source(input, &input_keys, &hls), purpose)
                .unwrap_err()
                .0,
            LAYOUT
        );
    }
    let key = OutputKey::from_hex(KID, KEY).unwrap();
    assert_eq!(
        converter
            .convert(request(
                input,
                &input_keys,
                output,
                OutputFormat::Mp4,
                &hls,
                &key
            ))
            .unwrap_err()
            .0,
        LAYOUT
    );
    assert!(!output.exists());
    tools(&events, false);
    assert_eq!(fs::read(input).unwrap(), before);
}

fn assert_forbidden_ranges(tail: &Path, video: &PlainVideo, root: &Path) {
    let records = encoded(tail);
    let first = &records[0];
    for (label, protected) in [("full-sample", first.raw.len()), ("header", 16)] {
        let mut spans = vec![(0, 0); first.spans.len()];
        spans[0] = (0, protected);
        spans[1] = (first.raw.len() - protected, 0);
        let input = root.join(format!("bad-{label}.mp4"));
        recipher_map(first, &spans, &input);
        assert_eq!(
            cipher(&encoded(&input)[0].raw, &first.iv, &spans),
            video.packets[0]
        );
        assert_bad_source(&input, &root.join(format!("refused-{label}.mp4")));
    }
    let mut parser = parser(video);
    let (ordinal, _) = video
        .packets
        .iter()
        .enumerate()
        .find(|(_, bytes)| parser.sample(bytes).unwrap().index_bytes != 0)
        .unwrap();
    let indexed = &records[ordinal];
    let mut spans = vec![(0, 0); indexed.spans.len()];
    spans[0] = (indexed.raw.len() - 16, 16);
    let input = root.join("bad-index.mp4");
    recipher_map(indexed, &spans, &input);
    assert_eq!(
        cipher(&encoded(&input)[ordinal].raw, &indexed.iv, &spans),
        video.packets[ordinal]
    );
    assert_bad_source(&input, &root.join("refused-index.mp4"));
}

#[test]
fn shaka_tail_sources_require_explicit_repair_and_cannot_verify_as_final_outputs() {
    let root = crate::native::private_tempdir().unwrap();
    let input = fixture(SEEDS[0]);
    let original = fs::read(&input).unwrap();
    let video = plain_video(&input);
    let tail = root.path().join("shaka-tail.mp4");
    prepare_tail(&input, &tail);
    let tail_bytes = fs::read(&tail).unwrap();
    let records = assert_binding(&video, &tail, false);
    assert_first_golden(&video, &records, false);
    let (admission, events) = command(ProcessingMode::Auto, false);
    let input_keys = keys();
    let empty = InputKeys::default();
    let hls = HlsInputKeys::default();
    let qualified = admission
        .validate_source(
            source(&tail, &input_keys, &hls),
            SourceValidationPurpose::Conversion,
        )
        .unwrap();
    proof(&qualified.verification);
    assert_eq!(
        qualified
            .compatibility_notes
            .iter()
            .map(|note| note.as_str())
            .collect::<Vec<_>>(),
        ["vp9-cenc-prefix-normalization-required"]
    );
    assert_eq!(
        admission
            .validate_source(
                source(&tail, &input_keys, &hls),
                SourceValidationPurpose::Verification
            )
            .unwrap_err()
            .0,
        LAYOUT
    );
    assert_eq!(
        admission
            .verify(VerificationRequest {
                source: source(&input, &empty, &hls),
                output: source(&tail, &input_keys, &hls)
            })
            .unwrap_err()
            .0,
        LAYOUT
    );
    tools(&events, false);

    let (converter, events) = command(ProcessingMode::Auto, true);
    let repaired = root.path().join("repaired.mp4");
    let key = OutputKey::from_hex(KID, KEY).unwrap();
    proof(
        &converter
            .convert(request(
                &tail,
                &input_keys,
                &repaired,
                OutputFormat::Mp4,
                &hls,
                &key,
            ))
            .unwrap()
            .verification,
    );
    assert_binding(&video, &repaired, true);
    let (fresh, fresh_events) = command(ProcessingMode::Auto, false);
    proof(
        &fresh
            .verify(VerificationRequest {
                source: source(&tail, &input_keys, &hls),
                output: source(&repaired, &input_keys, &hls),
            })
            .unwrap(),
    );
    tools(&events, true);
    tools(&fresh_events, false);
    // Independently prepare an otherwise canonical source so each malformed map is
    // the sole refusal trigger, including a late superframe-index mutation.
    let independent = root.path().join("independent-prefix.mp4");
    prepare_independent_prefix(&tail, &video, &independent);
    assert_binding(&video, &independent, true);
    assert_prepared_window_difference(&input, &independent, &input_keys);
    verify_from(&tail, &input_keys, &independent, &input_keys);
    assert_forbidden_ranges(&independent, &video, root.path());
    assert_eq!(fs::read(&tail).unwrap(), tail_bytes);
    assert_eq!(fs::read(input).unwrap(), original);
}

fn outer_keys() -> (HlsOutputKey, HlsInputKeys) {
    (
        HlsOutputKey::new(URI, Aes128Key::from_hex(KEY).unwrap()).unwrap(),
        HlsInputKeys::new(vec![(URI.into(), Aes128Key::from_hex(KEY).unwrap())]).unwrap(),
    )
}

fn verify_outer(input: &Path, input_keys: &InputKeys, output: &Path, hls_keys: &HlsInputKeys) {
    let (fresh, events) = command(ProcessingMode::Auto, false);
    proof(
        &fresh
            .verify(VerificationRequest {
                source: source(input, input_keys, &HlsInputKeys::default()),
                output: source(&output.join("media.m3u8"), &InputKeys::default(), hls_keys),
            })
            .unwrap(),
    );
    tools(&events, false);
}

fn assert_outer_codec(output: &Path, expected: &str) {
    let master = fs::read_to_string(output.join("master.m3u8")).unwrap();
    let values: Vec<_> = master
        .lines()
        .filter_map(|line| line.strip_prefix("#EXT-X-STREAM-INF:"))
        .map(|line| {
            crate::m3u8::parse_attributes(line)
                .unwrap()
                .get("CODECS")
                .unwrap()
                .to_owned()
        })
        .collect();
    assert_eq!(values, [expected]);
}

#[test]
fn vp9_review_public_flat_aes128_preserves_declared_levels_and_full_codecs() {
    let root = crate::native::private_tempdir().unwrap();
    let seed = fixture(SEEDS[0]);
    let original = fs::read(&seed).unwrap();
    let nodes = mp4::parse_boxes(&original).unwrap();
    let config = mp4::walk(&nodes)
        .into_iter()
        .find(|node| node.kind() == *b"vpcC")
        .unwrap();
    let (key, hls_keys) = outer_keys();
    let empty = InputKeys::default();
    for level in [0, 20, 11] {
        let mut bytes = original.clone();
        bytes[config.header.offset + config.header.header_size + 5] = level;
        let input = root.path().join(format!("level-{level}.mp4"));
        fs::write(&input, &bytes).unwrap();
        let codec = initialization_codec(&input);
        for mode in [ProcessingMode::Auto, ProcessingMode::Stitch] {
            let (converter, events) = command(mode, true);
            let output = root.path().join(format!("flat-{level}-{mode:?}"));
            proof(
                &converter
                    .convert_hls_aes128(&input, &output, &empty, &key)
                    .unwrap()
                    .verification,
            );
            assert_outer_codec(&output, &codec);
            verify_outer(&input, &empty, &output, &hls_keys);
            let rekeyed = root.path().join(format!("flat-rekey-{level}-{mode:?}"));
            proof(
                &converter
                    .convert_hls_to_hls_aes128(
                        &output.join("media.m3u8"),
                        &rekeyed,
                        &empty,
                        &hls_keys,
                        &key,
                    )
                    .unwrap()
                    .verification,
            );
            assert_outer_codec(&rekeyed, &codec);
            verify_outer(&input, &empty, &rekeyed, &hls_keys);
            tools(&events, true);
        }
        assert_eq!(fs::read(input).unwrap(), bytes);
    }
    assert_eq!(fs::read(seed).unwrap(), original);
}

fn tail_manifest(root: &Path, format: OutputFormat) -> PathBuf {
    let input = fixture(SEEDS[0]);
    let entry = prepare_manifest_protection(&input, root, format, true);
    assert_binding(&plain_video(&input), root, false);
    entry
}

#[test]
fn vp9_review_direct_tail_hls_to_outer_aes128_uses_source_admission_only() {
    let root = crate::native::private_tempdir().unwrap();
    let input_root = root.path().join("source");
    let input = tail_manifest(&input_root, OutputFormat::Hls);
    let before = snapshot(&input_root);
    let (key, hls_keys) = outer_keys();
    for mode in [ProcessingMode::Auto, ProcessingMode::Stitch] {
        let (converter, events) = command(mode, true);
        let output = root.path().join(format!("outer-{mode:?}"));
        proof(
            &converter
                .convert_hls_to_hls_aes128(
                    &input_root.join("video.m3u8"),
                    &output,
                    &keys(),
                    &HlsInputKeys::default(),
                    &key,
                )
                .unwrap()
                .verification,
        );
        assert_outer_codec(&output, &initialization_codec(&input_root.join("init.mp4")));
        verify_outer(&input, &keys(), &output, &hls_keys);
        tools(&events, true);
    }
    assert_eq!(snapshot(&input_root), before);
}

#[test]
fn vp9_review_generic_tail_hls_and_dash_use_source_only_view_comparison() {
    let root = crate::native::private_tempdir().unwrap();
    let video = plain_video(&fixture(SEEDS[0]));
    let key = OutputKey::from_hex(KID, KEY).unwrap();
    for format in [OutputFormat::Hls, OutputFormat::Dash] {
        let input_root = root.path().join(format!("source-{}", format.as_str()));
        let input = tail_manifest(&input_root, format);
        let before = snapshot(&input_root);
        for mode in [ProcessingMode::Auto, ProcessingMode::Stitch] {
            let (converter, events) = command(mode, true);
            let output = root
                .path()
                .join(format!("result-{}-{mode:?}", format.as_str()));
            proof(
                &converter
                    .convert(request(
                        &input,
                        &keys(),
                        &output,
                        OutputFormat::Dash,
                        &HlsInputKeys::default(),
                        &key,
                    ))
                    .unwrap()
                    .verification,
            );
            assert_binding(&video, &output, true);
            assert_manifest_codecs(&output, OutputFormat::Dash);
            verify_from(
                &input,
                &keys(),
                &entry(&output, OutputFormat::Dash),
                &keys(),
            );
            tools(&events, true);
        }
        assert_eq!(snapshot(&input_root), before);
    }
}

#[test]
fn vp9_review_saved_bundle_verifies_tail_source_but_refuses_tail_output() {
    let root = crate::native::private_tempdir().unwrap();
    let input = root.path().join("tail.mp4");
    prepare_tail(&fixture(SEEDS[0]), &input);
    let original = fs::read(&input).unwrap();
    let output_root = root.path().join("bundle");
    let input_keys = keys();
    let hls = HlsInputKeys::default();
    let intents = [BundleOutputIntent {
        id: "mp4".into(),
        format: OutputFormat::Mp4,
        target: TargetProtection::Cenc,
        drms: Vec::new(),
        conformance: ManifestValidation::None,
        processing: None,
        dash_addressing: None,
    }];
    let (converter, events) = command(ProcessingMode::Auto, true);
    let plan = converter
        .create_bundle_plan(BundlePlanRequest {
            input: source(&input, &input_keys, &hls),
            output_root: &output_root,
            outputs: &intents,
        })
        .unwrap();
    let saved = root.path().join("bundle.json");
    plan::write_bundle(&saved, &plan).unwrap();
    let plan::SavedPlan::Bundle(reopened) = plan::read_saved(&saved).unwrap() else {
        panic!("bundle expected");
    };
    let mut operator = key_file::BundleOperatorKeys {
        input: keys(),
        ..Default::default()
    };
    let mut output_keys = key_file::OperatorKeys::default();
    output_keys.output = Some(OutputKey::from_hex(KID, KEY).unwrap());
    operator.outputs.insert("mp4".into(), output_keys);
    converter
        .execute_bundle_plan(&reopened, &operator, &ManifestValidationTools::default())
        .unwrap();
    let (fresh, fresh_events) = command(ProcessingMode::Auto, false);
    assert!(
        fresh
            .verify_bundle_plan(&reopened, &operator, &ManifestValidationTools::default())
            .unwrap()
            .passed
    );
    let output = output_root.join("mp4/media.mp4");
    assert_binding(&plain_video(&fixture(SEEDS[0])), &output, true);
    fs::write(&output, &original).unwrap();
    assert_eq!(
        fresh
            .verify_bundle_plan(&reopened, &operator, &ManifestValidationTools::default())
            .unwrap_err()
            .0,
        LAYOUT
    );
    assert_eq!(fs::read(&input).unwrap(), original);
    tools(&events, true);
    tools(&fresh_events, false);
}
