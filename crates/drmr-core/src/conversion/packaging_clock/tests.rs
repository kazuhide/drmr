use super::*;
use crate::{
    events::Collector,
    toolrun::{Secrets, ToolRunner},
};
use std::{fs, process::Command, sync::Arc};

fn converter() -> super::super::Converter {
    super::super::Converter::new(
        Arc::new(ToolRunner::new(
            Arc::new(Secrets::new()),
            Arc::new(Collector::default()),
        )),
        "packager",
        "must-not-run-ffmpeg",
    )
}

fn clear_fixture(owner: &files::Stage, rate: u32) -> PathBuf {
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../drmr/tests/fixtures/bbb/clip.mp4");
    let transport = owner.path().join("source.ts");
    let encoded = Command::new("ffmpeg")
        .args(["-v", "error", "-nostdin", "-i"])
        .arg(source)
        .args([
            "-map", "0:a:0", "-vn", "-c:a", "aac", "-threads", "1", "-ar",
        ])
        .arg(rate.to_string())
        .args([
            "-output_ts_offset",
            "10",
            "-mpegts_copyts",
            "1",
            "-muxdelay",
            "0",
            "-muxpreload",
            "0",
            "-f",
            "mpegts",
        ])
        .arg(&transport)
        .output()
        .unwrap();
    assert!(encoded.status.success(), "licensed fixture encoding failed");
    crate::native::seal_media_tree(owner.path()).unwrap();
    let mut scanner = super::super::ts::NativeScanner::new();
    scanner.segment(&transport, &Stop::new()).unwrap();
    let (_, native) = scanner.finish().unwrap();
    let anchor = native.values().next().unwrap();
    let command = converter();
    let demuxed = owner.path().join("demuxed.mp4");
    command
        .backend
        .demux_transport(&transport, TrackKind::Audio, &demuxed, Some(anchor))
        .unwrap();
    let clear = owner.path().join("clear.mp4");
    command
        .backend
        .package_stream(&demuxed, 0, &clear, None, None)
        .unwrap();
    clear
}

fn first_decode(path: &Path) -> u64 {
    let bytes = fs::read(path).unwrap();
    let nodes = crate::mp4::parse_boxes(&bytes).unwrap();
    let tfdt = crate::mp4::walk(&nodes)
        .into_iter()
        .find(|node| node.kind() == *b"tfdt")
        .unwrap();
    let payload = tfdt.header.payload(&bytes);
    if payload[0] == 0 {
        u64::from(u32::from_be_bytes(payload[4..8].try_into().unwrap()))
    } else {
        u64::from_be_bytes(payload[4..12].try_into().unwrap())
    }
}

#[test]
fn temporary_audio_origin_is_zero_and_drop_restores_every_original_byte() {
    for rate in [44_100, 48_000] {
        let owner = files::Stage::new(None).unwrap();
        let clear = clear_fixture(&owner, rate);
        let original = fs::read(&clear).unwrap();
        assert!(first_decode(&clear) > 0);
        let projected = prepare(
            &owner,
            std::slice::from_ref(&clear),
            &[TrackKind::Audio],
            &Stop::new(),
        )
        .unwrap();
        assert_eq!(first_decode(&clear), 0);
        drop(projected);
        assert_eq!(fs::read(&clear).unwrap(), original);
    }
}

fn package_audio(clear: &Path, output: &files::Stage, format: OutputFormat, scheme: &str) {
    package_tracks(&[clear.to_path_buf()], output, format, scheme);
}

