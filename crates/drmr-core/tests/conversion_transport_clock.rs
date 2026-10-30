//! Manifested AAC clocks retain every original timestamp through native conversion.
mod common;

use drmr_core::{
    conversion::{
        ContentKey, ConversionReport, ConversionRequest, Converter, DecodeEvidence, HlsInputKeys,
        InputKeys, OutputKey, ProcessingMode, ValidationDepth, VerificationReport,
        VerificationRequest, VerificationScope, VerificationSource,
        plan::{self, ExecutionPlanRequest},
    },
    events::NoSink,
    inspection::{OutputFormat, TargetProtection},
    toolrun::{Secrets, ToolRunner},
};
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::Arc,
};

fn converter(mode: ProcessingMode, decode: bool) -> Converter {
    Converter::new(
        Arc::new(ToolRunner::new(
            Arc::new(Secrets::default()),
            Arc::new(NoSink),
        )),
        "packager",
        if decode {
            "ffmpeg"
        } else {
            "missing-native-clock-ffmpeg"
        },
    )
    .with_ffprobe(if decode {
        "ffprobe"
    } else {
        "missing-native-clock-ffprobe"
    })
    .with_processing(mode)
    .with_decode(decode)
}

fn run_fixture(command: &mut Command) {
    let result = command.output().unwrap();
    assert!(
        result.status.success(),
        "short licensed AAC fixture generation failed"
    );
}

fn source(directory: &Path, partial: bool, duration: &str) -> PathBuf {
    drmr_core::native::create_media_dir_all(directory).unwrap();
    let clip = Path::new(env!("CARGO_MANIFEST_DIR")).join("../drmr/tests/fixtures/bbb/clip.mp4");
    let transport = directory.join("source.ts");
    run_fixture(
        Command::new("ffmpeg")
            .args(["-v", "error", "-nostdin", "-threads", "1", "-i"])
            .arg(clip)
            .args([
                "-map", "0:a:0", "-vn", "-c:a", "aac", "-threads", "1", "-ar", "44100", "-t",
                duration, "-f", "mpegts",
            ])
            .arg(&transport),
    );
    let master = directory.join("master.m3u8");
    run_fixture(
        Command::new("packager")
            .arg("--single_threaded")
            .arg(format!(
                "in={},stream=audio,segment_template={},playlist_name=media.m3u8",
                transport.display(),
                directory.join("part-$Number$.ts").display()
            ))
            .arg("--hls_master_playlist_output")
            .arg(&master)
            .args([
                "--segment_duration",
                if partial { "1.06" } else { "2" },
                "--transport_stream_timestamp_offset_ms",
                "0",
            ]),
    );
    if partial {
        let playlist = directory.join("media.m3u8");
        let text = fs::read_to_string(&playlist).unwrap();
        let mut lines: Vec<_> = text.lines().map(str::to_owned).collect();
        let last = lines
            .iter()
            .rposition(|line| !line.is_empty() && !line.starts_with('#'))
            .unwrap();
        let frames = packet_clocks(&directory.join(&lines[last]), false).len();
        assert!(frames > 0);
        assert!(lines[last - 1].starts_with("#EXTINF:"));
        let samples = (frames - 1) * 1024 + 240;
        let seconds = f64::from(u32::try_from(samples).unwrap()) / 44_100_f64;
        let declared = seconds.to_string();
        assert!(declared.split_once('.').unwrap().1.len() > 9);
        lines[last - 1] = format!("#EXTINF:{declared},");
        fs::write(playlist, lines.join("\n") + "\n").unwrap();
    }
    master
}

fn source_view<'a>(
    path: &'a Path,
    empty: &'a InputKeys,
    hls: &'a HlsInputKeys,
) -> VerificationSource<'a> {
    VerificationSource {
        input: path,
        input_keys: empty,
        hls_keys: hls,
    }
}

