use super::*;
use crate::{
    conversion::{ContentKey, Converter, InputKeys, native_media},
    events::NoSink,
    mp4,
    toolrun::{Secrets, ToolRunner},
};
use sha2::{Digest as _, Sha256};
use std::{collections::BTreeMap, process::Command};

const KID: &str = "00112233445566778899aabbccddeeff";
const KEY: &str = "101112131415161718191a1b1c1d1e1f";

fn backend() -> Backend {
    Converter::new(
        Arc::new(ToolRunner::new(Arc::new(Secrets::new()), Arc::new(NoSink))),
        "packager",
        "unused",
    )
    .backend
}
fn boxed(kind: [u8; 4], payload: &[u8]) -> Vec<u8> {
    let mut bytes = u32::try_from(payload.len() + 8)
        .unwrap()
        .to_be_bytes()
        .to_vec();
    bytes.extend(kind);
    bytes.extend(payload);
    bytes
}
fn fixture(root: &Path, scheme: &str) -> PathBuf {
    let clip = Path::new(env!("CARGO_MANIFEST_DIR")).join("../drmr/tests/fixtures/bbb/clip.mp4");
    let path = root.join(format!("{scheme}.mp4"));
    let mut command = Command::new("packager");
    command
        .args([
            "--single_threaded",
            "--mp4_reset_initial_composition_offset_to_zero=false",
        ])
        .arg(format!(
            "input={},stream=audio,output={}",
            clip.display(),
            path.display()
        ))
        .args(["--clear_lead", "0", "--fragment_duration", "2"]);
    if scheme != "clear" {
        command.args([
            "--enable_raw_key_encryption",
            "--protection_scheme",
            scheme,
            "--keys",
            &format!("label=:key_id={KID}:key={KEY}"),
        ]);
    }
    assert!(
        command.output().unwrap().status.success(),
        "licensed fixture generation failed"
    );
    path
}
fn id_fields(bytes: &[u8]) -> Vec<(usize, u32)> {
    mp4::walk(&mp4::parse_boxes(bytes).unwrap())
        .into_iter()
        .filter_map(|node| {
            let payload = node.header.payload(bytes);
            let field = match &node.kind() {
                b"tkhd" => {
                    if payload[0] == 0 {
                        12
                    } else {
                        20
                    }
                }
                b"trex" | b"tfhd" | b"sidx" | b"tfra" => 4,
                _ => return None,
            };
            Some((
                node.header.offset + node.header.header_size + field,
                u32::from_be_bytes(payload[field..field + 4].try_into().unwrap()),
            ))
        })
        .collect()
}
fn rebind(path: &Path, id: u32) -> Vec<u8> {
    let mut bytes = std::fs::read(path).unwrap();
    for (at, _) in id_fields(&bytes) {
        bytes[at..at + 4].copy_from_slice(&id.to_be_bytes());
    }
    std::fs::write(path, &bytes).unwrap();
    bytes
}

type Samples = BTreeMap<usize, Vec<(native_media::SampleInfo, [u8; 32])>>;
fn evidence(path: &Path, keys: &InputKeys) -> (native_media::Movie, Samples) {
    let mut samples = Samples::new();
    let movie = native_media::scan(
        path,
        keys,
        &crate::concurrency::Stop::new(),
        &mut |track, info, bytes| {
            samples
                .entry(track.index)
                .or_default()
                .push((info.clone(), Sha256::digest(bytes).into()));
            Ok(())
        },
    )
    .unwrap();
    (movie, samples)
}
fn assert_unchanged_except_ids(before: &[u8], after: &[u8], fields: &[(usize, u32)]) {
    assert_eq!(before.len(), after.len());
    let mut expected = before.to_vec();
    for (at, _) in fields {
        expected[*at..*at + 4].copy_from_slice(&1_u32.to_be_bytes());
    }
    assert_eq!(after, expected);
}