fn package_tracks(clears: &[PathBuf], output: &files::Stage, format: OutputFormat, scheme: &str) {
    let mut command = Command::new("packager");
    for (ordinal, clear) in clears.iter().enumerate() {
        let directory = output.path().join(format!("track-{ordinal:04}"));
        files::create_dir(&directory).unwrap();
        command.arg(format!("in={},stream=0,init_segment={}/init.mp4,segment_template={}/seg_$Number$.m4s,playlist_name=track-{ordinal:04}/media.m3u8",clear.display(),directory.display(),directory.display()));
    }
    command.args([
        "--segment_duration",
        "6",
        "--fragment_duration",
        "6",
        "--mp4_reset_initial_composition_offset_to_zero=false",
    ]);
    if format == OutputFormat::Dash {
        command
            .arg("--generate_static_live_mpd")
            .arg("--mpd_output")
            .arg(output.path().join("manifest.mpd"));
    } else {
        command
            .args(["--hls_playlist_type", "VOD", "--hls_master_playlist_output"])
            .arg(output.path().join("master.m3u8"));
    }
    if scheme != "clear" {
        command.args([
            "--enable_raw_key_encryption",
            "--clear_lead",
            "0",
            "--protection_scheme",
            scheme,
            "--keys",
            "label=:key_id=000102030405060708090a0b0c0d0e0f:key=101112131415161718191a1b1c1d1e1f",
        ]);
    }
    if scheme == "cbcs" {
        if format == OutputFormat::Dash {
            command.args(["--protection_systems", "Widevine,PlayReady"]);
        } else {
            command.args([
                "--protection_systems",
                "FairPlay",
                "--hls_key_uri",
                "skd://example.invalid/test",
            ]);
        }
    }
    let result = command.output().unwrap();
    assert!(result.status.success(), "independent packaging failed");
    files::seal(output.path()).unwrap();
}

fn joined_output(output: &files::Stage, join: &Path) {
    joined_track(output, 0, join);
}

fn joined_track(output: &files::Stage, ordinal: usize, join: &Path) {
    let directory = output.path().join(format!("track-{ordinal:04}"));
    let mut bytes = fs::read(directory.join("init.mp4")).unwrap();
    let mut paths: Vec<_> = fs::read_dir(&directory)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "m4s"))
        .collect();
    paths.sort();
    for path in paths {
        bytes.extend(fs::read(path).unwrap());
    }
    fs::write(join, bytes).unwrap();
}

fn video_fixture(owner: &files::Stage) -> PathBuf {
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../drmr/tests/fixtures/bbb/clip.mp4");
    let transport = owner.path().join("video.ts");
    let result = Command::new("ffmpeg")
        .args(["-v", "error", "-nostdin", "-i"])
        .arg(source)
        .args([
            "-map",
            "0:v:0",
            "-an",
            "-vf",
            "scale=96:54",
            "-c:v",
            "libx264",
            "-preset",
            "veryfast",
            "-threads",
            "1",
            "-bf",
            "2",
            "-g",
            "144",
            "-keyint_min",
            "144",
            "-sc_threshold",
            "0",
            "-output_ts_offset",
            "10",
            "-mpegts_copyts",
            "1",
            "-muxdelay",
            "0",
            "-muxpreload",
            "0",
            "-f",
            "mpegts",
        ])
        .arg(&transport)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "licensed video fixture encoding failed"
    );
    files::seal(owner.path()).unwrap();
    let command = converter();
    let demuxed = owner.path().join("video-demuxed.mp4");
    command
        .backend
        .demux_transport(&transport, TrackKind::Video, &demuxed, None)
        .unwrap();
    let clear = owner.path().join("video-clear.mp4");
    command
        .backend
        .package_stream(&demuxed, 0, &clear, None, None)
        .unwrap();
    clear
}

fn track_bytes(output: &files::Stage, ordinal: usize) -> Vec<(PathBuf, Vec<u8>)> {
    let directory = output.path().join(format!("track-{ordinal:04}"));
    let mut files: Vec<_> = fs::read_dir(directory)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|v| v == "mp4" || v == "m4s"))
        .collect();
    files.sort();
    files
        .into_iter()
        .map(|path| {
            let bytes = fs::read(&path).unwrap();
            (path, bytes)
        })
        .collect()
}