fn assert_verified(report: &VerificationReport, decoded: bool) {
    assert!(report.passed && report.checks.is_pass(), "{report:?}");
    assert_eq!(
        report.required_depth,
        if decoded {
            ValidationDepth::Decoded
        } else {
            ValidationDepth::Native
        }
    );
    assert_eq!(report.scope, VerificationScope::Comparison);
    assert_eq!(
        report.decode_evidence,
        if decoded {
            DecodeEvidence::ContentCompared { equal: true }
        } else {
            DecodeEvidence::not_requested()
        }
    );
    assert!(!report.license_playback_verified);
}

fn assert_native_tools(
    measured: &drmr_core::conversion::measurement::MeasurementReport,
    allow_packager: bool,
) {
    let value = serde_json::to_value(measured).unwrap();
    for tool in value["tools"].as_array().unwrap() {
        if !allow_packager || tool["tool"] != "packager" {
            assert_eq!(tool["calls"], 0, "{tool:?}");
        }
    }
}

fn convert(
    command: &Converter,
    input: &Path,
    input_keys: &InputKeys,
    output: &Path,
    format: OutputFormat,
    target: TargetProtection,
) -> ConversionReport {
    convert_at_depth(command, input, input_keys, output, format, target, false)
}

fn convert_at_depth(
    command: &Converter,
    input: &Path,
    input_keys: &InputKeys,
    output: &Path,
    format: OutputFormat,
    target: TargetProtection,
    decoded: bool,
) -> ConversionReport {
    let output_key = (target == TargetProtection::Cbcs)
        .then(|| OutputKey::from_hex(common::KID, common::KEY).unwrap());
    let (result, measurement) = command.measure(|command| {
        command.convert(ConversionRequest {
            input,
            output,
            format,
            target,
            input_keys,
            hls_input_keys: &HlsInputKeys::default(),
            output_key: output_key.as_ref(),
            hls_key_uri: (format == OutputFormat::Hls && target == TargetProtection::Cbcs)
                .then_some("https://keys.example.invalid/transport-clock-cbcs.key"),
            hls_output_key: None,
        })
    });
    let report = result.unwrap();
    assert_verified(&report.verification, decoded);
    if decoded {
        let measured = serde_json::to_value(&measurement).unwrap();
        assert!(
            measured["tools"]
                .as_array()
                .unwrap()
                .iter()
                .any(|tool| { tool["tool"] == "ffmpeg" && tool["calls"].as_u64().unwrap() > 0 })
        );
    } else {
        assert_native_tools(
            &measurement,
            report.processing.as_ref().unwrap().mode != Some(ProcessingMode::Segments),
        );
    }
    report
}

fn entrypoint(directory: &Path, format: OutputFormat) -> PathBuf {
    directory.join(if format == OutputFormat::Dash {
        "manifest.mpd"
    } else {
        "master.m3u8"
    })
}

fn analyze(command: &Converter, input: &Path, keys: &InputKeys) {
    let (result, measured) = command.measure(|command| {
        command.analyze_source(source_view(input, keys, &HlsInputKeys::default()))
    });
    let analysis = result.unwrap();
    assert!(analysis.input_admitted && analysis.verification.passed);
    assert_eq!(
        analysis.verification.required_depth,
        ValidationDepth::Native
    );
    assert_eq!(
        analysis.verification.scope,
        VerificationScope::SourceAdmission
    );
    assert_eq!(
        analysis.verification.decode_evidence,
        DecodeEvidence::not_requested()
    );
    assert_native_tools(&measured, false);
}

fn verify(
    command: &Converter,
    original: &Path,
    output: &Path,
    output_keys: &InputKeys,
    decoded: bool,
) {
    let empty = InputKeys::default();
    let hls = HlsInputKeys::default();
    let (result, measurement) = command.measure(|command| {
        command.verify(VerificationRequest {
            source: source_view(original, &empty, &hls),
            output: source_view(output, output_keys, &hls),
        })
    });
    assert_verified(&result.unwrap(), decoded);
    if !decoded {
        assert_native_tools(&measurement, false);
    }
}