#[test]
fn tool_only_copies_preserve_all_samples_clocks_crypto_and_original_bytes() {
    for scheme in ["clear", "cenc", "cbcs"] {
        let owner = native::private_tempdir().unwrap();
        let path = fixture(owner.path(), scheme);
        let original = rebind(&path, 42);
        let keys = InputKeys::new(vec![ContentKey::from_hex(KID, KEY).unwrap()]).unwrap();
        let (mut expected, samples) = evidence(&path, &keys);
        let command = backend();
        let copy = ToolInput::prepare(&path, &command).unwrap().unwrap();
        let actual = std::fs::read(copy.path()).unwrap();
        assert_unchanged_except_ids(&original, &actual, &id_fields(&original));
        expected.tracks[0].track_id = 1;
        let (movie, copy_samples) = evidence(copy.path(), &keys);
        assert_eq!(movie, expected);
        assert_eq!(copy_samples, samples);
        assert_eq!(std::fs::read(&path).unwrap(), original);
        copy.check(&command.stop).unwrap();
        let temp = copy.path().to_owned();
        drop(copy);
        assert!(!temp.exists());
    }
}

#[test]
fn dense_and_progressive_sources_keep_the_original_tool_input() {
    let owner = native::private_tempdir().unwrap();
    let path = fixture(owner.path(), "clear");
    rebind(&path, 1);
    assert!(ToolInput::prepare(&path, &backend()).unwrap().is_none());
    let clip = Path::new(env!("CARGO_MANIFEST_DIR")).join("../drmr/tests/fixtures/bbb/clip.mp4");
    assert!(ToolInput::prepare(&clip, &backend()).unwrap().is_none());
}

#[test]
fn copied_inputs_remain_creation_bound_and_detect_modified_payloads() {
    for replace in [false, true] {
        let owner = native::private_tempdir().unwrap();
        let path = fixture(owner.path(), "clear");
        let original = rebind(&path, 2);
        let command = backend();
        let copy = ToolInput::prepare(&path, &command).unwrap().unwrap();
        let modified = if replace {
            if std::fs::rename(copy.path(), copy.path().with_file_name("moved.mp4")).is_ok() {
                native::create_private_new(copy.path())
                    .unwrap()
                    .write_all(&std::fs::read(copy.path().with_file_name("moved.mp4")).unwrap())
                    .unwrap();
                true
            } else {
                false
            }
        } else {
            let mut changed = std::fs::read(copy.path()).unwrap();
            let nodes = mp4::parse_boxes(&changed).unwrap();
            let media = nodes.iter().find(|n| n.kind() == *b"mdat").unwrap();
            changed[media.header.offset + media.header.header_size] ^= 1;
            std::fs::write(copy.path(), changed).is_ok()
        };
        assert_eq!(copy.check(&command.stop).is_err(), modified);
        let temp = copy.path().to_owned();
        drop(copy);
        assert!(!temp.exists());
        assert_eq!(std::fs::read(path).unwrap(), original);
    }
}

#[test]
fn cancellation_and_unknown_actual_references_do_not_reach_the_tool() {
    let owner = native::private_tempdir().unwrap();
    let path = fixture(owner.path(), "clear");
    let original = rebind(&path, 2);
    let command = backend();
    command.stop.set();
    assert_eq!(
        ToolInput::prepare(&path, &command).err(),
        Some(ConversionError("cancelled"))
    );
    assert_eq!(std::fs::read(&path).unwrap(), original);
    let mut bytes = original;
    let nodes = mp4::parse_boxes(&bytes).unwrap();
    let field = mp4::walk(&nodes)
        .into_iter()
        .find(|n| n.kind() == *b"tfhd")
        .unwrap();
    let at = field.header.offset + field.header.header_size + 4;
    bytes[at..at + 4].copy_from_slice(&99_u32.to_be_bytes());
    std::fs::write(&path, &bytes).unwrap();
    assert!(ToolInput::prepare(&path, &backend()).is_err());
    assert_eq!(std::fs::read(path).unwrap(), bytes);
}