#[test]
fn joint_video_and_audio_keep_absolute_clocks_and_the_shared_presentation_origin() {
    use super::super::{ContentKey, verify::StreamView};
    for rate in [44_100, 48_000] {
        let owner = files::Stage::new(None).unwrap();
        let audio = clear_fixture(&owner, rate);
        let video = video_fixture(&owner);
        let source_video = fs::read(&video).unwrap();
        let source_audio = fs::read(&audio).unwrap();
        let clears = [video.clone(), audio.clone()];
        let clock = super::super::native::ManifestAacClock::qualify_manifest(
            &super::super::native_media::inspect(&audio, &Stop::new()).unwrap(),
        )
        .unwrap();
        for format in [OutputFormat::Dash, OutputFormat::Hls] {
            for scheme in ["clear", "cbcs"] {
                let output = files::Stage::new(None).unwrap();
                let projected = prepare(
                    &owner,
                    &clears,
                    &[TrackKind::Video, TrackKind::Audio],
                    &Stop::new(),
                )
                .unwrap();
                assert_eq!(fs::read(&video).unwrap(), source_video);
                package_tracks(&clears, &output, format, scheme);
                let generated_video = track_bytes(&output, 0);
                let generated_audio_init =
                    fs::read(output.path().join("track-0001/init.mp4")).unwrap();
                projected
                    .restore(&output, output.path(), format, &Stop::new())
                    .unwrap();
                assert_eq!(track_bytes(&output, 0), generated_video);
                assert_eq!(
                    fs::read(output.path().join("track-0001/init.mp4")).unwrap(),
                    generated_audio_init
                );
                assert_eq!(fs::read(&video).unwrap(), source_video);
                assert_eq!(fs::read(&audio).unwrap(), source_audio);
                let key = ContentKey::from_hex(
                    "000102030405060708090a0b0c0d0e0f",
                    "101112131415161718191a1b1c1d1e1f",
                )
                .unwrap();
                for (ordinal, source) in clears.iter().enumerate() {
                    let joined = output.private_root().join(format!("joined-{ordinal}.mp4"));
                    joined_track(&output, ordinal, &joined);
                    let context = (ordinal == 1).then_some(clock).flatten();
                    let report = converter()
                        .backend
                        .native_verify_views(
                            StreamView::new(source, 0, None).with_clock(context),
                            StreamView::new(&joined, 0, (scheme != "clear").then_some(&key))
                                .with_clock(context),
                        )
                        .unwrap();
                    assert!(
                        report.passed,
                        "{rate}/{format:?}/{scheme}/track{ordinal}: {report:?}"
                    );
                }
                if format == OutputFormat::Dash {
                    let stage = files::Stage::new(None).unwrap();
                    let snapshot = super::super::dash::snapshot_segments(
                        &output.path().join("manifest.mpd"),
                        stage.path(),
                        &Stop::new(),
                    )
                    .unwrap();
                    assert_eq!(snapshot.representations.len(), 2);
                    assert_eq!(
                        snapshot.representations[0].window.origin,
                        snapshot.representations[1].window.origin
                    );
                    assert!(
                        snapshot
                            .representations
                            .iter()
                            .all(|rep| rep.resources.len() == 2)
                    );
                }
            }
        }
    }
}

