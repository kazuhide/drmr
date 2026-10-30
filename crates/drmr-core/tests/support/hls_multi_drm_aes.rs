//! Outer AES encryption never conceals unqualified fragmented MP4 controls.
use super::*;
use drmr_core::conversion::plan::{
    ExecutionPlanRequest,
    bundle::{BundleOutputIntent, BundlePlanRequest, ManifestValidation},
};
use drmr_core::conversion::{Aes128Key, SegmentIv, encrypt_segment};
use std::fmt::Write as _;
const AES_URI: &str = "https://example.invalid/retained-source-key";
const AES_KEY: &str = "202122232425262728292a2b2c2d2e2f";
const AES_IV: &str = "303132333435363738393a3b3c3d3e3f";

#[derive(Clone, Copy, Debug)]
enum Encryption {
    All,
    Initialization,
    Media,
    Alternating,
    Unused,
}
impl Encryption {
    fn active(self, map: bool, index: usize) -> bool {
        match self {
            Self::All => true,
            Self::Initialization => map,
            Self::Media => !map,
            Self::Alternating => map || index % 2 == 1,
            Self::Unused => false,
        }
    }
}
fn input_keys() -> HlsInputKeys {
    HlsInputKeys::new(vec![(
        AES_URI.into(),
        Aes128Key::from_hex(AES_KEY).unwrap(),
    )])
    .unwrap()
}
fn seed(root: &Path) -> PathBuf {
    let (command, _) = converter();
    convert(
        &command,
        &fixture("clip.mp4"),
        &InputKeys::default(),
        root,
        OutputFormat::Hls,
        TargetProtection::Clear,
        &key_file::OperatorKeys::default(),
    );
    let media = playlists(root)
        .into_iter()
        .find(|path| {
            let text = std::fs::read_to_string(path).unwrap();
            let value = text
                .lines()
                .find_map(|line| line.strip_prefix("#EXT-X-MAP:"))
                .unwrap();
            let attrs = drmr_core::m3u8::parse_attributes(value).unwrap();
            let init = path.parent().unwrap().join(attrs.get("URI").unwrap());
            drmr_core::inspection::inspect(&init).unwrap().tracks[0].kind
                == drmr_core::inspection::TrackKind::Video
        })
        .unwrap();
    let uri = media
        .strip_prefix(root)
        .unwrap()
        .to_str()
        .unwrap()
        .replace('\\', "/");
    let master = root.join("master.m3u8");
    std::fs::write(&master, format!("#EXTM3U\n#EXT-X-VERSION:7\n#EXT-X-STREAM-INF:BANDWIDTH=1000000,RESOLUTION=320x180\n{uri}\n")).unwrap();
    master
}
fn encrypted_fixture(
    seed: &Path,
    root: &Path,
    encryption: Encryption,
    opaque: Option<(bool, &[u8])>,
) -> PathBuf {
    let source = seed.parent().unwrap();
    for (name, bytes) in common::snapshot_tree(source) {
        let path = root.join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, bytes).unwrap();
    }
    let entrypoint = root.join("master.m3u8");
    let text = std::fs::read_to_string(&entrypoint).unwrap();
    let media = root.join(
        text.lines()
            .find(|line| !line.starts_with('#') && !line.is_empty())
            .unwrap(),
    );
    let original = std::fs::read_to_string(&media).unwrap();
    let aes = format!("#EXT-X-KEY:METHOD=AES-128,URI=\"{AES_URI}\",IV=0x{AES_IV}");
    let mut rewritten = String::new();
    let mut index = 0;
    for line in original.lines() {
        let map = line.starts_with("#EXT-X-MAP:");
        let uri = if map {
            let attrs =
                drmr_core::m3u8::parse_attributes(line.strip_prefix("#EXT-X-MAP:").unwrap())
                    .unwrap();
            Some(attrs.get("URI").unwrap().to_owned())
        } else {
            (!line.is_empty() && !line.starts_with('#')).then(|| line.to_owned())
        };
        if let Some(uri) = uri {
            let active = encryption.active(map, index);
            if map && matches!(encryption, Encryption::Unused) {
                writeln!(rewritten, "{aes}").unwrap();
            }
            rewritten.push_str(if active {
                &aes
            } else {
                "#EXT-X-KEY:METHOD=NONE"
            });
            rewritten.push('\n');
            let path = media.parent().unwrap().join(uri);
            let mut bytes = std::fs::read(&path).unwrap();
            if let Some((on_map, opaque)) = opaque
                && map == on_map
                && (map || index == 0)
            {
                bytes.extend_from_slice(opaque);
            }
            std::fs::write(&path, &bytes).unwrap();
            if active {
                let plaintext = root.join("plaintext.tmp");
                std::fs::write(&plaintext, bytes).unwrap();
                std::fs::remove_file(&path).unwrap();
                encrypt_segment(
                    &plaintext,
                    &path,
                    &Aes128Key::from_hex(AES_KEY).unwrap(),
                    &SegmentIv::from_hex(AES_IV).unwrap(),
                )
                .unwrap();
                std::fs::remove_file(plaintext).unwrap();
            }
            if !map {
                index += 1;
            }
        }
        rewritten.push_str(line);
        rewritten.push('\n');
    }
    std::fs::write(media, rewritten).unwrap();
    entrypoint
}
fn no_tools(decode: bool) -> (Converter, Arc<Collector>) {
    let events = Arc::new(Collector::default());
    (
        Converter::new(
            Arc::new(ToolRunner::new(Arc::new(Secrets::new()), events.clone())),
            "must-not-start-packager",
            "must-not-start-decoder",
        )
        .with_ffprobe("must-not-start-probe")
        .with_decode(decode),
        events,
    )
}
fn refused(input: &Path, root: &Path, format: OutputFormat, decode: bool, mode: ProcessingMode) {
    let (command, events) = no_tools(decode);
    let command = command.with_processing(mode);
    let keys = operator_keys(
        root,
        &format!("operator-{format:?}-{decode}-{mode:?}"),
        TargetProtection::Cbcs,
        format,
    );
    let empty = InputKeys::default();
    let hls = input_keys();
    let systems = keys.signaling.as_ref().unwrap().systems();
    let output = root.join(format!("refused-{format:?}-{decode}-{mode:?}"));
    let original = common::snapshot_tree(input.parent().unwrap());
    let public = command
        .validate_drm_source(
            view(input, &empty, &hls),
            &systems,
            format,
            TargetProtection::Cbcs,
        )
        .unwrap_err();
    let started: Vec<_> = events
        .records()
        .iter()
        .filter(|record| record.get("event") == Some(&serde_json::json!("command")))
        .filter_map(|record| record.get("tool").cloned())
        .collect();
    assert_eq!(
        public.0, "output-signaling-profile-not-supported",
        "public/{format:?}/{decode}; started={started:?}"
    );
    let outputs = [BundleOutputIntent {
        id: "result".into(),
        format,
        target: TargetProtection::Cbcs,
        drms: systems,
        conformance: ManifestValidation::None,
        processing: None,
        dash_addressing: None,
    }];
    assert_eq!(
        command
            .create_bundle_plan(BundlePlanRequest {
                input: view(input, &empty, &hls),
                output_root: &output,
                outputs: &outputs
            })
            .unwrap_err()
            .0,
        "output-signaling-profile-not-supported",
        "bundle/{format:?}/{decode}"
    );
    assert_eq!(
        command
            .with_output_signaling(keys.signaling.clone())
            .convert(ConversionRequest {
                input,
                output: &output,
                format,
                target: TargetProtection::Cbcs,
                input_keys: &empty,
                hls_input_keys: &hls,
                output_key: keys.output.as_ref(),
                hls_key_uri: if format == OutputFormat::Hls {
                    keys.hls_output_uri()
                } else {
                    None
                },
                hls_output_key: None,
            })
            .unwrap_err()
            .0,
        "output-signaling-profile-not-supported",
        "convert/{format:?}/{decode}"
    );
    assert!(!output.exists());
    assert!(
        !events
            .records()
            .iter()
            .any(|record| record.get("event") == Some(&serde_json::json!("command")))
    );
    assert_eq!(common::snapshot_tree(input.parent().unwrap()), original);
}
#[test]
fn encrypted_and_mixed_fragmented_resources_refuse_opaque_plaintext_before_any_tool() {
    let root = common::private_tempdir().unwrap();
    let seed = seed(&root.path().join("seed"));
    let skip = b"\0\0\0\x08skip";
    let uuid = [b"\0\0\0\x18uuid".as_slice(), &[0; 16]].concat();
    for (index, (encryption, map, opaque)) in [
        (Encryption::All, true, skip.as_slice()),
        (Encryption::All, true, uuid.as_slice()),
        (Encryption::All, false, skip.as_slice()),
        (Encryption::Initialization, true, skip.as_slice()),
        (Encryption::Media, true, skip.as_slice()),
        (Encryption::Alternating, false, skip.as_slice()),
        (Encryption::Unused, true, skip.as_slice()),
    ]
    .into_iter()
    .enumerate()
    {
        let cell = root.path().join(format!("cell-{index}"));
        let input = encrypted_fixture(&seed, &cell.join("input"), encryption, Some((map, opaque)));
        for decode in [true, false] {
            for format in [OutputFormat::Hls, OutputFormat::Dash] {
                for mode in [ProcessingMode::Stitch, ProcessingMode::Segments] {
                    refused(&input, &cell, format, decode, mode);
                }
            }
        }
    }
}
#[test]
fn a_saved_plan_with_an_unused_aes_declaration_cannot_skip_plaintext_census() {
    let root = common::private_tempdir().unwrap();
    let seed = seed(&root.path().join("seed"));
    let input = encrypted_fixture(
        &seed,
        &root.path().join("input"),
        Encryption::Unused,
        Some((true, b"\0\0\0\x08skip")),
    );
    let empty = InputKeys::default();
    let hls = input_keys();
    let output = root.path().join("output");
    let (command, _) = no_tools(false);
    // Generic native admission does not imply qualification for a later explicit DRM selection.
    let plan = command
        .create_execution_plan(ExecutionPlanRequest {
            input: view(&input, &empty, &hls),
            output: &output,
            format: Some(OutputFormat::Hls),
            target: TargetProtection::Cbcs,
        })
        .unwrap();
    let mut keys = operator_keys(
        root.path(),
        "saved",
        TargetProtection::Cbcs,
        OutputFormat::Hls,
    );
    keys.hls_input = hls;
    let (command, events) = no_tools(true);
    assert_eq!(
        command.execute_plan(&plan, &keys).unwrap_err().0,
        "output-signaling-profile-not-supported"
    );
    assert!(!output.exists());
    assert!(
        !events
            .records()
            .iter()
            .any(|record| record.get("event") == Some(&serde_json::json!("command")))
    );
}