#[test]
fn nondense_joint_ids_use_one_geometry_preserving_tool_copy() {
    let owner = native::private_tempdir().unwrap();
    let audio = fixture(owner.path(), "clear");
    let video = owner.path().join("video.mp4");
    let clip = Path::new(env!("CARGO_MANIFEST_DIR")).join("../drmr/tests/fixtures/bbb/clip.mp4");
    assert!(
        Command::new("packager")
            .arg(format!(
                "input={},stream=video,output={}",
                clip.display(),
                video.display()
            ))
            .args(["--single_threaded", "--clear_lead", "0"])
            .output()
            .unwrap()
            .status
            .success()
    );
    let source = owner.path().join("joint.mp4");
    crate::conversion::finite::combine_parts(
        &[video, audio],
        &source,
        &crate::concurrency::Stop::new(),
    )
    .unwrap();
    let mut original = std::fs::read(&source).unwrap();
    for (at, old) in id_fields(&original) {
        original[at..at + 4].copy_from_slice(&[7_u32, 42][old as usize - 1].to_be_bytes());
    }
    std::fs::write(&source, &original).unwrap();
    let (mut movie, samples) = evidence(&source, &InputKeys::default());
    let command = backend();
    let copy = ToolInput::prepare(&source, &command).unwrap().unwrap();
    let mut expected_bytes = original.clone();
    for (at, old) in id_fields(&original) {
        expected_bytes[at..at + 4].copy_from_slice(&if old == 7 { 1_u32 } else { 2 }.to_be_bytes());
    }
    assert_eq!(std::fs::read(copy.path()).unwrap(), expected_bytes);
    for (i, track) in movie.tracks.iter_mut().enumerate() {
        track.track_id = u32::try_from(i + 1).unwrap();
    }
    let (actual, copy_samples) = evidence(copy.path(), &InputKeys::default());
    assert_eq!(actual, movie);
    assert_eq!(copy_samples, samples);
    assert_eq!(std::fs::read(source).unwrap(), original);
}

#[test]
fn opaque_control_metadata_is_not_adopted_for_tool_compatibility() {
    let owner = native::private_tempdir().unwrap();
    let source = fixture(owner.path(), "clear");
    let mut original = rebind(&source, 2);
    original.extend(boxed(*b"uuid", &[0x5a; 16]));
    std::fs::write(&source, &original).unwrap();
    assert!(ToolInput::prepare(&source, &backend()).is_err());
    assert_eq!(std::fs::read(source).unwrap(), original);
}

#[test]
fn package_timeout_and_cancellation_reap_before_removing_the_compatibility_input() {
    use std::time::{Duration, Instant};
    for cancel in [false, true] {
        let owner = native::private_tempdir().unwrap();
        let source = fixture(owner.path(), "clear");
        let original = rebind(&source, 2);
        let ready = owner.path().join("ready");
        let audit = owner.path().join("argv");
        let program = drmr_test_support::install(
            &owner.path().join("response"),
            &drmr_test_support::ToolSpec {
                sleep_ms: 60_000,
                ..Default::default()
            }
            .with("ready_file", ready.to_str().unwrap())
            .with("argv_file", audit.to_str().unwrap()),
        )
        .unwrap();
        let mut command = backend();
        command.packager = program.to_str().unwrap().into();
        command.runner = Arc::new(
            ToolRunner::new(Arc::new(Secrets::new()), Arc::new(NoSink)).with_limiter(Arc::new(
                crate::resources::Limiter::new(1, 512 * 1024 * 1024),
            )),
        );
        command.timeout = if cancel {
            Duration::from_secs(10)
        } else {
            Duration::from_millis(300)
        };
        let stop = command.stop.clone();
        let cancel_thread = cancel.then(|| {
            let ready = ready.clone();
            std::thread::spawn(move || {
                let end = Instant::now() + Duration::from_secs(5);
                while !ready.exists() && Instant::now() < end {
                    std::thread::sleep(Duration::from_millis(1));
                }
                assert!(ready.exists());
                stop.set();
            })
        });
        let result =
            command.package_stream(&source, 0, &owner.path().join("output.mp4"), None, None);
        if let Some(thread) = cancel_thread {
            thread.join().unwrap();
        }
        assert_eq!(
            result.err(),
            Some(ConversionError(if cancel {
                "cancelled"
            } else {
                "media-tool-timeout"
            }))
        );
        assert!(ready.exists());
        let argv = std::fs::read_to_string(audit).unwrap();
        let copied = argv
            .lines()
            .find_map(|line| line.strip_prefix("input="))
            .unwrap()
            .split(",stream=")
            .next()
            .unwrap();
        assert_ne!(Path::new(copied), source);
        assert!(!Path::new(copied).exists());
        assert!(!Path::new(copied).parent().unwrap().exists());
        assert_eq!(std::fs::read(source).unwrap(), original);
    }
}