#[test]
fn final_audio_payloads_and_absolute_clocks_are_exact_for_clear_and_encrypted_packaging() {
    use super::super::{ContentKey, verify::StreamView};
    for rate in [44_100, 48_000] {
        let owner = files::Stage::new(None).unwrap();
        let clear = clear_fixture(&owner, rate);
        let original = fs::read(&clear).unwrap();
        let clock = super::super::native::ManifestAacClock::qualify_manifest(
            &super::super::native_media::inspect(&clear, &Stop::new()).unwrap(),
        )
        .unwrap();
        for format in [OutputFormat::Dash, OutputFormat::Hls] {
            for scheme in ["clear", "cenc", "cbcs"] {
                let output = files::Stage::new(None).unwrap();
                let projected = prepare(
                    &owner,
                    std::slice::from_ref(&clear),
                    &[TrackKind::Audio],
                    &Stop::new(),
                )
                .unwrap();
                package_audio(&clear, &output, format, scheme);
                projected
                    .restore(&output, output.path(), format, &Stop::new())
                    .unwrap();
                assert_eq!(fs::read(&clear).unwrap(), original);
                let joined = output.private_root().join("joined.mp4");
                joined_output(&output, &joined);
                let key = ContentKey::from_hex(
                    "000102030405060708090a0b0c0d0e0f",
                    "101112131415161718191a1b1c1d1e1f",
                )
                .unwrap();
                let report = converter()
                    .backend
                    .native_verify_views(
                        StreamView::new(&clear, 0, None).with_clock(clock),
                        StreamView::new(&joined, 0, (scheme != "clear").then_some(&key))
                            .with_clock(clock),
                    )
                    .unwrap();
                assert!(report.passed, "{rate}/{format:?}/{scheme}: {report:?}");
                if format == OutputFormat::Dash {
                    let stage = files::Stage::new(None).unwrap();
                    let snapshot = super::super::dash::snapshot_segments(
                        &output.path().join("manifest.mpd"),
                        stage.path(),
                        &Stop::new(),
                    )
                    .unwrap();
                    assert_eq!(snapshot.representations.len(), 1);
                    assert_eq!(snapshot.representations[0].resources.len(), 2);
                    assert_eq!(
                        snapshot.representations[0].window.first.0 * 90_000
                            / snapshot.representations[0].window.first.1,
                        first_decode(&clear)
                    );
                }
            }
        }
    }
}

#[test]
fn unknown_fragment_flags_are_refused_before_any_source_is_modified() {
    let owner = files::Stage::new(None).unwrap();
    let first = clear_fixture(&owner, 44_100);
    let original = fs::read(&first).unwrap();
    let second = owner.path().join("invalid.mp4");
    let mut bytes = original.clone();
    let nodes = crate::mp4::parse_boxes(&bytes).unwrap();
    let header = crate::mp4::walk(&nodes)
        .into_iter()
        .find(|node| node.kind() == *b"tfhd")
        .unwrap()
        .header;
    bytes[header.offset + header.header_size + 1] |= 0x80;
    fs::write(&second, &bytes).unwrap();
    files::seal(owner.path()).unwrap();
    assert!(
        prepare(
            &owner,
            &[first.clone(), second.clone()],
            &[TrackKind::Audio; 2],
            &Stop::new()
        )
        .is_err()
    );
    assert_eq!(fs::read(first).unwrap(), original);
    assert_eq!(fs::read(second).unwrap(), bytes);
}

