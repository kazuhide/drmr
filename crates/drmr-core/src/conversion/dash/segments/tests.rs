use super::*;
use crate::{
    conversion::dash,
    events::NoSink,
    toolrun::{RunOptions, Secrets, ToolRunner},
};
use sha2::{Digest as _, Sha256};
use std::{collections::BTreeMap, io::Write as _, sync::Arc, time::Duration};

fn tool(program: &str, arguments: Vec<String>) {
    let runner = ToolRunner::new(Arc::new(Secrets::default()), Arc::new(NoSink));
    let argv = std::iter::once(program.to_owned())
        .chain(arguments)
        .collect::<Vec<_>>();
    let shown = [program.to_owned(), "[fixture arguments redacted]".into()];
    let result = runner
        .run_tool(
            &argv,
            program,
            &RunOptions {
                timeout: Some(Duration::from_secs(30)),
                argv_redacted: Some(&shown),
                log_stdout: false,
                max_output_chars: 0,
                ..RunOptions::default()
            },
        )
        .unwrap();
    assert_eq!(
        result.returncode,
        Some(0),
        "required fixture tool failed: {program}"
    );
}

fn fixture(directory: &Path, protection: &str) -> PathBuf {
    files::create_dir(directory).unwrap();
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("../drmr/tests/fixtures/bbb/clip.mp4");
    let transport = directory.join("source.ts");
    tool(
        "ffmpeg",
        vec![
            "-v".into(),
            "error".into(),
            "-nostdin".into(),
            "-threads".into(),
            "1".into(),
            "-i".into(),
            files::tool_path(&input).unwrap(),
            "-map".into(),
            "0:v:0".into(),
            "-map".into(),
            "0:a:0".into(),
            "-c:v".into(),
            "libx264".into(),
            "-preset".into(),
            "ultrafast".into(),
            "-threads:v".into(),
            "1".into(),
            "-g".into(),
            "240".into(),
            "-sc_threshold".into(),
            "0".into(),
            "-force_key_frames".into(),
            "0,1,4,7".into(),
            "-c:a".into(),
            "copy".into(),
            "-f".into(),
            "mpegts".into(),
            files::tool_path(&transport).unwrap(),
        ],
    );
    let mut argv = Vec::new();
    for role in ["video", "audio"] {
        let output = directory.join(role);
        files::create_dir(&output).unwrap();
        argv.push(format!(
            "input={},stream={role},init_segment={},segment_template={}",
            files::tool_path(&transport).unwrap(),
            files::tool_path(&output.join("init.mp4")).unwrap(),
            files::tool_path(&output.join("segment-$Number$.m4s")).unwrap()
        ));
    }
    let manifest = directory.join("manifest.mpd");
    argv.extend([
        "--segment_duration".into(),
        "2".into(),
        "--fragment_duration".into(),
        "1".into(),
        "--generate_static_live_mpd".into(),
        "--mp4_reset_initial_composition_offset_to_zero=false".into(),
        "--mpd_output".into(),
        files::tool_path(&manifest).unwrap(),
    ]);
    if protection != "clear" {
        argv.extend([
            "--enable_raw_key_encryption".into(),
            "--keys".into(),
            "label=:key_id=000102030405060708090a0b0c0d0e0f:key=101112131415161718191a1b1c1d1e1f"
                .into(),
            "--iv".into(),
            "303132333435363738393a3b3c3d3e3f".into(),
            "--protection_scheme".into(),
            protection.into(),
            "--clear_lead".into(),
            "0".into(),
        ]);
    }
    tool("packager", argv);
    dash::finish_transport(&manifest).unwrap();
    manifest
}

fn input_hashes(manifest: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    let mut paths = vec![manifest.to_path_buf()];
    let root = manifest.parent().unwrap();
    paths.extend(
        dash::source_references(&fs::read(manifest).unwrap())
            .unwrap()
            .into_iter()
            .map(|path| root.join(path)),
    );
    paths
        .into_iter()
        .map(|path| {
            let hash = Sha256::digest(fs::read(&path).unwrap()).to_vec();
            (path, hash)
        })
        .collect()
}

