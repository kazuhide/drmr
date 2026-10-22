//! The native inner handler verifies privately; only its outer owner may publish.
use super::*;
use crate::{
    conversion::files::{self, Stage},
    events::NoSink,
    toolrun::{Secrets, ToolRunner},
};
use sha2::Digest as _;
use std::sync::Arc;

fn converter() -> Converter {
    Converter::new(
        Arc::new(ToolRunner::new(Arc::new(Secrets::new()), Arc::new(NoSink))),
        "packager",
        "ffmpeg",
    )
    .with_timeout(std::time::Duration::from_secs(30))
}

fn candidate(c: &Converter, parent: &Path, destination: &Path) -> Stage {
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("../drmr/tests/fixtures/bbb/clip.mp4");
    let fixture_bytes = fs::read(&fixture).unwrap();
    let source = parent.join("video.mp4");
    let created = std::process::Command::new("ffmpeg")
        .args(["-v", "error", "-nostdin", "-copyts", "-i"])
        .arg(&fixture)
        .args([
            "-map",
            "0:v:0",
            "-c",
            "copy",
            "-avoid_negative_ts",
            "disabled",
            "-movflags",
            "+frag_keyframe+empty_moov",
            "-use_editlist",
            "0",
        ])
        .arg(&source)
        .output()
        .unwrap();
    assert!(
        created.status.success(),
        "complete video fixture creation failed"
    );
    let packets = std::process::Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-count_packets",
            "-show_entries",
            "stream=nb_read_packets",
            "-of",
            "json",
        ])
        .arg(&source)
        .output()
        .unwrap();
    assert!(packets.status.success());
    let packets: serde_json::Value = serde_json::from_slice(&packets.stdout).unwrap();
    assert_eq!(packets["streams"].as_array().unwrap().len(), 1);
    assert_eq!(packets["streams"][0]["nb_read_packets"], "240");
    let original = fs::read(&source).unwrap();
    let tracks = [LocalTrack {
        source: source.clone(),
        kind: TrackKind::Video,
        index: 0,
    }];
    let stage = Stage::new(Some(parent)).unwrap();
    let report = c
        .convert_tracks_inner(
            TrackConversionRequest {
                tracks: &tracks,
                output: stage.path(),
                format: OutputFormat::Dash,
                target: TargetProtection::Clear,
                input_keys: &InputKeys::default(),
                output_key: None,
                hls_key_uri: None,
                hls_output_key: None,
            },
            Some(&stage),
            None,
            None,
        )
        .unwrap();
    assert!(report.verification.passed && report.verification.media_verified);
    assert!(report.verification.checks.stream_metadata.is_pass());
    assert!(report.verification.checks.packet_payloads_equal);
    assert!(report.verification.checks.packet_timing_equal);
    assert!(report.verification.checks.decoded_content_equal);
    assert!(!report.verification.license_playback_verified);
    assert_eq!(fs::read(source).unwrap(), original);
    assert_eq!(fs::read(fixture).unwrap(), fixture_bytes);
    assert!(
        !destination.exists(),
        "inner handler published before outer source verification"
    );
    assert!(stage.path().join("manifest.mpd").is_file());
    stage
}

fn snapshot(root: &Path) -> Vec<(PathBuf, [u8; 32])> {
    fn visit(root: &Path, directory: &Path, files: &mut Vec<(PathBuf, [u8; 32])>) {
        for entry in fs::read_dir(directory).unwrap() {
            let path = entry.unwrap().path();
            let metadata = path.symlink_metadata().unwrap();
            assert!(!metadata.is_symlink());
            if metadata.is_dir() {
                visit(root, &path, files);
            } else {
                assert!(metadata.is_file());
                files.push((
                    path.strip_prefix(root).unwrap().to_path_buf(),
                    sha2::Sha256::digest(fs::read(&path).unwrap()).into(),
                ));
            }
        }
    }
    let mut files = Vec::new();
    visit(root, root, &mut files);
    files.sort();
    files
}

#[test]
fn borrowed_native_candidate_waits_for_outer_publication_and_keeps_no_replace() {
    let parent = crate::native::private_tempdir().unwrap();
    let destination = parent.path().join("result");
    let c = converter();
    let stage = candidate(&c, parent.path(), &destination);
    let private_root = stage.private_root().to_path_buf();
    let source = fs::read(parent.path().join("video.mp4")).unwrap();
    let expected = snapshot(stage.path());
    files::create_dir(&destination).unwrap();
    files::create_new(&destination.join("sentinel"))
        .unwrap()
        .write_all(b"existing output")
        .unwrap();
    let existing = snapshot(&destination);
    assert_eq!(
        c.publish_directory(&stage, &destination).unwrap_err().0,
        "output-exists-or-publish-failed"
    );
    assert_eq!(snapshot(&destination), existing);
    assert_eq!(snapshot(stage.path()), expected);
    fs::remove_file(destination.join("sentinel")).unwrap();
    fs::remove_dir(&destination).unwrap();
    c.publish_directory(&stage, &destination).unwrap();
    drop(stage);
    assert!(!private_root.exists());
    assert_eq!(snapshot(&destination), expected);
    assert_eq!(fs::read(parent.path().join("video.mp4")).unwrap(), source);
}

#[test]
fn cancelled_or_failed_final_check_never_publishes_and_stage_drop_cleans_candidate() {
    let parent = crate::native::private_tempdir().unwrap();
    let destination = parent.path().join("result");
    let c = converter();
    let stage = candidate(&c, parent.path(), &destination);
    let private_root = stage.private_root().to_path_buf();
    let source = fs::read(parent.path().join("video.mp4")).unwrap();
    let expected = snapshot(stage.path());
    let stop = crate::concurrency::Stop::new();
    stop.set();
    assert_eq!(
        files::publish_directory_checked(stage.path(), &destination, Some(&stop), || Ok(()))
            .unwrap_err()
            .0,
        "cancelled"
    );
    assert!(!destination.exists());
    assert_eq!(snapshot(stage.path()), expected);
    assert_eq!(
        files::publish_directory_checked(stage.path(), &destination, None, || Err(
            ConversionError("download-source-changed")
        ))
        .unwrap_err()
        .0,
        "download-source-changed"
    );
    assert!(!destination.exists());
    assert_eq!(snapshot(stage.path()), expected);
    drop(stage);
    assert!(!private_root.exists());
    assert!(!destination.exists());
    assert_eq!(fs::read(parent.path().join("video.mp4")).unwrap(), source);
}