#[test]
fn canonical_terminal_audio_claim_survives_projection_and_packaging_exactly() {
    use super::super::{
        ContentKey,
        audio_presentation::{self, AudioPresentation},
        native_media, ts,
        verify::StreamView,
    };
    for rate in [44_100, 48_000] {
        let owner = files::Stage::new(None).unwrap();
        let clear = clear_fixture(&owner, rate);
        let clock = super::super::native::ManifestAacClock::qualify_manifest(
            &native_media::inspect(&clear, &Stop::new()).unwrap(),
        )
        .unwrap();
        let evidence = converter()
            .backend
            .native_audio_evidence(
                ts::View {
                    input: &clear,
                    metadata: &clear,
                    index: 0,
                    key: None,
                    all_keys: None,
                    transport: false,
                    native: None,
                    unedited: false,
                },
                clock,
            )
            .unwrap();
        let range =
            AudioPresentation::new(evidence, evidence.coded_samples().unwrap() - 784).unwrap();
        audio_presentation::restore(&clear, range, &Stop::new()).unwrap();
        let original = fs::read(&clear).unwrap();
        for format in [OutputFormat::Dash, OutputFormat::Hls] {
            let output = files::Stage::new(None).unwrap();
            let projected = prepare(
                &owner,
                std::slice::from_ref(&clear),
                &[TrackKind::Audio],
                &Stop::new(),
            )
            .unwrap();
            package_audio(&clear, &output, format, "cbcs");
            projected
                .restore(&output, output.path(), format, &Stop::new())
                .unwrap();
            assert_eq!(fs::read(&clear).unwrap(), original);
            let init = output.path().join("track-0000/init.mp4");
            audio_presentation::restore(&init, range, &Stop::new()).unwrap();
            if format == OutputFormat::Dash {
                super::super::dash::finish_transport(&output.path().join("manifest.mpd")).unwrap();
            } else {
                audio_presentation::finish_hls(output.path(), 1, &Stop::new()).unwrap();
            }
            assert_eq!(
                audio_presentation::read_claim(&init).unwrap(),
                Some(range.claim())
            );
            let joined = output.path().join("raw-output.mp4");
            joined_output(&output, &joined);
            files::seal(output.path()).unwrap();
            let raw = owner.path().join("raw-source.mp4");
            fs::write(&raw, &original).unwrap();
            files::seal(owner.path()).unwrap();
            audio_presentation::strip_claim(&raw, range.claim(), &Stop::new()).unwrap();
            audio_presentation::strip_claim(&joined, range.claim(), &Stop::new()).unwrap();
            let key = ContentKey::from_hex(
                "000102030405060708090a0b0c0d0e0f",
                "101112131415161718191a1b1c1d1e1f",
            )
            .unwrap();
            let report = converter()
                .backend
                .native_verify_views(
                    StreamView::new(&raw, 0, None).with_clock(clock),
                    StreamView::new(&joined, 0, Some(&key)).with_clock(clock),
                )
                .unwrap();
            assert!(report.passed, "{rate}/{format:?}: {report:?}");
            let actual = converter()
                .backend
                .native_audio_evidence(
                    ts::View {
                        input: &joined,
                        metadata: &joined,
                        index: 0,
                        key: Some(&key),
                        all_keys: None,
                        transport: false,
                        native: None,
                        unedited: false,
                    },
                    clock,
                )
                .unwrap();
            let rebound = range.claim().bind(actual).unwrap();
            assert_eq!(rebound, range);
        }
    }
}

#[test]
fn cancellation_and_invalid_ownership_never_leave_the_source_projected() {
    let owner = files::Stage::new(None).unwrap();
    let clear = clear_fixture(&owner, 44_100);
    let original = fs::read(&clear).unwrap();
    let other = files::Stage::new(None).unwrap();
    assert!(
        prepare(
            &other,
            std::slice::from_ref(&clear),
            &[TrackKind::Audio],
            &Stop::new()
        )
        .is_err()
    );
    assert!(
        prepare(
            &owner,
            &[clear.clone(), clear.clone()],
            &[TrackKind::Audio; 2],
            &Stop::new()
        )
        .is_err()
    );
    let stop = Stop::new();
    stop.set();
    assert_eq!(
        prepare(
            &owner,
            std::slice::from_ref(&clear),
            &[TrackKind::Audio],
            &stop
        )
        .err()
        .unwrap()
        .0,
        "cancelled"
    );
    assert_eq!(fs::read(&clear).unwrap(), original);
    let projected = prepare(
        &owner,
        std::slice::from_ref(&clear),
        &[TrackKind::Audio],
        &Stop::new(),
    )
    .unwrap();
    assert_eq!(first_decode(&clear), 0);
    assert_eq!(
        projected
            .restore(&other, other.path(), OutputFormat::Dash, &stop)
            .unwrap_err()
            .0,
        "cancelled"
    );
    assert_eq!(fs::read(clear).unwrap(), original);
}