#[test]
fn no_join_retains_uneven_exact_timeline_pto_and_encrypted_initialization() {
    for (scheme, expected) in [
        ("clear", Protection::Clear),
        ("cenc", Protection::Cenc),
        ("cbcs", Protection::Cbcs),
    ] {
        let root = crate::native::private_tempdir().unwrap();
        let manifest = fixture(&root.path().join("source"), scheme);
        let before = input_hashes(&manifest);
        let stage = crate::conversion::files::Stage::new(None).unwrap();
        let result = dash::snapshot_segments(&manifest, stage.path(), &Stop::new()).unwrap();
        assert_eq!(result.representations.len(), 2);
        let legacy_stage = crate::conversion::files::Stage::new(None).unwrap();
        let legacy = dash::snapshot_dash(&manifest, legacy_stage.path()).unwrap();
        for (index, (representation, track)) in
            result.representations.iter().zip(&legacy).enumerate()
        {
            assert_eq!(representation.kind, track.kind);
            assert_eq!(representation.protection, expected);
            assert!(
                representation.window.origin.0 > 0,
                "fixture must exercise a nonzero PTO"
            );
            assert_eq!(representation.window.first.1, representation.window.end.1);
            assert!(representation.window.first.0 < representation.window.end.0);
            if representation.kind == TrackKind::Video {
                assert!(
                    representation
                        .resources
                        .windows(2)
                        .any(|p| p[0].1.duration != p[1].1.duration),
                    "fixture must have uneven segment lengths"
                );
            }
            let mut combined = fs::read(&representation.initialization).unwrap();
            let mut clock =
                crate::conversion::timing::FragmentClock::read(&representation.initialization)
                    .unwrap();
            for (resource, boundary) in &representation.resources {
                let (time, duration, scale) = clock.dash_segment(resource).unwrap();
                assert_eq!(
                    u128::from(time) * u128::from(boundary.timescale),
                    u128::from(boundary.time) * u128::from(scale)
                );
                assert_eq!(
                    u128::from(duration) * u128::from(boundary.timescale),
                    u128::from(boundary.duration) * u128::from(scale)
                );
                combined.extend(fs::read(resource).unwrap());
            }
            assert_eq!(combined, fs::read(&track.source).unwrap());
            assert!(
                !stage
                    .path()
                    .join(format!("dash-track-{index}.mp4"))
                    .exists()
            );
            assert!(!stage.path().join("dash-piece").exists());
        }
        assert_eq!(input_hashes(&manifest), before);
    }
}

#[test]
fn no_join_cancellation_and_foreign_files_are_preserved() {
    let root = crate::native::private_tempdir().unwrap();
    let stage = crate::conversion::files::Stage::new(None).unwrap();
    let stop = Stop::new();
    stop.set();
    let Err(error) = dash::snapshot_segments(&root.path().join("absent.mpd"), stage.path(), &stop)
    else {
        panic!("cancelled snapshot succeeded");
    };
    assert_eq!(error.0, "cancelled");
    assert_eq!(fs::read_dir(stage.path()).unwrap().count(), 0);
    let manifest = root.path().join("manifest.mpd");
    crate::native::create_private_new(&manifest)
        .unwrap()
        .write_all(b"<MPD><Period/></MPD>")
        .unwrap();
    let foreign = stage.path().join("dash-manifest.mpd");
    crate::native::create_private_new(&foreign)
        .unwrap()
        .write_all(b"foreign")
        .unwrap();
    assert!(dash::snapshot_segments(&manifest, stage.path(), &Stop::new()).is_err());
    assert_eq!(fs::read(&foreign).unwrap(), b"foreign");
    assert_eq!(fs::read(&manifest).unwrap(), b"<MPD><Period/></MPD>");
}

