//! Independent packaging and decryption exercise the aggregate evidence across schemes.
use super::{Fixture, assert_no_tools, audio_fixture, converter, fixture};
use crate::conversion::{ContentKey, InputKeys, native::SegmentedMedia};
use std::{
    path::{Path, PathBuf},
    process::Command,
};

const KID: &str = "000102030405060708090a0b0c0d0e0f";
const KEY: &str = "101112131415161718191a1b1c1d1e1f";

fn run(command: &mut Command) -> Vec<u8> {
    let output = command.output().unwrap();
    assert!(
        output.status.success(),
        "independent fixture/oracle tool failed"
    );
    output.stdout
}

fn oracle(init: &Path, resources: &[PathBuf], encrypted: bool) -> serde_json::Value {
    // Joining here is test-only preparation for the independent ffprobe oracle.
    let full = init.with_file_name("oracle.mp4");
    let mut bytes = std::fs::read(init).unwrap();
    for resource in resources {
        bytes.extend(std::fs::read(resource).unwrap());
    }
    std::fs::write(&full, bytes).unwrap();
    let mut command = Command::new("ffprobe");
    command.args(["-v", "error", "-ignore_editlist", "1"]);
    if encrypted {
        command.arg("-decryption_keys").arg(format!("{KID}={KEY}"));
    }
    serde_json::from_slice(&run(command
        .args([
            "-show_packets",
            "-show_data_hash",
            "sha256",
            "-show_entries",
            "packet=stream_index,pts,dts,duration,size,data_hash:packet_side_data=",
            "-of",
            "json",
        ])
        .arg(full)))
    .unwrap()
}

fn packaged(source: &Fixture, track: &str, scheme: &str) -> (PathBuf, Vec<PathBuf>) {
    let directory = source.directory.path().join(scheme);
    std::fs::create_dir(&directory).unwrap();
    let init = directory.join("init.mp4");
    let template = directory.join("part-$Number$.m4s");
    let input = crate::manifest_support::path::fspath(&source.full).unwrap();
    let init_string = crate::manifest_support::path::fspath(&init).unwrap();
    let template_string = crate::manifest_support::path::fspath(&template).unwrap();
    let mut command = Command::new("packager");
    command.arg("--single_threaded");
    command.arg(format!("input={input},stream={track},init_segment={init_string},segment_template={template_string},drm_label=V"))
        .args(["--segment_duration", "2", "--mp4_reset_initial_composition_offset_to_zero=false"]);
    if scheme != "clear" {
        command
            .args(["--enable_raw_key_encryption", "--keys"])
            .arg(format!("label=V:key_id={KID}:key={KEY}"))
            .args(["--protection_scheme", scheme, "--clear_lead", "0"]);
    }
    run(&mut command);
    let mut resources = std::fs::read_dir(directory)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|value| value == "m4s"))
        .collect::<Vec<_>>();
    resources.sort();
    assert_ne!(resources.as_slice(), [] as [PathBuf; 0]);
    (init, resources)
}

fn compare_schemes(source: &Fixture, track: &str) {
    let (converter, events) = converter();
    let backend = &converter.backend;
    let keys = InputKeys::new(vec![ContentKey::from_hex(KID, KEY).unwrap()]).unwrap();
    let clear_keys = InputKeys::default();
    let mut media = Vec::<SegmentedMedia>::new();
    let mut expected = None;
    for scheme in ["clear", "cenc", "cbcs"] {
        let (init, resources) = packaged(source, track, scheme);
        let observed = oracle(&init, &resources, scheme != "clear");
        assert_ne!(
            observed["packets"].as_array().unwrap().as_slice(),
            [] as [serde_json::Value; 0]
        );
        if let Some(expected) = &expected {
            assert_eq!(&observed, expected, "independent decrypted packets differ");
        } else {
            expected = Some(observed);
        }
        let parsed = backend
            .native_segmented_media(
                &init,
                &resources,
                if scheme == "clear" {
                    &clear_keys
                } else {
                    &keys
                },
            )
            .unwrap();
        let sample = &parsed.media.tracks[0].samples;
        assert_eq!(
            sample.protected_samples,
            if scheme == "clear" { 0 } else { sample.count }
        );
        assert!(backend.native_admit_segmented(&parsed).unwrap().passed);
        media.push(parsed);
    }
    for source in &media {
        for output in &media {
            assert!(
                backend
                    .native_verify_segmented(source, output)
                    .unwrap()
                    .passed
            );
        }
    }
    assert_no_tools(&events);
}

#[test]
fn clear_cenc_cbcs_video_and_aac_at_both_rates_match_independent_decryption() {
    compare_schemes(&fixture(), "video");
    for rate in [44_100, 48_000] {
        compare_schemes(&audio_fixture(rate), "audio");
    }
}