#[cfg(unix)]
#[test]
fn symbolic_links_hard_links_and_replaced_stage_files_are_never_updated() {
    use std::os::unix::fs::symlink;
    let owner = files::Stage::new(None).unwrap();
    let clear = clear_fixture(&owner, 48_000);
    let original = fs::read(&clear).unwrap();
    let alias = owner.path().join("alias.mp4");
    symlink(&clear, &alias).unwrap();
    assert!(
        prepare(
            &owner,
            std::slice::from_ref(&alias),
            &[TrackKind::Audio],
            &Stop::new()
        )
        .is_err()
    );
    fs::remove_file(&alias).unwrap();
    fs::hard_link(&clear, &alias).unwrap();
    assert!(
        prepare(
            &owner,
            std::slice::from_ref(&clear),
            &[TrackKind::Audio],
            &Stop::new()
        )
        .is_err()
    );
    assert_eq!(fs::read(&clear).unwrap(), original);
    fs::remove_file(alias).unwrap();
    let projected = prepare(
        &owner,
        std::slice::from_ref(&clear),
        &[TrackKind::Audio],
        &Stop::new(),
    )
    .unwrap();
    let parked = owner.path().join("parked.mp4");
    fs::rename(&clear, &parked).unwrap();
    fs::write(&clear, &original).unwrap();
    files::seal(owner.path()).unwrap();
    drop(projected);
    assert_eq!(fs::read(clear).unwrap(), original);
    assert_eq!(first_decode(&parked), 0);
}

#[test]
fn an_unrepresentable_sidx_origin_becomes_free_without_changing_media_or_box_offsets() {
    let owner = files::Stage::new(None).unwrap();
    let clear = clear_fixture(&owner, 48_000);
    let output = files::Stage::new(None).unwrap();
    package_audio(&clear, &output, OutputFormat::Dash, "cbcs");
    let init = owned::BoundFile::open(&output, &output.path().join("track-0000/init.mp4")).unwrap();
    let mut budget = metadata::Budget::default();
    let track = metadata::initialization(&init, &mut budget, &Stop::new()).unwrap();
    let path = output.path().join("track-0000/seg_1.m4s");
    let mut original = fs::read(&path).unwrap();
    let nodes = crate::mp4::parse_boxes(&original).unwrap();
    let sidx = nodes
        .iter()
        .find(|node| node.kind() == *b"sidx")
        .unwrap()
        .header;
    assert_eq!(sidx.payload(&original)[0], 0);
    // A distinct, exactly related index timescale can overflow before the media clock does.
    let start = sidx.offset + sidx.header_size;
    for offset in [8, 12, 28] {
        let at = start + offset;
        let value = u32::from_be_bytes(original[at..at + 4].try_into().unwrap());
        original[at..at + 4].copy_from_slice(&value.checked_mul(2).unwrap().to_be_bytes());
    }
    fs::write(&path, &original).unwrap();
    let mut file = owned::BoundFile::open(&output, &path).unwrap();
    let delta = u64::from(u32::MAX / 2);
    let plan = metadata::restore(&file, track, delta, &mut budget, &Stop::new()).unwrap();
    let digest =
        metadata::apply(&mut file, &plan.patches, plan.digest, false, &Stop::new()).unwrap();
    let actual = fs::read(&path).unwrap();
    assert_eq!(actual.len(), original.len());
    assert_eq!(&actual[sidx.offset + 4..sidx.offset + 8], b"free");
    assert_eq!(
        &actual[sidx.offset + 8..sidx.offset + sidx.size],
        &original[sidx.offset + 8..sidx.offset + sidx.size]
    );
    assert_eq!(first_decode(&path), first_decode(&clear) + delta);
    metadata::apply(&mut file, &plan.patches, digest, true, &Stop::new()).unwrap();
    assert_eq!(fs::read(&path).unwrap(), original);
}

#[test]
fn unexpected_generated_tracks_refuse_before_any_final_fragment_is_updated() {
    let owner = files::Stage::new(None).unwrap();
    let clear = clear_fixture(&owner, 44_100);
    let original = fs::read(&clear).unwrap();
    let output = files::Stage::new(None).unwrap();
    let projected = prepare(
        &owner,
        std::slice::from_ref(&clear),
        &[TrackKind::Audio],
        &Stop::new(),
    )
    .unwrap();
    package_audio(&clear, &output, OutputFormat::Dash, "clear");
    let generated = track_bytes(&output, 0);
    files::create_dir(&output.path().join("track-0001")).unwrap();
    assert!(
        projected
            .restore(&output, output.path(), OutputFormat::Dash, &Stop::new())
            .is_err()
    );
    assert_eq!(track_bytes(&output, 0), generated);
    assert_eq!(fs::read(clear).unwrap(), original);
}
