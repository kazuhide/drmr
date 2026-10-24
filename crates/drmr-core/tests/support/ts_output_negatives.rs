//! Genuine source/candidate mutations exercise the complete TS output verification boundary.
use super::{
    Aes128Key, ConversionRequest, Converter, HlsInputKeys, HlsOutputKey, INPUT_IV, INPUT_KEY,
    InputKeys, KEY_URI, OUTPUT_IV, OUTPUT_KEY, OutputFormat, Path, SegmentIv, TargetProtection,
    all_checks, common, converter, sample_aes_fixture, shaka_source,
};
use drmr_core::conversion::{VerificationReport, VerificationRequest, VerificationSource};
use drmr_core::toolrun::{Secrets, ToolRunner};
use std::{path::PathBuf, sync::Arc};

fn input_keys(key: &str, iv: &str) -> HlsInputKeys {
    HlsInputKeys::new_with_ivs(vec![(
        KEY_URI.into(),
        Aes128Key::from_hex(key).unwrap(),
        Some(SegmentIv::from_hex(iv).unwrap()),
    )])
    .unwrap()
}
fn verify(
    c: &Converter,
    source: &Path,
    output: &Path,
    keys: &HlsInputKeys,
) -> Result<VerificationReport, drmr_core::conversion::ConversionError> {
    let empty = InputKeys::default();
    c.verify(VerificationRequest {
        source: VerificationSource {
            input: source,
            input_keys: &empty,
            hls_keys: &HlsInputKeys::default(),
        },
        output: VerificationSource {
            input: &output.join("master.m3u8"),
            input_keys: &empty,
            hls_keys: keys,
        },
    })
}
fn refused(result: Result<VerificationReport, drmr_core::conversion::ConversionError>) {
    match result {
        Ok(report) => {
            assert!(!report.passed);
            assert!(
                !report.checks.is_pass()
                    || !report.decode_evidence.satisfies(
                        drmr_core::conversion::ValidationDepth::Decoded,
                        report.scope
                    )
            );
        }
        Err(error) => assert!(
            !matches!(
                error.0,
                "cleanup-unconfirmed" | "media-tool-timeout" | "cancelled" | "resource-budget"
            ),
            "an unrelated lifecycle/admission failure is not a media refusal"
        ),
    }
}
fn convert(
    c: &Converter,
    input: &Path,
    output: &Path,
    keys: &HlsInputKeys,
) -> Result<drmr_core::conversion::ConversionReport, drmr_core::conversion::ConversionError> {
    let key = HlsOutputKey::new_with_iv(
        KEY_URI,
        Aes128Key::from_hex(OUTPUT_KEY).unwrap(),
        SegmentIv::from_hex(OUTPUT_IV).unwrap(),
    )
    .unwrap();
    c.convert(ConversionRequest {
        input,
        output,
        format: OutputFormat::HlsTs,
        target: TargetProtection::HlsSampleAes,
        input_keys: &InputKeys::default(),
        hls_input_keys: keys,
        output_key: None,
        hls_key_uri: None,
        hls_output_key: Some(&key),
    })
}
fn last_resource(root: &Path, ordinal: usize) -> PathBuf {
    let directory = root.join(format!("track-{ordinal:04}"));
    let text = std::fs::read_to_string(directory.join("media.m3u8")).unwrap();
    let name = text
        .lines()
        .rfind(|line| !line.is_empty() && !line.starts_with('#'))
        .unwrap();
    assert!(!name.contains(['/', '\\']));
    directory.join(name)
}
fn last_pes_offsets(bytes: &[u8], audio: bool) -> Vec<usize> {
    assert!(bytes.len() <= 8 * 1024 * 1024 && bytes.len().is_multiple_of(188));
    let mut id = None;
    let mut offsets = Vec::new();
    for (slot, packet) in bytes.as_chunks::<188>().0.iter().enumerate() {
        if packet[3] & 0x10 == 0 {
            continue;
        }
        let at = if packet[3] & 0x20 == 0 {
            4
        } else {
            5 + usize::from(packet[4])
        };
        let pid = (u16::from(packet[1] & 31) << 8) | u16::from(packet[2]);
        if packet[1] & 0x40 != 0
            && packet.get(at..at + 3) == Some(&[0, 0, 1])
            && packet.get(at + 3).is_some_and(|stream| {
                if audio {
                    *stream & 0xe0 == 0xc0
                } else {
                    *stream & 0xf0 == 0xe0
                }
            })
        {
            id = Some(pid);
            offsets.clear();
        }
        if id == Some(pid) {
            offsets.extend((at..188).map(|offset| slot * 188 + offset));
        }
    }
    assert!(offsets.len() > 14);
    offsets
}
pub(super) fn shift_final_audio_pes(path: &Path) {
    shift_final_audio_pes_by(path, 1);
}
pub(super) fn shift_final_audio_pes_by(path: &Path, delta: u64) {
    let mut bytes = std::fs::read(path).unwrap();
    let offsets = last_pes_offsets(&bytes, true);
    let old: [u8; 5] = std::array::from_fn(|index| bytes[offsets[9 + index]]);
    let pts = (u64::from(old[0] & 0x0e) << 29)
        | (u64::from(old[1]) << 22)
        | (u64::from(old[2] & 0xfe) << 14)
        | (u64::from(old[3]) << 7)
        | u64::from(old[4] >> 1);
    let next = pts.checked_add(delta).unwrap();
    let encoded = [
        old[0] & 0xf0 | u8::try_from((next >> 29) & 0x0e).unwrap() | 1,
        u8::try_from((next >> 22) & 255).unwrap(),
        u8::try_from((next >> 14) & 0xfe).unwrap() | 1,
        u8::try_from((next >> 7) & 255).unwrap(),
        u8::try_from((next << 1) & 0xfe).unwrap() | 1,
    ];
    for (at, value) in offsets[9..14].iter().zip(encoded) {
        bytes[*at] = value;
    }
    std::fs::write(path, bytes).unwrap();
}
pub(super) fn change_final_audio_sample_rate(path: &Path) {
    let mut bytes = std::fs::read(path).unwrap();
    let offsets = last_pes_offsets(&bytes, true);
    let elementary = 9 + usize::from(bytes[offsets[8]]);
    let at = offsets[elementary + 2];
    assert_eq!((bytes[at] >> 2) & 15, 4);
    bytes[at] = (bytes[at] & 0xc3) | (3 << 2);
    std::fs::write(path, bytes).unwrap();
}
fn corrupt_final_picture(path: &Path) {
    let mut bytes = std::fs::read(path).unwrap();
    let offsets = last_pes_offsets(&bytes, false);
    let elementary = 9 + usize::from(bytes[offsets[8]]);
    let data = offsets[elementary..]
        .iter()
        .map(|at| bytes[*at])
        .collect::<Vec<_>>();
    let nal = data
        .windows(4)
        .enumerate()
        .filter(|(_, b)| b[..3] == [0, 0, 1] && matches!(b[3] & 31, 1 | 5))
        .map(|(at, _)| at + 3)
        .next_back()
        .unwrap();
    assert!(nal + 40 < data.len());
    bytes[offsets[elementary + nal + 40]] ^= 0x40;
    std::fs::write(path, bytes).unwrap();
}
fn missing_or_repeated_last_segment(
    c: &Converter,
    source: &Path,
    output: &Path,
    keys: &HlsInputKeys,
) {
    let playlist = output.join("track-0000/media.m3u8");
    let before = std::fs::read_to_string(&playlist).unwrap();
    let lines = before.lines().collect::<Vec<_>>();
    let last = lines
        .iter()
        .rposition(|line| !line.is_empty() && !line.starts_with('#'))
        .unwrap();
    assert!(lines[last - 1].starts_with("#EXTINF:"));
    let mut shortened = lines.clone();
    shortened.drain(last - 1..=last);
    std::fs::write(&playlist, shortened.join("\n") + "\n").unwrap();
    refused(verify(c, source, output, keys));
    let repeat = format!("{}\n{}\n#EXT-X-ENDLIST", lines[last - 1], lines[last]);
    std::fs::write(&playlist, before.replace("#EXT-X-ENDLIST", &repeat)).unwrap();
    refused(verify(c, source, output, keys));
    std::fs::write(playlist, before).unwrap();
}
#[test]
fn final_cipher_picture_audio_clock_and_master_associations_cannot_escape_fresh_verification() {
    let root = drmr_core::native::private_tempdir().unwrap();
    let fixture = sample_aes_fixture::source(&root.path().join("input"), INPUT_KEY, INPUT_IV);
    let original = common::snapshot_tree(fixture.clear.parent().unwrap());
    let output = root.path().join("output");
    let c = converter();
    all_checks(
        &convert(&c, &fixture.clear, &output, &HlsInputKeys::default())
            .unwrap()
            .verification,
    );
    let correct = input_keys(OUTPUT_KEY, OUTPUT_IV);
    all_checks(&verify(&c, &fixture.clear, &output, &correct).unwrap());
    for keys in [
        input_keys(INPUT_KEY, OUTPUT_IV),
        input_keys(OUTPUT_KEY, INPUT_IV),
        HlsInputKeys::default(),
    ] {
        refused(verify(&c, &fixture.clear, &output, &keys));
    }
    missing_or_repeated_last_segment(&c, &fixture.clear, &output, &correct);
    let video = last_resource(&output, 0);
    let before = std::fs::read(&video).unwrap();
    corrupt_final_picture(&video);
    refused(verify(&c, &fixture.clear, &output, &correct));
    std::fs::write(video, before).unwrap();
    let audio = last_resource(&output, 1);
    let before = std::fs::read(&audio).unwrap();
    shift_final_audio_pes(&audio);
    refused(verify(&c, &fixture.clear, &output, &correct));
    std::fs::write(audio, before).unwrap();
    let master = output.join("master.m3u8");
    let before = std::fs::read_to_string(&master).unwrap();
    assert!(before.contains("AUDIO=\"default-audio-group\""));
    let mut reordered = before
        .lines()
        .filter(|line| !line.starts_with("#EXT-X-MEDIA:"))
        .map(str::to_owned)
        .collect::<Vec<_>>();
    reordered.extend(
        before
            .lines()
            .filter(|line| line.starts_with("#EXT-X-MEDIA:"))
            .map(str::to_owned),
    );
    std::fs::write(&master, reordered.join("\n") + "\n").unwrap();
    all_checks(&verify(&c, &fixture.clear, &output, &correct).unwrap());
    std::fs::write(
        &master,
        before.replace("AUDIO=\"default-audio-group\"", "AUDIO=\"unrelated-group\""),
    )
    .unwrap();
    refused(verify(&c, &fixture.clear, &output, &correct));
    std::fs::write(
        &master,
        before.replace(
            "URI=\"track-0001/media.m3u8\"",
            "URI=\"track-0000/media.m3u8\"",
        ),
    )
    .unwrap();
    refused(verify(&c, &fixture.clear, &output, &correct));
    std::fs::write(master, before).unwrap();
    all_checks(&verify(&c, &fixture.clear, &output, &correct).unwrap());
    assert_eq!(
        common::snapshot_tree(fixture.clear.parent().unwrap()),
        original
    );
    assert!(common::snapshot_tree(&output).iter().all(|(name, _)| {
        Path::new(name)
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("ts") || ext.eq_ignore_ascii_case("m3u8"))
    }));
}
#[test]
fn wrong_source_key_and_noncontiguous_sample_aes_audio_cannot_publish_ts_output() {
    let root = drmr_core::native::private_tempdir().unwrap();
    let fixture = sample_aes_fixture::source(&root.path().join("input"), INPUT_KEY, INPUT_IV);
    let source = fixture.encrypted.parent().unwrap();
    let original = common::snapshot_tree(source);
    let c = converter();
    let output = root.path().join("wrong-key");
    assert!(
        convert(
            &c,
            &fixture.encrypted,
            &output,
            &input_keys(OUTPUT_KEY, INPUT_IV)
        )
        .is_err()
    );
    assert!(!output.exists());
    assert_eq!(common::snapshot_tree(source), original);
    for (ordinal, keys) in [HlsInputKeys::default(), input_keys(INPUT_KEY, OUTPUT_IV)]
        .into_iter()
        .enumerate()
    {
        let output = root.path().join(format!("missing-or-wrong-iv-{ordinal}"));
        assert!(convert(&c, &fixture.encrypted, &output, &keys).is_err());
        assert!(!output.exists());
        assert_eq!(common::snapshot_tree(source), original);
    }
    let text = std::fs::read_to_string(&fixture.encrypted).unwrap();
    let last = text
        .lines()
        .rfind(|line| !line.is_empty() && !line.starts_with('#'))
        .unwrap();
    let segment = source.join(last);
    shift_final_audio_pes(&segment);
    let mutated = common::snapshot_tree(source);
    let output = root.path().join("audio-clock");
    let events = Arc::new(drmr_core::events::Collector::default());
    let c = Converter::new(
        Arc::new(ToolRunner::new(
            Arc::new(Secrets::default()),
            events.clone(),
        )),
        "packager",
        "ffmpeg",
    )
    .with_decode(true);
    assert_eq!(
        convert(
            &c,
            &fixture.encrypted,
            &output,
            &input_keys(INPUT_KEY, INPUT_IV)
        )
        .unwrap_err()
        .0,
        "ts-clock-wrap-or-discontinuity-not-supported"
    );
    assert_eq!(events.records().len(), 0);
    assert!(!output.exists());
    assert_eq!(common::snapshot_tree(source), mutated);
}