#[test]
fn uncertain_cleanup_and_unwind_retain_only_the_call_owned_copy_until_reaped() {
    use crate::toolrun::{CleanupOwner, RunOptions};
    use std::time::{Duration, Instant};
    struct PanicOnFailure;
    impl crate::events::Sink for PanicOnFailure {
        fn emit(&self, record: &crate::events::Record) {
            assert_ne!(
                record.get("level").and_then(serde_json::Value::as_str),
                Some("error"),
                "synthetic sink failure"
            );
        }
    }
    for unwind in [false, true] {
        let owner = native::private_tempdir().unwrap();
        let source = fixture(owner.path(), "clear");
        let original = rebind(&source, 2);
        let ready = owner.path().join("ready");
        let program = drmr_test_support::install(
            &owner.path().join("response"),
            &drmr_test_support::ToolSpec {
                sleep_ms: 60_000,
                ..Default::default()
            }
            .with("ready_file", ready.to_str().unwrap()),
        )
        .unwrap();
        let cleanup_owner = Arc::new(CleanupOwner::new());
        let sink: Arc<dyn crate::events::Sink> = if unwind {
            Arc::new(PanicOnFailure)
        } else {
            Arc::new(NoSink)
        };
        let runner = ToolRunner::new(Arc::new(Secrets::new()), sink)
            .with_limiter(Arc::new(crate::resources::Limiter::new(
                1,
                512 * 1024 * 1024,
            )))
            .with_cleanup_owner(cleanup_owner.clone());
        let mut command = backend();
        command.runner = Arc::new(runner);
        let mut copied = PathBuf::new();
        let mut called = false;
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let mut copy = ToolInput::prepare(&source, &command).unwrap().unwrap();
            copied = copy.path().to_owned();
            let failed = command.runner.run_tool_injected_cleanup_with_receipt(
                &[program.to_str().unwrap().into()],
                &RunOptions {
                    timeout: Some(Duration::from_millis(100)),
                    ..Default::default()
                },
                &mut copy.cleanup,
                |_| {
                    let end = Instant::now() + Duration::from_secs(5);
                    while !ready.exists() && Instant::now() < end {
                        std::thread::sleep(Duration::from_millis(1));
                    }
                    assert!(ready.exists());
                    assert!(copy.path.is_file());
                    called = true;
                    Err(std::io::Error::other("injected cleanup uncertainty"))
                },
            );
            assert_eq!(failed.unwrap_err().kind, "cleanup-unconfirmed");
            assert!(cleanup_owner.has_pending());
            assert!(copy.path.is_file());
            // Drop must resolve the exact receipt even if the surrounding operation unwinds.
            drop(copy);
        }));
        assert_eq!(result.is_err(), unwind);
        assert!(called);
        assert!(!cleanup_owner.has_pending());
        assert!(!copied.exists());
        assert_eq!(std::fs::read(source).unwrap(), original);
    }
}

#[test]
fn compatibility_fingerprints_are_bounded_to_the_captured_length() {
    let owner = native::private_tempdir().unwrap();
    let path = owner.path().join("source.mp4");
    let mut file = native::create_private_new(&path).unwrap();
    file.write_all(b"bounded bytes").unwrap();
    drop(file);
    let reader = native::open_private_read(&path).unwrap();
    let stop = crate::concurrency::Stop::new();
    assert!(fingerprint(&reader, 13, &stop).is_ok());
    assert_eq!(fingerprint(&reader, 12, &stop).err(), Some(INVALID));
    assert_eq!(fingerprint(&reader, 14, &stop).err(), Some(INVALID));
}