#[test]
fn snapshot_global_budgets_are_charged_before_oversized_resource_writes() {
    let root = crate::native::private_tempdir().unwrap();
    let source = root.path().join("source");
    crate::native::create_private_new(&source)
        .unwrap()
        .write_all(b"five!")
        .unwrap();
    let destination = root.path().join("destination");
    let mut budget = Budget {
        bytes: MAX_BYTES - 4,
        paths: 0,
        resources: 0,
    };
    assert_eq!(
        budget
            .snapshot(&source, &destination, &Stop::new())
            .unwrap_err()
            .0,
        "dash-media-size-limit"
    );
    assert_eq!(fs::metadata(&destination).unwrap().len(), 0);
    assert_eq!(fs::read(&source).unwrap(), b"five!");
    for (mut budget, expected) in [
        (
            Budget {
                bytes: 0,
                paths: 0,
                resources: MAX_SEGMENTS + MAX_TRACKS + 1,
            },
            "dash-segment-limit",
        ),
        (
            Budget {
                bytes: 0,
                paths: 16 * 1024 * 1024,
                resources: 0,
            },
            "source-graph-limit",
        ),
    ] {
        let destination = root.path().join(expected);
        assert_eq!(
            budget
                .snapshot(&source, &destination, &Stop::new())
                .unwrap_err()
                .0,
            expected
        );
        assert!(!destination.exists());
    }
}

#[test]
fn cooperative_copy_checks_cancellation_between_each_bounded_chunk() {
    struct CancelWriter<'a> {
        stop: &'a Stop,
        bytes: usize,
    }
    impl std::io::Write for CancelWriter<'_> {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.bytes += bytes.len();
            self.stop.set();
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let stop = Stop::new();
    let mut output = CancelWriter {
        stop: &stop,
        bytes: 0,
    };
    let mut input = std::io::Cursor::new(vec![0x55; 128 * 1024]);
    let mut budget = Budget::default();
    assert_eq!(
        budget
            .copy_chunks(&mut input, &mut output, &stop)
            .unwrap_err()
            .0,
        "cancelled"
    );
    assert_eq!(output.bytes, 64 * 1024);
    assert_eq!(input.position(), 64 * 1024);
    assert_eq!(budget.bytes, 64 * 1024);
}

#[test]
fn original_timing_and_protection_tampering_has_the_same_semantic_refusal() {
    let root = crate::native::private_tempdir().unwrap();
    let manifest = fixture(&root.path().join("source"), "cenc");
    let original = fs::read_to_string(&manifest).unwrap();
    let doc = roxmltree::Document::parse(&original).unwrap();
    let duration = doc
        .descendants()
        .find(|n| n.is_element() && n.tag_name().name() == "S")
        .unwrap()
        .attribute_node("d")
        .unwrap();
    let mut bad_timing = original.clone();
    bad_timing.replace_range(
        duration.range_value(),
        (duration.value().parse::<u64>().unwrap() + 1)
            .to_string()
            .as_str(),
    );
    let mut bad_end = original.clone();
    let end = doc
        .root_element()
        .attribute_node("mediaPresentationDuration")
        .unwrap();
    bad_end.replace_range(end.range_value(), "PT123S");
    let bad_protection = original.replacen("value=\"cenc\"", "value=\"cbcs\"", 1);
    assert_ne!(bad_protection, original);
    for (changed, allowed) in [
        (
            bad_timing,
            [
                "dash-timeline-gap-overlap-or-duration-not-supported",
                "manifest-fragment-timeline-mismatch",
            ]
            .as_slice(),
        ),
        (bad_end, ["dash-presentation-duration-mismatch"].as_slice()),
        (
            bad_protection,
            ["dash-media-protection-mismatch"].as_slice(),
        ),
    ] {
        crate::native::create_media_or_truncate(&manifest)
            .unwrap()
            .write_all(changed.as_bytes())
            .unwrap();
        let joined = files::Stage::new(None).unwrap();
        let legacy = dash::snapshot_dash(&manifest, joined.path()).unwrap_err();
        assert!(
            allowed.contains(&legacy.0),
            "unexpected refusal code: {}",
            legacy.0
        );
        let stage = files::Stage::new(None).unwrap();
        let Err(actual) = dash::snapshot_segments(&manifest, stage.path(), &Stop::new()) else {
            panic!("tampered input succeeded");
        };
        assert_eq!(actual, legacy);
    }
    crate::native::create_media_or_truncate(&manifest)
        .unwrap()
        .write_all(original.as_bytes())
        .unwrap();
}