#[test]
fn ts_output_never_overwrites_an_existing_destination_or_changes_original_media() {
    use std::io::Write as _;
    let root = drmr_core::native::private_tempdir().unwrap();
    let source = super::shaka_source(&root.path().join("input"), "video", false);
    let before = common::snapshot_tree(source.parent().unwrap());
    let destination = root.path().join("existing");
    drmr_core::native::create_media_dir_all(&destination).unwrap();
    drmr_core::native::create_media_new(&destination.join("sentinel"))
        .unwrap()
        .write_all(b"existing operator data")
        .unwrap();
    let existing = common::snapshot_tree(&destination);
    let error = convert(
        &converter(),
        &source,
        &destination,
        &HlsInputKeys::default(),
    )
    .unwrap_err();
    assert_eq!(error.0, "output-exists-or-unsafe");
    assert_eq!(common::snapshot_tree(&destination), existing);
    assert_eq!(common::snapshot_tree(source.parent().unwrap()), before);
}

#[test]
fn whole_resource_aes_ts_rejects_wrong_key_manifest_iv_and_last_padding_block_corruption() {
    let root = drmr_core::native::private_tempdir().unwrap();
    let source = super::shaka_source(&root.path().join("input"), "video", false);
    let before = common::snapshot_tree(source.parent().unwrap());
    let output = root.path().join("output");
    let c = converter();
    let material = super::output_materials()
        .into_iter()
        .find(|value| value.target == TargetProtection::HlsAes128)
        .unwrap();
    all_checks(
        &c.convert(ConversionRequest {
            input: &source,
            output: &output,
            format: OutputFormat::HlsTs,
            target: material.target,
            input_keys: &InputKeys::default(),
            hls_input_keys: &HlsInputKeys::default(),
            output_key: None,
            hls_key_uri: None,
            hls_output_key: material.key.as_ref(),
        })
        .unwrap()
        .verification,
    );
    all_checks(&verify(&c, &source, &output, &material.keys).unwrap());
    refused(verify(
        &c,
        &source,
        &output,
        &input_keys(INPUT_KEY, OUTPUT_IV),
    ));
    let playlist = output.join("track-0000/media.m3u8");
    let text = std::fs::read_to_string(&playlist).unwrap();
    let tag = text
        .lines()
        .find(|line| line.starts_with("#EXT-X-KEY:"))
        .unwrap();
    let (_, iv) = tag.split_once(",IV=").unwrap();
    let changed = text.replacen(iv, &format!("0x{INPUT_IV}"), 1);
    assert_ne!(changed, text);
    std::fs::write(&playlist, changed).unwrap();
    refused(verify(&c, &source, &output, &material.keys));
    std::fs::write(playlist, text).unwrap();
    let segment = last_resource(&output, 0);
    let original = std::fs::read(&segment).unwrap();
    assert!(original.len() > 32 && original.len().is_multiple_of(16));
    let mut changed = original.clone();
    let tail = changed.len() - 17;
    changed[tail] ^= 1;
    std::fs::write(&segment, changed).unwrap();
    refused(verify(&c, &source, &output, &material.keys));
    std::fs::write(segment, original).unwrap();
    all_checks(&verify(&c, &source, &output, &material.keys).unwrap());
    assert_eq!(common::snapshot_tree(source.parent().unwrap()), before);
}

