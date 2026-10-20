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
fn video_tracks(root: &Path) -> [LocalTrack; 2] {
    let clip = Path::new(env!("CARGO_MANIFEST_DIR")).join("../drmr/tests/fixtures/bbb/clip.mp4");
    std::array::from_fn(|index| {
        let path = root.join(format!("video-{index}.mp4"));
        let mut command = Command::new("ffmpeg");
        command
            .args(["-v", "error", "-nostdin", "-threads", "1", "-i"])
            .arg(&clip)
            .args(["-map", "0:v:0", "-c", "copy"]);
        if index == 1 {
            command.args([
                "-bsf:v",
                &format!(
                    "h264_metadata=sei_user_data=086f3693-b7b3-4f2c-a299-1fb58e87c381+{MARKER}"
                ),
            ]);
        }
        run(command
            .args(["-movflags", "+frag_keyframe+empty_moov"])
            .arg(&path));
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
    assert!(verification.passed && verification.media_verified);
    assert!(verification.checks.stream_metadata.is_pass());
    assert!(
        verification.checks.packet_payloads_equal
            && verification.checks.packet_timing_equal
            && verification.checks.decoded_content_equal
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