fn packet_clocks(input: &Path, mp4: bool) -> Vec<[i64; 2]> {
    let mut command = Command::new("ffprobe");
    command.args(["-v", "error", "-threads", "1"]);
    if mp4 {
        command.args(["-ignore_editlist", "1"]);
    }
    let result = command
        .args([
            "-select_streams",
            "a:0",
            "-show_packets",
            "-show_entries",
            "packet=pts,dts:stream=time_base",
            "-of",
            "json",
        ])
        .arg(input)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "independent packet clock inspection failed"
    );
    let value: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(value["streams"][0]["time_base"], "1/90000");
    value["packets"]
        .as_array()
        .unwrap()
        .iter()
        .map(|packet| {
            [
                packet["pts"].as_i64().unwrap(),
                packet["dts"].as_i64().unwrap(),
            ]
        })
        .collect()
}

fn media_resources(directory: &Path) -> Vec<PathBuf> {
    let mut pending = vec![directory.to_path_buf()];
    let mut resources = Vec::new();
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(directory).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                pending.push(path);
            } else if path.extension().is_some_and(|extension| extension == "m4s") {
                resources.push(path);
            }
        }
    }
    resources.sort_by_key(|path| {
        let stem = path.file_stem().unwrap().to_str().unwrap();
        stem.rsplit(|c: char| !c.is_ascii_digit())
            .next()
            .unwrap()
            .parse::<u64>()
            .unwrap()
    });
    assert_ne!(resources, [] as [PathBuf; 0]);
    resources
}

fn joined_output(directory: &Path, destination: &Path) {
    use std::io::Write as _;
    let media = media_resources(directory);
    let init = media[0].parent().unwrap().join("init.mp4");
    let mut file = drmr_core::native::create_private_new(destination).unwrap();
    file.write_all(&fs::read(init).unwrap()).unwrap();
    for segment in media {
        file.write_all(&fs::read(segment).unwrap()).unwrap();
    }
    file.flush().unwrap();
}

#[test]
fn repackaged_aac_transport_clocks_roundtrip_through_both_native_manifests() {
    let root = common::private_tempdir().unwrap();
    let input = source(&root.path().join("source"), false, "4.25");
    let original = common::snapshot_tree(input.parent().unwrap());
    let empty = InputKeys::default();
    let encrypted_keys = InputKeys::new(vec![
        ContentKey::from_hex(common::KID, common::KEY).unwrap(),
    ])
    .unwrap();
    let stitch = converter(ProcessingMode::Stitch, false);
    let segments = converter(ProcessingMode::Segments, false);
    let expected = packet_clocks(&input, false);
    assert!(expected.len() > 14);
    analyze(&stitch, &input, &empty);
    for format in [OutputFormat::Hls, OutputFormat::Dash] {
        let output = root.path().join(format!("initial-{}", format.as_str()));
        convert(
            &stitch,
            &input,
            &empty,
            &output,
            format,
            TargetProtection::Clear,
        );
        let manifest = entrypoint(&output, format);
        assert!(media_resources(&output).len() > 1);
        verify(&stitch, &input, &manifest, &empty, false);
        analyze(&stitch, &manifest, &empty);
        let joined = root.path().join(format!("raw-{}.mp4", format.as_str()));
        joined_output(&output, &joined);
        assert_eq!(packet_clocks(&joined, true), expected);
        let other = if format == OutputFormat::Dash {
            OutputFormat::Hls
        } else {
            OutputFormat::Dash
        };
        let roundtrip = root.path().join(format!("repack-{}", format.as_str()));
        convert(
            &stitch,
            &manifest,
            &empty,
            &roundtrip,
            other,
            TargetProtection::Clear,
        );
        verify(
            &stitch,
            &input,
            &entrypoint(&roundtrip, other),
            &empty,
            false,
        );
        let encrypted = root.path().join(format!("cbcs-{}", format.as_str()));
        convert(
            &segments,
            &manifest,
            &empty,
            &encrypted,
            format,
            TargetProtection::Cbcs,
        );
        let encrypted_manifest = entrypoint(&encrypted, format);
        analyze(&segments, &encrypted_manifest, &encrypted_keys);
        verify(
            &segments,
            &manifest,
            &encrypted_manifest,
            &encrypted_keys,
            false,
        );
        verify(&stitch, &input, &encrypted_manifest, &encrypted_keys, false);
        let clear = root.path().join(format!("clear-{}", format.as_str()));
        convert(
            &segments,
            &encrypted_manifest,
            &encrypted_keys,
            &clear,
            format,
            TargetProtection::Clear,
        );
        verify(
            &segments,
            &manifest,
            &entrypoint(&clear, format),
            &empty,
            false,
        );
        verify(&stitch, &input, &entrypoint(&clear, format), &empty, false);
    }
    assert_eq!(common::snapshot_tree(input.parent().unwrap()), original);
}

