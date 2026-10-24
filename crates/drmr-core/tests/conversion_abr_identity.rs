//! Equal decoded pictures do not make different compressed renditions interchangeable.
mod common;
use drmr_core::{
    concurrency::Stop,
    conversion::{
        Converter, HlsInputKeys, InputKeys, LocalTrack, TrackConversionRequest,
        VerificationRequest, VerificationSource,
    },
    events::{NoSink, Record, Sink},
    inspection::{OutputFormat, TargetProtection, TrackKind},
    mp4::{parse_boxes, walk},
    toolrun::{Secrets, ToolRunner},
};
use std::{
    path::Path,
    process::Command,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

const MARKER: &str = "drmr-rendition-B";
fn converter() -> Converter {
    Converter::new(
        Arc::new(ToolRunner::new(
            Arc::new(Secrets::default()),
            Arc::new(NoSink),
        )),
        "packager",
        "ffmpeg",
    )
    .with_decode(true)
}
fn run(command: &mut Command) -> Vec<u8> {
    let result = command.output().unwrap();
    assert!(
        result.status.success(),
        "fixture command failed: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    result.stdout
}
fn sei_value(nal: &[u8], at: &mut usize) -> usize {
    let mut value = 0_usize;
    loop {
        let byte = nal[*at];
        *at += 1;
        value = value.checked_add(usize::from(byte)).unwrap();
        if byte != 255 {
            return value;
        }
    }
}
fn mark_existing_user_data(path: &Path) {
    let original = std::fs::read(path).unwrap();
    let boxes = parse_boxes(&original).unwrap();
    let configs: Vec<_> = walk(&boxes)
        .into_iter()
        .filter(|b| b.kind() == *b"avcC")
        .collect();
    assert_eq!(configs.len(), 1);
    assert_eq!(configs[0].header.payload(&original)[4] & 3, 3);
    let mdat = boxes.iter().find(|b| b.kind() == *b"mdat").unwrap();
    let start = mdat.header.offset + mdat.header.header_size;
    let data = mdat.header.payload(&original);
    let size = usize::try_from(u32::from_be_bytes(data[..4].try_into().unwrap())).unwrap();
    let nal = &data[4..4 + size];
    assert_eq!(nal[0], 6);
    assert_eq!(nal.last(), Some(&0x80));
    // The existing SEI precedes the first IDR picture of this video-only fragment.
    let next = &data[4 + size..];
    let next_size = usize::try_from(u32::from_be_bytes(next[..4].try_into().unwrap())).unwrap();
    assert!(next_size > 0 && next_size <= next.len() - 4);
    assert_eq!(next[4] & 31, 5);
    // This fixture's SEI has no emulation-prevention bytes; RBSP offsets are byte offsets.
    assert!(!nal.windows(3).any(|bytes| bytes == [0, 0, 3]));
    let mut at = 1;
    assert_eq!(sei_value(nal, &mut at), 5);
    let payload_size = sei_value(nal, &mut at);
    assert_eq!(at + payload_size + 1, nal.len());
    assert!(payload_size >= 16 + MARKER.len());
    let uuid = &nal[at..at + 16];
    let begin = start + 4 + at + 16;
    let end = begin + MARKER.len();
    assert!(
        original[begin..end]
            .iter()
            .all(|b| b.is_ascii_graphic() || *b == b' ')
    );
    assert!(MARKER.bytes().all(|b| b.is_ascii_graphic()));
    let mut marked = original.clone();
    marked[begin..end].copy_from_slice(MARKER.as_bytes());
    assert_eq!(marked.len(), original.len());
    assert_eq!(&marked[..begin], &original[..begin]);
    assert_eq!(&marked[end..], &original[end..]);
    assert_eq!(&marked[start + 4 + at..start + 4 + at + 16], uuid);
    assert!(
        !marked[start + 4..start + 4 + size]
            .windows(3)
            .any(|bytes| bytes == [0, 0, 3])
    );
    assert_eq!(parse_boxes(&marked).unwrap(), boxes);
    std::fs::write(path, marked).unwrap();
}
fn video_tracks(root: &Path) -> [LocalTrack; 2] {
    let clip = Path::new(env!("CARGO_MANIFEST_DIR")).join("../drmr/tests/fixtures/bbb/clip.mp4");
    std::array::from_fn(|index| {
        let path = root.join(format!("video-{index}.mp4"));
        let mut command = Command::new("ffmpeg");
        command
            .args(["-v", "error", "-nostdin", "-threads", "1", "-i"])
            .arg(&clip)
            .args(["-map", "0:v:0", "-c", "copy"]);
        run(command
            .args(["-movflags", "+frag_keyframe+empty_moov"])
            .arg(&path));
        if index == 1 {
            mark_existing_user_data(&path);
        }
        LocalTrack {
            source: path,
            index: 0,
            kind: TrackKind::Video,
        }
    })
}
fn hash(path: &Path, copy: bool) -> Vec<u8> {
    let mut command = Command::new("ffmpeg");
    command
        .args(["-v", "error", "-nostdin", "-xerror", "-threads", "1", "-i"])
        .arg(path)
        .args(["-map", "0:v:0"]);
    if copy {
        command.args(["-c", "copy"]);
    } else {
        command.args(["-threads", "1", "-fps_mode", "passthrough"]);
    }
    run(command.args(["-f", "hash", "-hash", "sha256", "-"]))
}
fn reverse_variants(text: &str) -> String {
    let lines: Vec<_> = text.lines().filter(|l| !l.is_empty()).collect();
    let start = lines
        .iter()
        .position(|l| l.starts_with("#EXT-X-STREAM-INF:"))
        .unwrap();
    let (variants, _) = lines[start..].as_chunks::<2>();
    assert_eq!(variants.len(), 2);
    assert_eq!(lines.len() - start, 4);
    let mut reversed = lines[..start].join("\n");
    for variant in variants.iter().rev() {
        reversed.push('\n');
        reversed.push_str(&variant.join("\n"));
    }
    reversed.push('\n');
    reversed
}
fn verify(
    source: &Path,
    output: &Path,
) -> Result<drmr_core::conversion::VerificationReport, drmr_core::conversion::ConversionError> {
    let keys = InputKeys::default();
    let hls = HlsInputKeys::default();
    converter().verify(VerificationRequest {
        source: VerificationSource {
            input: source,
            input_keys: &keys,
            hls_keys: &hls,
        },
        output: VerificationSource {
            input: output,
            input_keys: &keys,
            hls_keys: &hls,
        },
    })
}
struct CancelOnProof {
    stop: Stop,
    started: AtomicUsize,
}
impl Sink for CancelOnProof {
    fn emit(&self, record: &Record) {
        if record.get("activity").and_then(|value| value.as_str()) == Some("verification")
            && record.get("state").and_then(|value| value.as_str()) == Some("started")
        {
            self.started.fetch_add(1, Ordering::SeqCst);
            self.stop.set();
        }
    }
}
fn assert_tie_cancellation_propagates(source: &Path, output: &Path) {
    let stop = Stop::new();
    let sink = Arc::new(CancelOnProof {
        stop: stop.clone(),
        started: AtomicUsize::new(0),
    });
    let keys = InputKeys::default();
    let hls = HlsInputKeys::default();
    let error = converter()
        .with_stop(stop)
        .with_activity_sink(sink.clone())
        .verify(VerificationRequest {
            source: VerificationSource {
                input: source,
                input_keys: &keys,
                hls_keys: &hls,
            },
            output: VerificationSource {
                input: output,
                input_keys: &keys,
                hls_keys: &hls,
            },
        })
        .unwrap_err();
    assert_eq!(error.0, "cancelled");
    assert_eq!(sink.started.load(Ordering::SeqCst), 1);
}
#[test]
fn identical_pictures_with_distinct_packets_verify_after_declaration_reordering() {
    let root = tempfile::tempdir().unwrap();
    let tracks = video_tracks(root.path());
    assert_eq!(
        hash(&tracks[0].source, false),
        hash(&tracks[1].source, false)
    );
    assert_ne!(hash(&tracks[0].source, true), hash(&tracks[1].source, true));
    let original = root.path().join("original");
    let keys = InputKeys::default();
    let converted = converter()
        .convert_tracks(TrackConversionRequest {
            tracks: &tracks,
            output: &original,
            format: OutputFormat::Hls,
            target: TargetProtection::Clear,
            input_keys: &keys,
            output_key: None,
            hls_key_uri: None,
            hls_output_key: None,
        })
        .unwrap();
    assert!(converted.verification.passed);
    let baseline = common::snapshot_tree(&original);
    let candidate = root.path().join("candidate");
    for (path, bytes) in &baseline {
        common::write(&candidate.join(path), bytes);
    }
    let source = original.join("master.m3u8");
    let result = candidate.join("master.m3u8");
    let text = std::fs::read_to_string(&result).unwrap();
    std::fs::write(&result, reverse_variants(&text)).unwrap();
    let verification = verify(&source, &result).unwrap();
    assert!(
        verification.passed
            && (verification.checks.is_pass()
                && verification.decode_evidence.satisfies(
                    drmr_core::conversion::ValidationDepth::Decoded,
                    verification.scope
                ))
    );
    assert!(verification.checks.stream_metadata.is_pass());
    assert!(
        verification.checks.packet_payloads_equal
            && verification.checks.packet_timing_equal
            && verification.decode_evidence.satisfies(
                drmr_core::conversion::ValidationDepth::Decoded,
                verification.scope
            )
    );
    assert_tie_cancellation_propagates(&source, &result);
    let (path, mut bytes) = common::snapshot_tree(&candidate)
        .into_iter()
        .find(|(path, bytes)| {
            Path::new(path)
                .extension()
                .and_then(|ext| ext.to_str())
                .is_some_and(|ext| ext.eq_ignore_ascii_case("m4s"))
                && bytes.windows(MARKER.len()).any(|b| b == MARKER.as_bytes())
        })
        .unwrap();
    let at = bytes
        .windows(MARKER.len())
        .position(|b| b == MARKER.as_bytes())
        .unwrap();
    bytes[at + MARKER.len() - 1] = b'C';
    std::fs::write(candidate.join(path), bytes).unwrap();
    assert_eq!(
        verify(&source, &result).unwrap_err().0,
        "presentation-track-content-mismatch"
    );
    assert_eq!(common::snapshot_tree(&original), baseline);
}