#[test]
fn master_audio_video_aliases_are_reconciled_independently_of_declaration_order() {
    let root = drmr_core::native::private_tempdir().unwrap();
    let input = shaka_source(&root.path().join("video"), "video", false);
    let directory = input.parent().unwrap();
    let original = common::snapshot_tree(directory);
    let master = directory.join("master.m3u8");
    let original_master = std::fs::read(&master).ok();
    let uri = input.file_name().unwrap().to_str().unwrap();
    let plain = format!("#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=100000\n{uri}\n");
    std::fs::write(&master, &plain).unwrap();
    let c = converter();
    all_checks(&verify(&c, &input, directory, &HlsInputKeys::default()).unwrap());
    let variant = format!("#EXT-X-STREAM-INF:BANDWIDTH=100000,AUDIO=\"g\"\n{uri}\n");
    let events = Arc::new(drmr_core::events::Collector::default());
    let guarded = Converter::new(
        Arc::new(ToolRunner::new(
            Arc::new(Secrets::default()),
            events.clone(),
        )),
        "packager",
        "ffmpeg",
    )
    .with_decode(true);
    let output = root.path().join("refused-output");
    let mut outcomes = Vec::new();
    for audio_uri in [uri.to_owned(), format!("./{uri}")] {
        let audio = format!(
            "#EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"g\",NAME=\"stream_0\",URI=\"{audio_uri}\"\n"
        );
        for declarations in [format!("{variant}{audio}"), format!("{audio}{variant}")] {
            std::fs::write(&master, format!("#EXTM3U\n{declarations}")).unwrap();
            let result = verify(&c, &input, directory, &HlsInputKeys::default())
                .map(|report| {
                    (
                        report.passed,
                        (report.checks.is_pass()
                            && report.decode_evidence.satisfies(
                                drmr_core::conversion::ValidationDepth::Decoded,
                                report.scope,
                            )),
                    )
                })
                .map_err(|error| error.0);
            outcomes.push(result);
            assert_eq!(
                convert(&guarded, &master, &output, &HlsInputKeys::default())
                    .unwrap_err()
                    .0,
                "hls-audio-association-not-supported"
            );
            assert_eq!(events.records().len(), 0);
            assert!(!output.exists());
        }
    }
    assert_eq!(outcomes, [Err("hls-audio-association-not-supported"); 4]);
    std::fs::write(&master, plain).unwrap();
    all_checks(&verify(&c, &input, directory, &HlsInputKeys::default()).unwrap());
    if let Some(bytes) = original_master {
        std::fs::write(master, bytes).unwrap();
    } else {
        std::fs::remove_file(master).unwrap();
    }
    assert!(
        common::snapshot_tree(directory) == original,
        "source bytes must remain unchanged"
    );
}