#[test]
fn repackaged_partial_aac_keeps_native_windows_and_complete_decoded_content() {
    let root = common::private_tempdir().unwrap();
    let input = source(&root.path().join("source"), true, "1.05");
    let original = common::snapshot_tree(input.parent().unwrap());
    let empty = InputKeys::default();
    let stitch = converter(ProcessingMode::Stitch, false);
    let decoded = converter(ProcessingMode::Stitch, true);
    analyze(&stitch, &input, &empty);
    for format in [OutputFormat::Hls, OutputFormat::Dash] {
        let output = root.path().join(format!("partial-{}", format.as_str()));
        convert(
            &stitch,
            &input,
            &empty,
            &output,
            format,
            TargetProtection::Clear,
        );
        let manifest = entrypoint(&output, format);
        verify(&stitch, &input, &manifest, &empty, false);
        analyze(&stitch, &manifest, &empty);
        verify(&decoded, &input, &manifest, &empty, true);
        let intermediate = common::snapshot_tree(&output);
        for additional_decode in [false, true] {
            let depth = if additional_decode {
                "decoded"
            } else {
                "native"
            };
            let repacked = root
                .path()
                .join(format!("partial-repacked-{}-{depth}", format.as_str()));
            let command = if additional_decode { &decoded } else { &stitch };
            convert_at_depth(
                command,
                &manifest,
                &empty,
                &repacked,
                format,
                TargetProtection::Clear,
                additional_decode,
            );
            let repacked_manifest = entrypoint(&repacked, format);
            verify(&stitch, &input, &repacked_manifest, &empty, false);
            verify(&decoded, &input, &repacked_manifest, &empty, true);
            assert_eq!(common::snapshot_tree(&output), intermediate);
        }
    }
    assert_eq!(common::snapshot_tree(input.parent().unwrap()), original);
}

// This test oracle works only on the short fixture's integer clocks. It does
// not use the native validator or excuse any difference between two files.
fn has_one_bounded_transport_phase(clocks: &[[i64; 2]]) -> bool {
    let mut low = i128::MIN;
    let mut high = i128::MAX;
    for (ordinal, clock) in clocks.iter().enumerate() {
        if clock[0] != clock[1] {
            return false;
        }
        let offset =
            i128::from(clock[0]) * 44_100 - i128::try_from(ordinal).unwrap() * 1024 * 90_000;
        low = low.max(offset - 44_100);
        high = high.min(offset + 44_100);
    }
    low < high
}

struct ClockMutation {
    path: PathBuf,
    original: Vec<u8>,
    expected: Vec<[i64; 2]>,
}