#[test]
fn supported_outer_aes_sources_keep_complete_native_and_optional_decode_proofs() {
    let root = common::private_tempdir().unwrap();
    let reference = seed(&root.path().join("seed"));
    let before = common::snapshot_tree(reference.parent().unwrap());
    for (index, encryption) in [
        Encryption::All,
        Encryption::Initialization,
        Encryption::Media,
        Encryption::Alternating,
        Encryption::Unused,
    ]
    .into_iter()
    .enumerate()
    {
        let cell = root.path().join(format!("valid-{index}"));
        let input = encrypted_fixture(&reference, &cell.join("input"), encryption, None);
        for format in [OutputFormat::Hls, OutputFormat::Dash] {
            positive_conversion(&input, &reference, &cell, format, false);
        }
        if matches!(encryption, Encryption::All) {
            positive_conversion(&input, &reference, &cell, OutputFormat::Hls, true);
        }
    }
    assert_eq!(common::snapshot_tree(reference.parent().unwrap()), before);
}
fn positive_conversion(
    input: &Path,
    reference: &Path,
    root: &Path,
    format: OutputFormat,
    decode: bool,
) {
    let keys = operator_keys(
        root,
        &format!("positive-{format:?}-{decode}"),
        TargetProtection::Cbcs,
        format,
    );
    let events = Arc::new(Collector::default());
    let command = Converter::new(
        Arc::new(ToolRunner::new(Arc::new(Secrets::new()), events.clone())),
        "packager",
        "ffmpeg",
    )
    .with_decode(decode)
    .with_output_signaling(keys.signaling.clone());
    let empty = InputKeys::default();
    let hls = input_keys();
    let output = root.join(format!("output-{format:?}-{decode}"));
    let before = common::snapshot_tree(input.parent().unwrap());
    let report = command
        .convert(ConversionRequest {
            input,
            output: &output,
            format,
            target: TargetProtection::Cbcs,
            input_keys: &empty,
            hls_input_keys: &hls,
            output_key: keys.output.as_ref(),
            hls_key_uri: if format == OutputFormat::Hls {
                keys.hls_output_uri()
            } else {
                None
            },
            hls_output_key: None,
        })
        .unwrap_or_else(|error| panic!("{format:?}/{decode}: {error}"));
    assert!(report.verification.passed && report.verification.checks.is_pass());
    if decode {
        assert!(matches!(
            report.verification.decode_evidence,
            DecodeEvidence::ContentCompared { equal: true }
        ));
    } else {
        assert_eq!(
            report.verification.decode_evidence,
            DecodeEvidence::not_requested()
        );
        assert!(!events.records().iter().any(|record| matches!(
            record.get("tool").and_then(serde_json::Value::as_str),
            Some("ffmpeg" | "ffprobe")
        )));
    }
    assert!(!report.verification.license_playback_verified);
    let manifest = output.join(if format == OutputFormat::Hls {
        "master.m3u8"
    } else {
        "manifest.mpd"
    });
    fresh(
        &command,
        reference,
        &manifest,
        &keys,
        TargetProtection::Cbcs,
        format,
    );
    assert_eq!(common::snapshot_tree(input.parent().unwrap()), before);
}