fn mutate_one_interior_timestamp(directory: &Path, clocks: &[[i64; 2]]) -> ClockMutation {
    use drmr_core::mp4;
    assert!(has_one_bounded_transport_phase(clocks));
    let mut ordinal = 0;
    for path in media_resources(directory) {
        let original = fs::read(&path).unwrap();
        let boxes = mp4::parse_boxes(&original).unwrap();
        for node in mp4::walk(&boxes)
            .into_iter()
            .filter(|node| node.kind() == *b"trun")
        {
            let run = mp4::Trun::read(&original, &node.header).unwrap();
            let count = usize::try_from(run.sample_count).unwrap();
            assert!(count > 0);
            assert_ne!(
                run.flags & 0x100,
                0,
                "fixture has nonuniform sample durations"
            );
            let fields = [0x100, 0x200, 0x400, 0x800]
                .into_iter()
                .filter(|bit| run.flags & bit != 0)
                .count();
            let records = node.header.offset
                + node.header.header_size
                + 8
                + usize::from(run.flags & 1 != 0) * 4
                + usize::from(run.flags & 4 != 0) * 4;
            let stride = fields * 4;
            for index in 1..count.saturating_sub(1) {
                let selected = ordinal + index;
                for delta in [-1_i64, 1] {
                    let mut expected = clocks.to_vec();
                    expected[selected][0] += delta;
                    expected[selected][1] += delta;
                    if !has_one_bounded_transport_phase(&expected) {
                        continue;
                    }
                    let mut changed = original.clone();
                    for (at, delta) in [
                        (records + (index - 1) * stride, delta),
                        (records + index * stride, -delta),
                    ] {
                        let old = u32::from_be_bytes(changed[at..at + 4].try_into().unwrap());
                        let new = u32::try_from(i64::from(old) + delta).unwrap();
                        assert!(new > 0);
                        changed[at..at + 4].copy_from_slice(&new.to_be_bytes());
                    }
                    let unchanged_payload = boxes
                        .iter()
                        .filter(|node| node.kind() == *b"mdat")
                        .all(|node| node.header.bytes(&changed) == node.header.bytes(&original));
                    assert!(unchanged_payload);
                    assert_eq!(changed.len(), original.len());
                    fs::write(&path, changed).unwrap();
                    return ClockMutation {
                        path,
                        original,
                        expected,
                    };
                }
            }
            ordinal += count;
        }
    }
    panic!("fixture must expose an interior timestamp inside the admitted clock family");
}

fn assert_rejected(command: &Converter, original: &Path, result: &Path) {
    let empty = InputKeys::default();
    let hls = HlsInputKeys::default();
    let result = command.verify(VerificationRequest {
        source: source_view(original, &empty, &hls),
        output: source_view(result, &empty, &hls),
    });
    assert!(match result {
        Ok(report) => !report.passed,
        Err(_) => true,
    });
}

#[test]
fn admitted_aac_clock_rounding_never_tolerates_a_changed_output_timestamp_or_payload() {
    let root = common::private_tempdir().unwrap();
    let input = source(&root.path().join("source"), false, "1.05");
    let original_source = common::snapshot_tree(input.parent().unwrap());
    let empty = InputKeys::default();
    let hls = HlsInputKeys::default();
    let native = converter(ProcessingMode::Stitch, false);
    for format in [OutputFormat::Hls, OutputFormat::Dash] {
        let output = root.path().join(format!("negative-{}", format.as_str()));
        convert(
            &native,
            &input,
            &empty,
            &output,
            format,
            TargetProtection::Clear,
        );
        let manifest = entrypoint(&output, format);
        let joined = root.path().join(format!("before-{}.mp4", format.as_str()));
        joined_output(&output, &joined);
        let clocks = packet_clocks(&joined, true);
        let changed = mutate_one_interior_timestamp(&output, &clocks);
        let joined_changed = root.path().join(format!("changed-{}.mp4", format.as_str()));
        joined_output(&output, &joined_changed);
        let observed = packet_clocks(&joined_changed, true);
        assert_eq!(observed, changed.expected);
        assert_eq!(observed.len(), clocks.len());
        assert_eq!(observed.first(), clocks.first());
        assert_eq!(observed.last(), clocks.last());
        assert_eq!(
            observed.iter().zip(&clocks).filter(|(a, b)| a != b).count(),
            1
        );
        // An individually valid source clock does not authorize a changed result.
        analyze(&native, &manifest, &empty);
        let comparison = native
            .verify(VerificationRequest {
                source: source_view(&input, &empty, &hls),
                output: source_view(&manifest, &empty, &hls),
            })
            .unwrap();
        assert!(!comparison.passed);
        assert!(comparison.checks.packet_payloads_equal);
        assert!(!comparison.checks.packet_timing_equal);
        fs::write(&changed.path, &changed.original).unwrap();
        verify(&native, &input, &manifest, &empty, false);

        let mut payload = changed.original.clone();
        let boxes = drmr_core::mp4::parse_boxes(&payload).unwrap();
        let media = boxes.iter().find(|node| node.kind() == *b"mdat").unwrap();
        let at = media.header.offset + media.header.header_size;
        payload[at] ^= 1;
        fs::write(&changed.path, payload).unwrap();
        assert_rejected(&native, &input, &manifest);
        fs::write(&changed.path, &changed.original).unwrap();
        fs::remove_file(&changed.path).unwrap();
        assert_rejected(&native, &input, &manifest);
        fs::write(&changed.path, changed.original).unwrap();
        verify(&native, &input, &manifest, &empty, false);
    }
    assert_eq!(
        common::snapshot_tree(input.parent().unwrap()),
        original_source
    );
}

fn strict_clock_values(clocks: &[[i64; 2]]) -> Vec<i64> {
    let first = clocks[0][0];
    let last = clocks.last().unwrap()[0];
    let frame_ticks = 1024_i128 * 90_000;
    let last_exact = i128::try_from(clocks.len() - 1).unwrap() * frame_ticks;
    let low = 0_i128.max(i128::from(last - first) * 44_100 - last_exact);
    let high = 44_100_i128.min(i128::from(last - first + 1) * 44_100 - last_exact);
    assert!(low < high);
    let strict: Vec<_> = (0..clocks.len())
        .map(|index| {
            first
                + i64::try_from((i128::try_from(index).unwrap() * frame_ticks + low) / 44_100)
                    .unwrap()
        })
        .collect();
    assert_eq!(strict.first().copied(), Some(first));
    assert_eq!(strict.last().copied(), Some(last));
    assert!(strict.iter().zip(clocks).any(|(a, b)| *a != b[0]));
    strict
}

fn strict_clock_control(output: &Path, destination: &Path, clocks: &[[i64; 2]]) -> PathBuf {
    use drmr_core::mp4;
    drmr_core::native::create_media_dir_all(destination).unwrap();
    let resources = media_resources(output);
    fs::copy(
        resources[0].parent().unwrap().join("init.mp4"),
        destination.join("init.mp4"),
    )
    .unwrap();
    let mut data: Vec<_> = resources
        .iter()
        .flat_map(|path| fs::read(path).unwrap())
        .collect();
    let original_data = data.clone();
    let boxes = mp4::parse_boxes(&data).unwrap();
    let mut durations = Vec::new();
    let mut bases = Vec::new();
    for fragment in boxes.iter().filter(|node| node.kind() == *b"moof") {
        let track = fragment
            .children
            .iter()
            .find(|node| node.kind() == *b"traf")
            .unwrap();
        let base = track
            .children
            .iter()
            .find(|node| node.kind() == *b"tfdt")
            .unwrap();
        bases.push((base.header, durations.len()));
        for node in track.children.iter().filter(|node| node.kind() == *b"trun") {
            let run = mp4::Trun::read(&data, &node.header).unwrap();
            assert_ne!(run.flags & 0x100, 0);
            let fields = [0x100, 0x200, 0x400, 0x800]
                .into_iter()
                .filter(|bit| run.flags & bit != 0)
                .count();
            let start = node.header.offset
                + node.header.header_size
                + 8
                + usize::from(run.flags & 1 != 0) * 4
                + usize::from(run.flags & 4 != 0) * 4;
            durations.extend(
                (0..usize::try_from(run.sample_count).unwrap())
                    .map(|index| start + index * fields * 4),
            );
        }
    }
    assert_eq!(durations.len(), clocks.len());
    let first = clocks[0][0];
    let last = clocks.last().unwrap()[0];
    let end = last
        + i64::from(u32::from_be_bytes(
            data[*durations.last().unwrap()..*durations.last().unwrap() + 4]
                .try_into()
                .unwrap(),
        ));
    let strict = strict_clock_values(clocks);
    for (header, ordinal) in bases {
        let at = header.offset + header.header_size;
        let ticks = u64::try_from(strict[ordinal]).unwrap();
        match data[at] {
            0 => data[at + 4..at + 8].copy_from_slice(&u32::try_from(ticks).unwrap().to_be_bytes()),
            1 => data[at + 4..at + 12].copy_from_slice(&ticks.to_be_bytes()),
            _ => panic!("fixture decode-time version"),
        }
    }
    for (index, at) in durations.iter().enumerate() {
        let next = strict.get(index + 1).copied().unwrap_or(end);
        let duration = u32::try_from(next - strict[index]).unwrap();
        assert!(duration > 0);
        data[*at..*at + 4].copy_from_slice(&duration.to_be_bytes());
    }
    assert!(
        boxes
            .iter()
            .filter(|node| node.kind() == *b"mdat")
            .all(|node| { node.header.bytes(&data) == node.header.bytes(&original_data) })
    );
    fs::write(destination.join("segment-1.m4s"), data).unwrap();
    let span = u128::try_from(end - first).unwrap();
    let nanoseconds = (span * 1_000_000_000 + 45_000) / 90_000;
    let duration = format!(
        "{}.{:09}",
        nanoseconds / 1_000_000_000,
        nanoseconds % 1_000_000_000
    );
    let playlist = destination.join("media.m3u8");
    fs::write(&playlist, format!("#EXTM3U\n#EXT-X-VERSION:7\n#EXT-X-TARGETDURATION:2\n#EXT-X-MEDIA-SEQUENCE:0\n#EXT-X-PLAYLIST-TYPE:VOD\n#EXT-X-MAP:URI=\"init.mp4\"\n#EXTINF:{duration},\nsegment-1.m4s\n#EXT-X-ENDLIST\n")).unwrap();
    playlist
}