#[test]
fn every_encrypted_rendition_is_censused_before_the_first_decoded_probe() {
    let root = common::private_tempdir().unwrap();
    let original = seed(&root.path().join("seed"));
    let good = encrypted_fixture(&original, &root.path().join("good"), Encryption::All, None);
    let bad = encrypted_fixture(
        &original,
        &root.path().join("bad"),
        Encryption::All,
        Some((true, b"\0\0\0\x08skip")),
    );
    let uri = |input: &Path| {
        let text = std::fs::read_to_string(input).unwrap();
        let media = text
            .lines()
            .find(|line| !line.starts_with('#') && !line.is_empty())
            .unwrap();
        format!(
            "{}/{}",
            input
                .parent()
                .unwrap()
                .file_name()
                .unwrap()
                .to_str()
                .unwrap(),
            media
        )
    };
    let input = root.path().join("master.m3u8");
    std::fs::write(&input, format!("#EXTM3U\n#EXT-X-VERSION:7\n#EXT-X-STREAM-INF:BANDWIDTH=1000000,RESOLUTION=320x180\n{}\n#EXT-X-STREAM-INF:BANDWIDTH=1000001,RESOLUTION=320x180\n{}\n", uri(&good), uri(&bad))).unwrap();
    let empty = InputKeys::default();
    let hls = input_keys();
    let keys = operator_keys(
        root.path(),
        "all-renditions",
        TargetProtection::Cbcs,
        OutputFormat::Hls,
    );
    // One worker makes the required barrier deterministic: the valid first rendition
    // must never start probing before the later invalid rendition has been censused.
    let (command, events) = no_tools(true);
    let error = command
        .with_max_workers(Some(1))
        .unwrap()
        .validate_drm_source(
            view(&input, &empty, &hls),
            &keys.signaling.unwrap().systems(),
            OutputFormat::Hls,
            TargetProtection::Cbcs,
        )
        .unwrap_err();
    let started: Vec<_> = events
        .records()
        .iter()
        .filter(|record| record.get("event") == Some(&serde_json::json!("command")))
        .filter_map(|record| record.get("tool").cloned())
        .collect();
    assert_eq!(
        error.0, "output-signaling-profile-not-supported",
        "started={started:?}"
    );
    assert_eq!(started, [] as [serde_json::Value; 0]);
}