#[test]
fn mp4_eligibility_depends_on_actual_aac_clocks_not_only_sample_rate_and_timescale() {
    let root = common::private_tempdir().unwrap();
    let input = source(&root.path().join("source"), false, "1.05");
    let original = common::snapshot_tree(input.parent().unwrap());
    let empty = InputKeys::default();
    let hls = HlsInputKeys::default();
    let native = converter(ProcessingMode::Stitch, false);
    let output = root.path().join("hls");
    convert(
        &native,
        &input,
        &empty,
        &output,
        OutputFormat::Hls,
        TargetProtection::Clear,
    );
    let bare = output.join("track-0000/media.m3u8");
    let analysis = native
        .analyze_source(source_view(&bare, &empty, &hls))
        .unwrap();
    assert!(analysis.input_admitted);
    assert!(!analysis.eligible_formats.contains(&OutputFormat::Mp4));
    let joined = root.path().join("two-step.mp4");
    joined_output(&output, &joined);
    let joined_before = fs::read(&joined).unwrap();
    let joined_analysis = native
        .analyze_source(source_view(&joined, &empty, &hls))
        .unwrap();
    assert!(joined_analysis.input_admitted);
    assert_eq!(joined_analysis.actual_format, OutputFormat::Mp4);
    assert!(joined_analysis.verification.passed);
    assert!(joined_analysis.verification.checks.is_pass());
    assert_eq!(
        joined_analysis.verification.required_depth,
        ValidationDepth::Native
    );
    assert_eq!(
        joined_analysis.verification.scope,
        VerificationScope::SourceAdmission
    );
    assert!(
        !joined_analysis
            .eligible_formats
            .contains(&OutputFormat::Mp4)
    );
    assert_eq!(joined_analysis.eligible_formats, analysis.eligible_formats);
    let unsupported_output = root.path().join("unsupported.mp4");
    let unsupported_plan = root.path().join("unsupported.plan.json");
    let error = native
        .create_execution_plan(ExecutionPlanRequest {
            input: source_view(&joined, &empty, &hls),
            output: &unsupported_output,
            format: Some(OutputFormat::Mp4),
            target: TargetProtection::Clear,
        })
        .and_then(|planned| plan::write_new(&unsupported_plan, &planned))
        .unwrap_err();
    assert_eq!(error.0, "execution-plan-not-supported");
    assert!(!unsupported_plan.exists());
    assert!(!unsupported_output.exists());
    assert_eq!(
        fs::read(&joined).unwrap(),
        joined_before,
        "source admission and refused planning must not normalize the input clocks"
    );
    let clocks = packet_clocks(&joined, true);
    let control = strict_clock_control(&output, &root.path().join("strict-control"), &clocks);
    let analysis = native
        .analyze_source(source_view(&control, &empty, &hls))
        .unwrap();
    assert!(analysis.input_admitted);
    assert!(analysis.eligible_formats.contains(&OutputFormat::Mp4));
    let joined_control = root.path().join("strict-control.mp4");
    joined_output(control.parent().unwrap(), &joined_control);
    let actual = packet_clocks(&joined_control, true);
    assert_eq!(actual.len(), clocks.len());
    assert_eq!(actual.first(), clocks.first());
    assert_eq!(actual.last(), clocks.last());
    assert_ne!(actual, clocks);
    let joined_control_analysis = native
        .analyze_source(source_view(&joined_control, &empty, &hls))
        .unwrap();
    assert!(joined_control_analysis.input_admitted);
    assert!(
        joined_control_analysis
            .eligible_formats
            .contains(&OutputFormat::Mp4)
    );
    assert_eq!(common::snapshot_tree(input.parent().unwrap()), original);
}

#[test]
fn optional_hls_channels_survive_dash_roundtrip_with_actual_codec_validation() {
    let root = common::private_tempdir().unwrap();
    let input = source(&root.path().join("source"), false, "1.05");
    let master = fs::read_to_string(&input).unwrap();
    assert_eq!(master.matches(",CHANNELS=\"2\"").count(), 1);
    fs::write(&input, master.replace(",CHANNELS=\"2\"", "")).unwrap();
    let original = common::snapshot_tree(input.parent().unwrap());
    let empty = InputKeys::default();
    let hls_keys = HlsInputKeys::default();
    let native = converter(ProcessingMode::Stitch, false);
    analyze(&native, &input, &empty);
    let dash = root.path().join("dash");
    convert(
        &native,
        &input,
        &empty,
        &dash,
        OutputFormat::Dash,
        TargetProtection::Clear,
    );
    let hls = root.path().join("hls");
    convert(
        &native,
        &entrypoint(&dash, OutputFormat::Dash),
        &empty,
        &hls,
        OutputFormat::Hls,
        TargetProtection::Clear,
    );
    let manifest = entrypoint(&hls, OutputFormat::Hls);
    let actual = fs::read_to_string(&manifest).unwrap();
    assert_eq!(actual.matches("CHANNELS=\"2\"").count(), 1);
    analyze(&native, &manifest, &empty);
    verify(&native, &input, &manifest, &empty, false);

    fs::write(
        &manifest,
        actual.replace("CHANNELS=\"2\"", "CHANNELS=\"1\""),
    )
    .unwrap();
    let error = native
        .analyze_source(source_view(&manifest, &empty, &hls_keys))
        .unwrap_err();
    assert_eq!(error.0, "presentation-metadata-mismatch");
    assert_rejected(&native, &input, &manifest);
    fs::write(&manifest, actual).unwrap();
    verify(&native, &input, &manifest, &empty, false);
    assert_eq!(common::snapshot_tree(input.parent().unwrap()), original);
}
