//! A declared audio presentation can end inside the final complete coded AAC frame.
use super::audio_hls::{join_resources, packet_clocks};
use super::*;
use num_rational::Ratio;
use sha2::{Digest as _, Sha256};

const RAW_FRAMES: usize = 432;
const RAW_SAMPLES: usize = RAW_FRAMES * 1024;
const FINAL_PRESENTED_SAMPLES: usize = 240;
const PRESENTED_SAMPLES: usize = (RAW_FRAMES - 1) * 1024 + FINAL_PRESENTED_SAMPLES;

#[derive(Clone, Copy)]
struct AudioProfile {
    rate: u32,
    frames: usize,
    final_samples: usize,
    frequency_index: u8,
    movie_scale: u32,
    ticks_per_ts_tick: u64,
    ticks_per_sample: u64,
}
const AAC_44100: AudioProfile = AudioProfile {
    rate: 44_100,
    frames: 432,
    final_samples: 240,
    frequency_index: 4,
    movie_scale: 4_410_000,
    ticks_per_ts_tick: 49,
    ticks_per_sample: 100,
};
const AAC_48000: AudioProfile = AudioProfile {
    rate: 48_000,
    frames: 470,
    final_samples: 240,
    frequency_index: 3,
    movie_scale: 720_000,
    ticks_per_ts_tick: 8,
    ticks_per_sample: 15,
};
impl AudioProfile {
    fn raw_samples(self) -> usize {
        self.frames * 1024
    }
    fn presented_samples(self) -> usize {
        (self.frames - 1) * 1024 + self.final_samples
    }
}

fn long_decimal(value: f64) -> String {
    let mut text = value.to_string();
    if !text.contains('.') {
        text.push('.');
    }
    while text.split_once('.').unwrap().1.len() <= 9 {
        text.push('0');
    }
    assert_eq!(text.parse::<f64>().unwrap().to_bits(), value.to_bits());
    text
}

#[derive(Debug, PartialEq, Eq)]
struct RawAudio {
    aac_frames: usize,
    aac_sha256: String,
    pcm_samples_per_channel: usize,
    pcm_sha256: String,
    presented_pcm_sha256: String,
}

fn ffmpeg_audio(path: &Path, mp4: bool, decode: bool, profile: AudioProfile) -> Vec<u8> {
    let mut argv = vec![
        "ffmpeg".into(),
        "-v".into(),
        "error".into(),
        "-nostdin".into(),
        "-threads".into(),
        "1".into(),
    ];
    if mp4 {
        // The raw coded-media oracle deliberately bypasses MP4 edit lists.
        argv.extend(["-ignore_editlist".into(), "1".into()]);
        if decode {
            // Include codec delay and discard padding in the complete coded-frame oracle.
            argv.extend(["-flags2".into(), "+skip_manual".into()]);
        }
    }
    argv.extend([
        "-i".into(),
        drmr_core::manifest_support::path::fspath(path).unwrap(),
        "-map".into(),
        "0:a:0".into(),
        "-c:a".into(),
        if decode { "pcm_s32le" } else { "copy" }.into(),
        "-f".into(),
        if decode { "s32le" } else { "adts" }.into(),
        "-".into(),
    ]);
    let result = ToolRunner::new(Arc::new(Secrets::default()), Arc::new(NoSink))
        .run_tool(
            &argv,
            "ffmpeg",
            &drmr_core::toolrun::RunOptions {
                timeout: Some(std::time::Duration::from_secs(30)),
                max_stdout_bytes: profile.raw_samples() * 2 * 4,
                max_output_chars: 0,
                log_stdout: false,
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(result.returncode, Some(0));
    result.stdout
}

fn raw_audio(path: &Path, mp4: bool, profile: AudioProfile) -> RawAudio {
    let aac = ffmpeg_audio(path, mp4, false, profile);
    let mut position = 0;
    let mut frames = 0;
    while position < aac.len() {
        let header = aac.get(position..position + 7).unwrap();
        assert_eq!(header[0], 0xff);
        assert_eq!(header[1] & 0xf6, 0xf0);
        assert_eq!(header[2] >> 6, 1);
        assert_eq!((header[2] >> 2) & 15, profile.frequency_index);
        assert_eq!(((header[2] & 1) << 2) | (header[3] >> 6), 2);
        assert_eq!(header[6] & 3, 0);
        let length = (usize::from(header[3] & 3) << 11)
            | (usize::from(header[4]) << 3)
            | usize::from(header[5] >> 5);
        assert!(length > 7 && position + length <= aac.len());
        position += length;
        frames += 1;
    }
    assert_eq!(position, aac.len());
    let pcm = ffmpeg_audio(path, mp4, true, profile);
    assert_eq!(pcm.len(), profile.raw_samples() * 2 * 4);
    RawAudio {
        aac_frames: frames,
        aac_sha256: hex::encode(Sha256::digest(&aac)),
        pcm_samples_per_channel: pcm.len() / (2 * 4),
        pcm_sha256: hex::encode(Sha256::digest(&pcm)),
        presented_pcm_sha256: hex::encode(Sha256::digest(
            &pcm[..profile.presented_samples() * 2 * 4],
        )),
    }
}

fn shorten_final_declaration(source: &Path, profile: AudioProfile) -> Vec<[Ratio<i64>; 3]> {
    let original = std::fs::read_to_string(source).unwrap();
    let mut lines: Vec<_> = original.lines().map(str::to_owned).collect();
    let resources: Vec<_> = lines
        .iter()
        .enumerate()
        .filter(|(_, line)| !line.is_empty() && !line.starts_with('#'))
        .map(|(index, name)| {
            assert!(!name.contains(['/', '\\', ':']));
            (index, source.parent().unwrap().join(name))
        })
        .collect();
    assert_eq!(resources.len(), 6);
    let mut packets = Vec::new();
    for (index, (_, segment)) in resources.iter().enumerate() {
        let observed = packet_clocks(segment);
        assert_ne!(observed.len(), 0);
        if index == resources.len() - 1 {
            assert_eq!(
                observed.len(),
                1,
                "the final resource must contain one complete AAC frame"
            );
        }
        packets.extend(observed);
    }
    assert_eq!(packets.len(), profile.frames);
    let at = resources.last().unwrap().0 - 1;
    assert!(lines[at].starts_with("#EXTINF:"));
    assert!((1..1024).contains(&profile.final_samples));
    let seconds =
        f64::from(u32::try_from(profile.final_samples).unwrap()) / f64::from(profile.rate);
    let declared = long_decimal(seconds);
    assert!(declared.split_once('.').unwrap().1.len() > 9);
    assert_eq!(
        declared.parse::<f64>().unwrap().to_bits(),
        seconds.to_bits()
    );
    lines[at] = format!("#EXTINF:{declared},");
    let changed: Vec<_> = original
        .lines()
        .zip(&lines)
        .enumerate()
        .filter_map(|(index, (before, after))| (before != after).then_some(index))
        .collect();
    assert_eq!(changed, [at]);
    std::fs::write(source, lines.join("\n") + "\n").unwrap();
    let last = packets.last().unwrap();
    let presented_end = last[0]
        + Ratio::new(
            i64::try_from(profile.final_samples).unwrap(),
            i64::from(profile.rate),
        );
    assert!(presented_end < last[0] + last[2]);
    assert!(packets[0][0] >= Ratio::from_integer(0));
    packets
}

fn join_dash_track(output: &Path, destination: &Path) {
    join_dash_track_named(output, "track-0000", destination);
}

fn join_dash_track_named(output: &Path, track: &str, destination: &Path) {
    use std::io::Write as _;
    assert!(track.starts_with("track-") && !track.contains(['/', '\\', ':']));
    let directory = output.join(track);
    let mut segments: Vec<_> = std::fs::read_dir(&directory)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|extension| extension == "m4s"))
        .collect();
    segments.sort_by_key(|path| {
        path.file_stem()
            .unwrap()
            .to_str()
            .unwrap()
            .strip_prefix("seg_")
            .unwrap()
            .parse::<usize>()
            .unwrap()
    });
    assert!(segments.len() > 1);
    let mut file = drmr_core::native::create_private_new(destination).unwrap();
    file.write_all(&std::fs::read(directory.join("init.mp4")).unwrap())
        .unwrap();
    for segment in segments {
        file.write_all(&std::fs::read(segment).unwrap()).unwrap();
    }
}

fn raw_mp4_packet_clocks(path: &Path) -> Vec<[Ratio<i64>; 3]> {
    observed_packet_clocks(path, "a:0", true)
}

fn observed_packet_clocks(path: &Path, selector: &str, mp4: bool) -> Vec<[Ratio<i64>; 3]> {
    let mut command = std::process::Command::new("ffprobe");
    command.args(["-v", "error", "-threads", "1"]);
    if mp4 {
        command.args(["-ignore_editlist", "1"]);
    }
    let result = command
        .args([
            "-select_streams",
            selector,
            "-show_packets",
            "-show_entries",
            "packet=pts,dts,duration:stream=time_base",
            "-of",
            "json",
        ])
        .arg(path)
        .output()
        .unwrap();
    assert!(result.status.success());
    let report: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
    let (n, d) = report["streams"][0]["time_base"]
        .as_str()
        .unwrap()
        .split_once('/')
        .unwrap();
    let scale = Ratio::new(n.parse::<i64>().unwrap(), d.parse::<i64>().unwrap());
    report["packets"]
        .as_array()
        .unwrap()
        .iter()
        .map(|packet| {
            ["pts", "dts", "duration"].map(|field| scale * packet[field].as_i64().unwrap())
        })
        .collect()
}

fn duration(text: &str) -> Ratio<i128> {
    let value = text.strip_prefix("PT").unwrap().strip_suffix('S').unwrap();
    decimal(value)
}

fn decimal(value: &str) -> Ratio<i128> {
    let (whole, fraction) = value.split_once('.').unwrap_or((value, ""));
    let scale = 10_i128.pow(u32::try_from(fraction.len()).unwrap());
    let part = if fraction.is_empty() {
        0
    } else {
        fraction.parse::<i128>().unwrap()
    };
    Ratio::new(whole.parse::<i128>().unwrap() * scale + part, scale)
}

#[derive(Clone, Copy)]
struct BoxRange {
    start: usize,
    payload: usize,
    end: usize,
    kind: [u8; 4],
}

fn boxes(data: &[u8], start: usize, end: usize) -> Vec<BoxRange> {
    let mut result = Vec::new();
    let mut at = start;
    while at < end {
        let length = u32::from_be_bytes(data[at..at + 4].try_into().unwrap());
        let (size, header) = match length {
            0 => (end - at, 8),
            1 => (
                usize::try_from(u64::from_be_bytes(
                    data[at + 8..at + 16].try_into().unwrap(),
                ))
                .unwrap(),
                16,
            ),
            _ => (usize::try_from(length).unwrap(), 8),
        };
        assert!(size >= header && at + size <= end);
        result.push(BoxRange {
            start: at,
            payload: at + header,
            end: at + size,
            kind: data[at + 4..at + 8].try_into().unwrap(),
        });
        at += size;
    }
    assert_eq!(at, end);
    result
}

fn one_box(data: &[u8], path: &[[u8; 4]]) -> BoxRange {
    let mut start = 0;
    let mut end = data.len();
    let mut found = None;
    for kind in path {
        let matched: Vec<_> = boxes(data, start, end)
            .into_iter()
            .filter(|item| item.kind == *kind)
            .collect();
        assert_eq!(matched.len(), 1);
        let item = matched[0];
        start = item.payload;
        end = item.end;
        found = Some(item);
    }
    found.unwrap()
}

fn header_scale(data: &[u8], item: BoxRange) -> u32 {
    let at = item.payload
        + match data[item.payload] {
            0 => 12,
            1 => 20,
            _ => panic!("unknown MP4 header version"),
        };
    u32::from_be_bytes(data[at..at + 4].try_into().unwrap())
}

#[derive(Clone, Copy)]
struct Edit {
    duration: u64,
    media_time: i64,
    rate_integer: u16,
    rate_fraction: u16,
    duration_at: usize,
    media_at: usize,
    width: usize,
}

fn edits(data: &[u8]) -> (BoxRange, Vec<Edit>) {
    let item = one_box(data, &[*b"moov", *b"trak", *b"edts", *b"elst"]);
    let width = match data[item.payload] {
        0 => 4,
        1 => 8,
        _ => panic!("unknown edit-list version"),
    };
    assert_eq!(&data[item.payload + 1..item.payload + 4], [0; 3]);
    let count = usize::try_from(u32::from_be_bytes(
        data[item.payload + 4..item.payload + 8].try_into().unwrap(),
    ))
    .unwrap();
    let stride = width * 2 + 4;
    assert_eq!(item.payload + 8 + count * stride, item.end);
    let entries = (0..count)
        .map(|index| {
            let at = item.payload + 8 + index * stride;
            let (duration, media_time) = if width == 8 {
                (
                    u64::from_be_bytes(data[at..at + 8].try_into().unwrap()),
                    i64::from_be_bytes(data[at + 8..at + 16].try_into().unwrap()),
                )
            } else {
                (
                    u64::from(u32::from_be_bytes(data[at..at + 4].try_into().unwrap())),
                    i64::from(i32::from_be_bytes(data[at + 4..at + 8].try_into().unwrap())),
                )
            };
            Edit {
                duration,
                media_time,
                rate_integer: u16::from_be_bytes(
                    data[at + 2 * width..at + 2 * width + 2].try_into().unwrap(),
                ),
                rate_fraction: u16::from_be_bytes(
                    data[at + 2 * width + 2..at + stride].try_into().unwrap(),
                ),
                duration_at: at,
                media_at: at + width,
                width,
            }
        })
        .collect();
    (item, entries)
}

fn assert_two_standard_edits(init: &Path, profile: AudioProfile, raw_first: Ratio<i64>) {
    let data = std::fs::read(init).unwrap();
    assert_eq!(
        header_scale(&data, one_box(&data, &[*b"moov", *b"mvhd"])),
        profile.movie_scale
    );
    assert_eq!(
        header_scale(
            &data,
            one_box(&data, &[*b"moov", *b"trak", *b"mdia", *b"mdhd"])
        ),
        90_000
    );
    let ticks = raw_first * Ratio::from_integer(90_000);
    assert_eq!(*ticks.denom(), 1);
    let origin = u64::try_from(ticks.to_integer()).unwrap();
    let (_, items) = edits(&data);
    assert_eq!(items.len(), 2);
    assert_eq!(items[0].duration, origin * profile.ticks_per_ts_tick);
    assert_eq!(items[0].media_time, -1);
    assert_eq!(
        items[1].duration,
        u64::try_from(profile.presented_samples()).unwrap() * profile.ticks_per_sample
    );
    assert_eq!(items[1].media_time, i64::try_from(origin).unwrap());
    for item in items {
        assert_eq!((item.rate_integer, item.rate_fraction), (1, 0));
    }
}

fn assert_short_dash_presentation(manifest: &Path, profile: AudioProfile) {
    let span = Ratio::new(
        i64::try_from(profile.presented_samples()).unwrap(),
        i64::from(profile.rate),
    );
    assert_dash_span(manifest, span);
}

fn assert_dash_span(manifest: &Path, span: Ratio<i64>) {
    let text = std::fs::read_to_string(manifest).unwrap();
    let doc = roxmltree::Document::parse(&text).unwrap();
    let numerator = i128::from(*span.numer()) * 1_000_000_000;
    let denominator = i128::from(*span.denom());
    let nanoseconds = (numerator + denominator - 1) / denominator;
    let expected = Ratio::new(nanoseconds, 1_000_000_000);
    assert_eq!(
        duration(
            doc.root_element()
                .attribute("mediaPresentationDuration")
                .unwrap()
        ),
        expected
    );
    let period = doc
        .descendants()
        .find(|node| node.has_tag_name("Period"))
        .unwrap();
    assert_eq!(duration(period.attribute("duration").unwrap()), expected);
}

fn assert_short_hls_presentation(output: &Path, profile: AudioProfile) {
    let text = std::fs::read_to_string(output.join("track-0000/media.m3u8")).unwrap();
    let values: Vec<_> = text
        .lines()
        .filter_map(|line| line.strip_prefix("#EXTINF:"))
        .map(|value| decimal(value.trim_end_matches(',')))
        .collect();
    assert!(values.len() > 1);
    let total: Ratio<i128> = values.iter().copied().sum();
    let expected = Ratio::new(
        i128::try_from(profile.presented_samples()).unwrap(),
        i128::from(profile.rate),
    );
    // Each generated declaration rounds upward to nanoseconds. This bound is
    // only serialization precision; coded clocks and sample counts stay exact.
    assert!(total >= expected);
    assert!(total - expected < Ratio::new(i128::try_from(values.len()).unwrap(), 1_000_000_000));
}

fn result_manifest(output: &Path, format: OutputFormat) -> std::path::PathBuf {
    output.join(if format == OutputFormat::Dash {
        "manifest.mpd"
    } else {
        "master.m3u8"
    })
}

fn assert_short_presentation(output: &Path, format: OutputFormat, profile: AudioProfile) {
    if format == OutputFormat::Dash {
        assert_short_dash_presentation(&result_manifest(output, format), profile);
    } else {
        assert_short_hls_presentation(output, profile);
    }
}

fn assert_raw_result(
    output: &Path,
    format: OutputFormat,
    root: &Path,
    profile: AudioProfile,
    packets: &[[Ratio<i64>; 3]],
    media: &RawAudio,
) {
    let candidate = root.join(if format == OutputFormat::HlsTs {
        "candidate.ts"
    } else {
        "candidate.mp4"
    });
    if format == OutputFormat::Dash {
        join_dash_track(output, &candidate);
    } else {
        join_resources(
            &output.join("track-0000/media.m3u8"),
            &candidate,
            format == OutputFormat::Hls,
        );
    }
    assert_eq!(
        raw_audio(&candidate, format != OutputFormat::HlsTs, profile),
        *media
    );
    let result_packets = if format == OutputFormat::HlsTs {
        packet_clocks(&candidate)
    } else {
        raw_mp4_packet_clocks(&candidate)
    };
    assert_eq!(result_packets.len(), packets.len());
    for (index, (before, after)) in packets.iter().zip(&result_packets).enumerate() {
        assert_eq!(after[0], before[0], "frame {index} PTS");
        assert_eq!(after[1], before[1], "frame {index} DTS");
        let expected_duration = if format == OutputFormat::HlsTs {
            before[2]
        } else {
            packets
                .get(index + 1)
                .map_or(before[2], |next| next[1] - before[1])
        };
        assert_eq!(after[2], expected_duration, "frame {index} coded duration");
    }
    assert_eq!(
        result_packets.last().unwrap()[0] + result_packets.last().unwrap()[2],
        packets.last().unwrap()[0] + packets.last().unwrap()[2]
    );
    if format != OutputFormat::HlsTs {
        assert_two_standard_edits(&output.join("track-0000/init.mp4"), profile, packets[0][0]);
    }
    assert_short_presentation(output, format, profile);
}

fn zero_origin_audio_source(root: &Path) -> std::path::PathBuf {
    drmr_core::native::create_media_dir_all(root).unwrap();
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("../drmr/tests/fixtures/bbb/clip.mp4");
    let source = root.join("media.m3u8");
    let path = |value: &Path| drmr_core::manifest_support::path::fspath(value).unwrap();
    let argv = vec![
        "ffmpeg".into(),
        "-v".into(),
        "error".into(),
        "-nostdin".into(),
        "-threads".into(),
        "1".into(),
        "-i".into(),
        path(&fixture),
        "-map".into(),
        "0:a:0".into(),
        "-c:a".into(),
        "copy".into(),
        "-copyts".into(),
        "-start_at_zero".into(),
        "-muxdelay".into(),
        "0".into(),
        "-muxpreload".into(),
        "0".into(),
        "-avoid_negative_ts".into(),
        "make_zero".into(),
        "-hls_time".into(),
        "2".into(),
        "-hls_playlist_type".into(),
        "vod".into(),
        "-hls_segment_filename".into(),
        path(&root.join("segment-%d.ts")),
        path(&source),
    ];
    let shown = ["ffmpeg".into(), "[fixture arguments redacted]".into()];
    let result = ToolRunner::new(Arc::new(Secrets::default()), Arc::new(NoSink))
        .run_tool(
            &argv,
            "ffmpeg",
            &drmr_core::toolrun::RunOptions {
                timeout: Some(std::time::Duration::from_secs(30)),
                argv_redacted: Some(&shown),
                max_output_chars: 0,
                log_stdout: false,
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(result.returncode, Some(0));
    let raw = root.with_extension("ts");
    join_resources(&source, &raw, false);
    let clocks = packet_clocks(&raw);
    assert_eq!(clocks.len(), AAC_48000.frames);
    for (index, clock) in clocks.iter().enumerate() {
        let timestamp = Ratio::new(i64::try_from(index).unwrap() * 1024, 48_000);
        assert_eq!(*clock, [timestamp, timestamp, Ratio::new(1024, 48_000)]);
    }
    assert_eq!(
        raw_audio(&raw, false, AAC_48000),
        raw_audio(&fixture, true, AAC_48000)
    );
    source
}

fn saved_clear_route(profile: AudioProfile, zero_origin: bool, format: OutputFormat) {
    use drmr_core::conversion::{
        VerificationSource,
        key_file::OperatorKeys,
        plan::{self, ExecutionPlanRequest},
    };
    let root = drmr_core::native::private_tempdir().unwrap();
    let source = if zero_origin {
        assert_eq!(profile.rate, 48_000);
        zero_origin_audio_source(&root.path().join("zero"))
    } else {
        audio_sources_at_rate(
            root.path(),
            (profile.rate != 48_000).then_some(profile.rate),
        )
        .0
    };
    let original_tree = common::snapshot_tree(source.parent().unwrap());
    let packets = shorten_final_declaration(&source, profile);
    if zero_origin {
        assert_eq!(packets[0][0], Ratio::from_integer(0));
    } else {
        assert!(packets[0][0] > Ratio::from_integer(0));
    }
    let source_before = common::snapshot_tree(source.parent().unwrap());
    assert_eq!(original_tree.len(), source_before.len());
    for ((name, bytes), (after_name, after_bytes)) in original_tree.iter().zip(&source_before) {
        assert_eq!(name, after_name);
        if name != "media.m3u8" {
            assert_eq!(after_bytes, bytes);
        }
    }
    let original = root.path().join("original.ts");
    join_resources(&source, &original, false);
    assert_eq!(packet_clocks(&original), packets);
    let media = raw_audio(&original, false, profile);
    assert_eq!(media.aac_frames, profile.frames);
    assert_eq!(media.pcm_samples_per_channel, profile.raw_samples());
    assert_eq!(RAW_SAMPLES, 442_368);
    assert_eq!(PRESENTED_SAMPLES, 441_584);
    println!(
        "DRMR_TS_AUDIO_WINDOW_FIXTURE {}",
        serde_json::json!({
            "source_segments": 6, "raw_aac_frames": media.aac_frames,
            "raw_pcm_samples_per_channel": media.pcm_samples_per_channel,
            "presented_pcm_samples_per_channel": profile.presented_samples(), "final_presented_samples": profile.final_samples,
            "raw_aac_sha256": media.aac_sha256, "raw_pcm_sha256": media.pcm_sha256,
            "presented_pcm_sha256": media.presented_pcm_sha256,
        })
    );
    let c = converter();
    let keys = OperatorKeys::default();
    let analysis = c
        .analyze_source(VerificationSource {
            input: &source,
            input_keys: &keys.input,
            hls_keys: &keys.hls_input,
        })
        .unwrap_or_else(|error| panic!("partial-frame source analysis: {}", error.0));
    assert!(analysis.input_admitted);
    assert_eq!(analysis.actual_format, OutputFormat::HlsTs);
    let output = root.path().join("output");
    let planned = c
        .create_execution_plan(ExecutionPlanRequest {
            input: VerificationSource {
                input: &source,
                input_keys: &keys.input,
                hls_keys: &keys.hls_input,
            },
            output: &output,
            format: Some(format),
            target: TargetProtection::Clear,
        })
        .unwrap();
    assert_eq!(planned.source, analysis.source);
    let saved = root.path().join("conversion.plan.json");
    plan::write_new(&saved, &planned).unwrap();
    let plan_before = std::fs::read(&saved).unwrap();
    let loaded = plan::read(&saved).unwrap();
    assert_eq!(loaded.plan_id, planned.plan_id);
    all_checks(&c.execute_plan(&loaded, &keys).unwrap().verification);
    let checked = converter().verify_plan(&loaded, &keys).unwrap();
    assert_eq!(checked.plan_id, planned.plan_id);
    assert!(checked.source_unchanged && checked.output_matches_plan);
    all_checks(&checked.verification);
    assert_raw_result(&output, format, root.path(), profile, &packets, &media);
    assert_eq!(std::fs::read(&saved).unwrap(), plan_before);
    assert_eq!(
        common::snapshot_tree(source.parent().unwrap()),
        source_before
    );
}

#[test]
fn final_partial_aac_presentation_survives_saved_dash_conversion_without_dropping_raw_media() {
    saved_clear_route(AAC_44100, false, OutputFormat::Dash);
}

#[test]
fn partial_aac_saved_hls_and_ts_routes_preserve_raw_and_presented_audio_separately() {
    for format in [OutputFormat::Hls, OutputFormat::HlsTs] {
        saved_clear_route(AAC_44100, false, format);
    }
}

#[test]
fn partial_aac_48000_and_zero_origin_keep_the_same_canonical_edit_contract() {
    for zero_origin in [false, true] {
        for format in [OutputFormat::Hls, OutputFormat::Dash, OutputFormat::HlsTs] {
            saved_clear_route(AAC_48000, zero_origin, format);
        }
    }
}

#[test]
fn partial_aac_final_one_and_1023_samples_keep_exact_global_sample_endpoints() {
    for profile in [AAC_44100, AAC_48000] {
        for final_samples in [1, 1023] {
            saved_clear_route(
                AudioProfile {
                    final_samples,
                    ..profile
                },
                false,
                OutputFormat::Dash,
            );
        }
    }
}

#[test]
fn standalone_partial_audio_mp4_inputs_and_outputs_are_refused_without_side_effects() {
    use drmr_core::conversion::{VerificationRequest, VerificationSource};
    let root = drmr_core::native::private_tempdir().unwrap();
    let (source, _) = audio_sources_at_rate(root.path(), Some(44_100));
    let packets = shorten_final_declaration(&source, AAC_44100);
    let original = root.path().join("original.ts");
    join_resources(&source, &original, false);
    let media = raw_audio(&original, false, AAC_44100);
    assert_eq!(media.aac_frames, RAW_FRAMES);
    assert_eq!(media.pcm_samples_per_channel, RAW_SAMPLES);
    let prepared = root.path().join("prepared");
    clear_conversion(&source, &prepared, OutputFormat::Dash);
    let mp4 = root.path().join("standalone.mp4");
    join_dash_track(&prepared, &mp4);
    assert_two_standard_edits(&mp4, AAC_44100, packets[0][0]);
    let before = common::snapshot_tree(root.path());
    let empty = InputKeys::default();
    let hls = HlsInputKeys::default();
    refuse_code(
        &converter().analyze_source(VerificationSource {
            input: &mp4,
            input_keys: &empty,
            hls_keys: &hls,
        }),
        "audio-presentation-range-not-supported",
    );
    for (format, code) in [
        (OutputFormat::Hls, "audio-presentation-range-not-supported"),
        (OutputFormat::Dash, "audio-presentation-range-not-supported"),
        // TS output first requires an original HLS transport source.
        (OutputFormat::HlsTs, "ts-original-evidence-required"),
        (OutputFormat::Mp4, "audio-presentation-range-not-supported"),
    ] {
        let output = root
            .path()
            .join(format!("refused-input-{}", format.as_str()));
        refuse_clear_plan_and_conversion(
            &mp4,
            &output,
            format,
            "audio-presentation-range-not-supported",
            code,
        );
    }
    let manifest = prepared.join("manifest.mpd");
    for (reference, result) in [(&mp4, &manifest), (&source, &mp4)] {
        refuse_code(
            &converter().verify(VerificationRequest {
                source: VerificationSource {
                    input: reference,
                    input_keys: &empty,
                    hls_keys: &hls,
                },
                output: VerificationSource {
                    input: result,
                    input_keys: &empty,
                    hls_keys: &hls,
                },
            }),
            "audio-presentation-range-not-supported",
        );
    }
    for (label, input, code) in [
        ("ts", &source, "ts-audio-only-timing-proof-not-supported"),
        ("dash", &manifest, "audio-presentation-range-not-supported"),
    ] {
        let output = root.path().join(format!("refused-output-{label}.mp4"));
        refuse_clear_plan_and_conversion(
            input,
            &output,
            OutputFormat::Mp4,
            "execution-plan-not-supported",
            code,
        );
    }
    assert_eq!(common::snapshot_tree(root.path()), before);
}

fn refuse_clear_plan_and_conversion(
    input: &Path,
    output: &Path,
    format: OutputFormat,
    plan_code: &str,
    conversion_code: &str,
) {
    use drmr_core::conversion::{VerificationSource, plan::ExecutionPlanRequest};
    let empty = InputKeys::default();
    let hls = HlsInputKeys::default();
    refuse_code(
        &converter().create_execution_plan(ExecutionPlanRequest {
            input: VerificationSource {
                input,
                input_keys: &empty,
                hls_keys: &hls,
            },
            output,
            format: Some(format),
            target: TargetProtection::Clear,
        }),
        plan_code,
    );
    refuse_code(
        &converter().convert(ConversionRequest {
            input,
            output,
            format,
            target: TargetProtection::Clear,
            input_keys: &empty,
            hls_input_keys: &hls,
            output_key: None,
            hls_key_uri: None,
            hls_output_key: None,
        }),
        conversion_code,
    );
    assert!(!output.exists());
}

fn refuse_code<T>(result: &Result<T, drmr_core::conversion::ConversionError>, expected: &str) {
    match result {
        Ok(_) => panic!("unsupported profile was admitted"),
        Err(error) => assert_eq!(error.0, expected),
    }
}

fn protected_targets(format: OutputFormat) -> [TargetProtection; 3] {
    match format {
        OutputFormat::Hls => [
            TargetProtection::Clear,
            TargetProtection::Cbcs,
            TargetProtection::HlsAes128,
        ],
        OutputFormat::Dash => [
            TargetProtection::Clear,
            TargetProtection::Cenc,
            TargetProtection::Cbcs,
        ],
        OutputFormat::HlsTs => [
            TargetProtection::Clear,
            TargetProtection::HlsAes128,
            TargetProtection::HlsSampleAes,
        ],
        OutputFormat::Mp4 => panic!("unexpected output profile"),
    }
}

struct Material {
    content: Option<drmr_core::conversion::OutputKey>,
    segment: Option<HlsOutputKey>,
    verification: InputKeys,
    hls_verification: HlsInputKeys,
}

fn material(target: TargetProtection) -> Material {
    use drmr_core::conversion::{ContentKey, OutputKey};
    let kid = "909192939495969798999a9b9c9d9e9f";
    let coded = matches!(target, TargetProtection::Cenc | TargetProtection::Cbcs);
    let content = coded.then(|| OutputKey::from_hex(kid, OUTPUT_KEY).unwrap());
    let verification = if coded {
        InputKeys::new(vec![ContentKey::from_hex(kid, OUTPUT_KEY).unwrap()]).unwrap()
    } else {
        InputKeys::default()
    };
    let segment = match target {
        TargetProtection::HlsAes128 => {
            Some(HlsOutputKey::new(KEY_URI, Aes128Key::from_hex(OUTPUT_KEY).unwrap()).unwrap())
        }
        TargetProtection::HlsSampleAes => Some(
            HlsOutputKey::new_with_iv(
                KEY_URI,
                Aes128Key::from_hex(OUTPUT_KEY).unwrap(),
                SegmentIv::from_hex(OUTPUT_IV).unwrap(),
            )
            .unwrap(),
        ),
        _ => None,
    };
    let hls_verification = if segment.is_some() {
        HlsInputKeys::new_with_ivs(vec![(
            KEY_URI.into(),
            Aes128Key::from_hex(OUTPUT_KEY).unwrap(),
            (target == TargetProtection::HlsSampleAes)
                .then(|| SegmentIv::from_hex(OUTPUT_IV).unwrap()),
        )])
        .unwrap()
    } else {
        HlsInputKeys::default()
    };
    Material {
        content,
        segment,
        verification,
        hls_verification,
    }
}

fn copy_final_duration(clear: &Path, sample: &Path) {
    let clear_text = std::fs::read_to_string(clear).unwrap();
    let declared = clear_text
        .lines()
        .rfind(|line| line.starts_with("#EXTINF:"))
        .unwrap();
    let sample_text = std::fs::read_to_string(sample).unwrap();
    let mut lines: Vec<_> = sample_text.lines().map(str::to_owned).collect();
    let at = lines
        .iter()
        .rposition(|line| line.starts_with("#EXTINF:"))
        .unwrap();
    declared.clone_into(&mut lines[at]);
    std::fs::write(sample, lines.join("\n") + "\n").unwrap();
}

struct ProtectedAudioReference<'a> {
    label: &'a str,
    source: &'a Path,
    source_keys: &'a HlsInputKeys,
    clear: &'a Path,
    packets: &'a [[Ratio<i64>; 3]],
    media: &'a RawAudio,
}

fn convert_protected_audio(
    reference: &ProtectedAudioReference<'_>,
    output: &Path,
    format: OutputFormat,
    target: TargetProtection,
    material: &Material,
) -> drmr_core::conversion::ConversionReport {
    converter()
        .convert(ConversionRequest {
            input: reference.source,
            output,
            format,
            target,
            input_keys: &InputKeys::default(),
            hls_input_keys: reference.source_keys,
            output_key: material.content.as_ref(),
            hls_key_uri: (format == OutputFormat::Hls && target == TargetProtection::Cbcs)
                .then_some(KEY_URI),
            hls_output_key: material.segment.as_ref(),
        })
        .unwrap_or_else(|error| {
            panic!(
                "{}/{}/{}, convert: {}",
                reference.label,
                format.as_str(),
                target.as_str(),
                error.0
            )
        })
}

fn verify_protected_audio(
    reference: &ProtectedAudioReference<'_>,
    candidate: &Path,
    format: OutputFormat,
    target: TargetProtection,
    material: &Material,
) {
    use drmr_core::conversion::{VerificationRequest, VerificationSource};
    let empty = InputKeys::default();
    let no_hls = HlsInputKeys::default();
    for (source, keys) in [
        (reference.source, reference.source_keys),
        (reference.clear, &no_hls),
    ] {
        all_checks(
            &converter()
                .verify(VerificationRequest {
                    source: VerificationSource {
                        input: source,
                        input_keys: &empty,
                        hls_keys: keys,
                    },
                    output: VerificationSource {
                        input: candidate,
                        input_keys: &material.verification,
                        hls_keys: &material.hls_verification,
                    },
                })
                .unwrap_or_else(|error| {
                    panic!(
                        "{}/{}/{}, fresh verify: {}",
                        reference.label,
                        format.as_str(),
                        target.as_str(),
                        error.0
                    )
                }),
        );
    }
}

fn reconvert_protected_audio(
    reference: &ProtectedAudioReference<'_>,
    candidate: &Path,
    output: &Path,
    format: OutputFormat,
    target: TargetProtection,
    material: &Material,
) {
    all_checks(
        &converter()
            .convert(ConversionRequest {
                input: candidate,
                output,
                format: OutputFormat::Dash,
                target: TargetProtection::Clear,
                input_keys: &material.verification,
                hls_input_keys: &material.hls_verification,
                output_key: None,
                hls_key_uri: None,
                hls_output_key: None,
            })
            .unwrap_or_else(|error| {
                panic!(
                    "{}/{}/{}, reconvert: {}",
                    reference.label,
                    format.as_str(),
                    target.as_str(),
                    error.0
                )
            })
            .verification,
    );
    all_checks(&verify_clear_range(reference.clear, output, OutputFormat::Dash).unwrap());
}

fn protected_audio_cell(
    root: &Path,
    reference: &ProtectedAudioReference<'_>,
    format: OutputFormat,
    target: TargetProtection,
    index: usize,
) {
    let output = root.join(format!(
        "{}-{}-{}",
        reference.label,
        format.as_str(),
        target.as_str()
    ));
    let material = material(target);
    let converted = convert_protected_audio(reference, &output, format, target, &material);
    all_checks(&converted.verification);
    let candidate = result_manifest(&output, format);
    verify_protected_audio(reference, &candidate, format, target, &material);
    assert_short_presentation(&output, format, AAC_44100);
    if format != OutputFormat::HlsTs && target != TargetProtection::HlsAes128 {
        assert_two_standard_edits(
            &output.join("track-0000/init.mp4"),
            AAC_44100,
            reference.packets[0][0],
        );
    }
    let roundtrip = root.join(format!("roundtrip-{index}"));
    reconvert_protected_audio(reference, &candidate, &roundtrip, format, target, &material);
    let evidence = root.join(format!("evidence-{index}"));
    drmr_core::native::create_media_dir_all(&evidence).unwrap();
    assert_raw_result(
        &roundtrip,
        OutputFormat::Dash,
        &evidence,
        AAC_44100,
        reference.packets,
        reference.media,
    );
    let rendered = serde_json::to_string(&converted).unwrap();
    for secret in [INPUT_KEY, OUTPUT_KEY, INPUT_IV, OUTPUT_IV, KEY_URI] {
        assert!(!rendered.contains(secret));
    }
    println!(
        "DRMR_TS_AUDIO_WINDOW_CELL {}",
        serde_json::json!({
            "input": reference.label, "output_format": format.as_str(), "target": target.as_str(),
            "raw_aac_frames": RAW_FRAMES, "raw_pcm_samples_per_channel": RAW_SAMPLES,
            "presented_pcm_samples_per_channel": PRESENTED_SAMPLES, "fresh_verification": true,
            "clear_reconversion_verified": true, "license_playback_verified": false,
        })
    );
}

#[test]
fn partial_final_aac_twenty_seven_protection_routes_reconvert_with_the_same_raw_and_declared_audio()
{
    use drmr_core::conversion::VerificationSource;
    let root = drmr_core::native::private_tempdir().unwrap();
    let (clear, sample) = audio_sources_at_rate(root.path(), Some(44_100));
    let packets = shorten_final_declaration(&clear, AAC_44100);
    copy_final_duration(&clear, &sample);
    let aes = aes_source(&clear, &root.path().join("aes"));
    let original = root.path().join("original.ts");
    join_resources(&clear, &original, false);
    let media = raw_audio(&original, false, AAC_44100);
    assert_eq!(media.aac_frames, RAW_FRAMES);
    assert_eq!(media.pcm_samples_per_channel, RAW_SAMPLES);
    let empty = InputKeys::default();
    let no_hls = HlsInputKeys::default();
    let encrypted = HlsInputKeys::new_with_ivs(vec![(
        KEY_URI.into(),
        Aes128Key::from_hex(INPUT_KEY).unwrap(),
        Some(SegmentIv::from_hex(INPUT_IV).unwrap()),
    )])
    .unwrap();
    let mut cells = 0;
    for (label, source, source_keys) in [
        ("clear", &clear, &no_hls),
        ("aes-128", &aes, &encrypted),
        ("sample-aes", &sample, &encrypted),
    ] {
        let before = common::snapshot_tree(source.parent().unwrap());
        let admitted = converter()
            .analyze_source(VerificationSource {
                input: source,
                input_keys: &empty,
                hls_keys: source_keys,
            })
            .unwrap_or_else(|error| panic!("{label} partial-source analysis: {}", error.0));
        assert!(admitted.input_admitted);
        let reference = ProtectedAudioReference {
            label,
            source,
            source_keys,
            clear: &clear,
            packets: &packets,
            media: &media,
        };
        for format in [OutputFormat::Hls, OutputFormat::Dash, OutputFormat::HlsTs] {
            for target in protected_targets(format) {
                protected_audio_cell(root.path(), &reference, format, target, cells);
                assert_eq!(common::snapshot_tree(source.parent().unwrap()), before);
                cells += 1;
            }
        }
    }
    assert_eq!(cells, 27);
}

fn verify_clear_range(
    source: &Path,
    output: &Path,
    format: OutputFormat,
) -> Result<drmr_core::conversion::VerificationReport, drmr_core::conversion::ConversionError> {
    use drmr_core::conversion::{VerificationRequest, VerificationSource};
    let empty = InputKeys::default();
    let hls = HlsInputKeys::default();
    converter().verify(VerificationRequest {
        source: VerificationSource {
            input: source,
            input_keys: &empty,
            hls_keys: &hls,
        },
        output: VerificationSource {
            input: &result_manifest(output, format),
            input_keys: &empty,
            hls_keys: &hls,
        },
    })
}

fn refuse_range(
    result: Result<
        drmr_core::conversion::VerificationReport,
        drmr_core::conversion::ConversionError,
    >,
    expected: &[&str],
) {
    match result {
        Err(error) => assert!(
            expected.contains(&error.0),
            "unexpected failure cannot prove range rejection: {}",
            error.0
        ),
        Ok(report) => {
            assert!(!report.passed && !report.media_verified && !report.license_playback_verified);
            assert!(
                !report.checks.stream_metadata.is_pass()
                    || !report.checks.packet_payloads_equal
                    || !report.checks.packet_timing_equal
                    || !report.checks.decoded_content_equal
            );
        }
    }
}

fn clear_conversion(source: &Path, output: &Path, format: OutputFormat) {
    all_checks(
        &converter()
            .convert(ConversionRequest {
                input: source,
                output,
                format,
                target: TargetProtection::Clear,
                input_keys: &InputKeys::default(),
                hls_input_keys: &HlsInputKeys::default(),
                output_key: None,
                hls_key_uri: None,
                hls_output_key: None,
            })
            .unwrap()
            .verification,
    );
    all_checks(&verify_clear_range(source, output, format).unwrap());
}

fn put_unsigned(data: &mut [u8], at: usize, width: usize, value: u64) {
    if width == 8 {
        data[at..at + 8].copy_from_slice(&value.to_be_bytes());
    } else {
        assert_eq!(width, 4);
        data[at..at + 4].copy_from_slice(&u32::try_from(value).unwrap().to_be_bytes());
    }
}

fn edit_mutations(original: &[u8], profile: AudioProfile) -> Vec<(&'static str, Vec<u8>)> {
    let (_, entries) = edits(original);
    assert_eq!(entries.len(), 2);
    let mut values = Vec::new();
    let mut one_sample = original.to_vec();
    put_unsigned(
        &mut one_sample,
        entries[1].duration_at,
        entries[1].width,
        entries[1].duration + profile.ticks_per_sample,
    );
    values.push(("one-sample-regular-edit", one_sample));
    let mut empty_tick = original.to_vec();
    put_unsigned(
        &mut empty_tick,
        entries[0].duration_at,
        entries[0].width,
        entries[0].duration + 1,
    );
    values.push(("one-tick-empty-edit", empty_tick));
    let mut media_tick = original.to_vec();
    put_unsigned(
        &mut media_tick,
        entries[1].media_at,
        entries[1].width,
        u64::try_from(entries[1].media_time + 1).unwrap(),
    );
    values.push(("one-tick-media-time", media_tick));
    let mut unknown = original.to_vec();
    let fraction_at = entries[1].media_at + entries[1].width + 2;
    unknown[fraction_at..fraction_at + 2].copy_from_slice(&1_u16.to_be_bytes());
    values.push(("unknown-edit-rate", unknown));
    values.push(("missing-own-edit", without_audio_edits(original)));
    values
}

fn without_audio_edits(original: &[u8]) -> Vec<u8> {
    let ancestors = [
        one_box(original, &[*b"moov"]),
        one_box(original, &[*b"moov", *b"trak"]),
    ];
    let edts = one_box(original, &[*b"moov", *b"trak", *b"edts"]);
    let removed = edts.end - edts.start;
    let mut absent = original.to_vec();
    for ancestor in ancestors {
        let length = u32::from_be_bytes(
            original[ancestor.start..ancestor.start + 4]
                .try_into()
                .unwrap(),
        );
        match length {
            0 => {}
            1 => put_unsigned(
                &mut absent,
                ancestor.start + 8,
                8,
                u64::try_from(ancestor.end - ancestor.start - removed).unwrap(),
            ),
            _ => put_unsigned(
                &mut absent,
                ancestor.start,
                4,
                u64::try_from(ancestor.end - ancestor.start - removed).unwrap(),
            ),
        }
    }
    absent.drain(edts.start..edts.end);
    let track = one_box(&absent, &[*b"moov", *b"trak"]);
    assert!(
        boxes(&absent, track.payload, track.end)
            .iter()
            .all(|item| item.kind != *b"edts")
    );
    assert_eq!(absent.len() + removed, original.len());
    absent
}

fn changed_manifest(
    output: &Path,
    format: OutputFormat,
    profile: AudioProfile,
) -> (std::path::PathBuf, Vec<u8>, Vec<u8>) {
    if format == OutputFormat::Dash {
        let path = output.join("manifest.mpd");
        let original = std::fs::read(&path).unwrap();
        let text = std::str::from_utf8(&original).unwrap();
        let doc = roxmltree::Document::parse(text).unwrap();
        let period = doc
            .descendants()
            .find(|node| node.has_tag_name("Period"))
            .unwrap();
        let attr = period.attribute_node("duration").unwrap();
        let nanos = (u128::try_from(profile.presented_samples() + 1).unwrap() * 1_000_000_000)
            .div_ceil(u128::from(profile.rate));
        let mut changed = text.to_owned();
        changed.replace_range(
            attr.range_value(),
            &format!("PT{}.{:09}S", nanos / 1_000_000_000, nanos % 1_000_000_000),
        );
        (path, original, changed.into_bytes())
    } else {
        let path = output.join("track-0000/media.m3u8");
        let original = std::fs::read(&path).unwrap();
        let mut lines: Vec<_> = std::str::from_utf8(&original)
            .unwrap()
            .lines()
            .map(str::to_owned)
            .collect();
        let at = lines
            .iter()
            .rposition(|line| line.starts_with("#EXTINF:"))
            .unwrap();
        let declared = lines[at]
            .strip_prefix("#EXTINF:")
            .unwrap()
            .trim_end_matches(',');
        let changed = if format == OutputFormat::HlsTs {
            let scaled = decimal(declared) * i128::from(profile.rate);
            let samples = scaled.to_integer();
            assert!(samples > 0);
            assert!(
                scaled - Ratio::from_integer(samples)
                    < Ratio::new(i128::from(profile.rate), 1_000_000_000)
            );
            let nanos = (u128::try_from(samples + 1).unwrap() * 1_000_000_000)
                .div_ceil(u128::from(profile.rate));
            format!("{}.{:09}", nanos / 1_000_000_000, nanos % 1_000_000_000)
        } else {
            long_decimal(declared.parse::<f64>().unwrap() + 1.0 / f64::from(profile.rate))
        };
        lines[at] = format!("#EXTINF:{changed},");
        (path, original, (lines.join("\n") + "\n").into_bytes())
    }
}

#[test]
fn independent_verification_refuses_changed_audio_edits_and_final_manifest_ranges() {
    for profile in [AAC_44100, AAC_48000] {
        let root = drmr_core::native::private_tempdir().unwrap();
        let (source, _) = audio_sources_at_rate(
            root.path(),
            (profile.rate != 48_000).then_some(profile.rate),
        );
        let packets = shorten_final_declaration(&source, profile);
        let source_before = common::snapshot_tree(source.parent().unwrap());
        for format in [OutputFormat::Hls, OutputFormat::Dash, OutputFormat::HlsTs] {
            let output = root.path().join(format.as_str());
            clear_conversion(&source, &output, format);
            let output_before = common::snapshot_tree(&output);
            if format != OutputFormat::HlsTs {
                let init = output.join("track-0000/init.mp4");
                assert_two_standard_edits(&init, profile, packets[0][0]);
                let bytes = std::fs::read(&init).unwrap();
                for (label, mutated) in edit_mutations(&bytes, profile) {
                    println!(
                        "partial-audio-edit-check: rate={} format={} mutation={label}",
                        profile.rate,
                        format.as_str()
                    );
                    std::fs::write(&init, mutated).unwrap();
                    refuse_range(
                        verify_clear_range(&source, &output, format),
                        &[edit_refusal_code(label, format)],
                    );
                    std::fs::write(&init, &bytes).unwrap();
                    all_checks(&verify_clear_range(&source, &output, format).unwrap());
                }
            }
            println!(
                "partial-audio-manifest-check: rate={} format={} mutation=one-sample-range",
                profile.rate,
                format.as_str()
            );
            let (manifest, original, mutated) = changed_manifest(&output, format, profile);
            assert_ne!(original, mutated);
            std::fs::write(&manifest, mutated).unwrap();
            if format == OutputFormat::Dash {
                refuse_range(
                    verify_clear_range(&source, &output, format),
                    &["dash-presentation-duration-mismatch"],
                );
            } else if format == OutputFormat::HlsTs {
                // The changed integer-sample declaration must pass numeric admission.
                refuse_range(verify_clear_range(&source, &output, format), &[]);
            } else {
                refuse_range(
                    verify_clear_range(&source, &output, format),
                    &[
                        "audio-presentation-range-mismatch",
                        "manifest-segment-duration-mismatch",
                    ],
                );
            }
            std::fs::write(&manifest, original).unwrap();
            all_checks(&verify_clear_range(&source, &output, format).unwrap());
            assert_eq!(common::snapshot_tree(&output), output_before);
            assert_eq!(
                common::snapshot_tree(source.parent().unwrap()),
                source_before
            );
        }
    }
}

fn edit_refusal_code(label: &str, format: OutputFormat) -> &'static str {
    match (label, format) {
        ("one-sample-regular-edit" | "missing-own-edit", OutputFormat::Dash) => {
            "dash-presentation-duration-mismatch"
        }
        ("one-sample-regular-edit", OutputFormat::Hls) => "audio-presentation-range-mismatch",
        ("missing-own-edit", OutputFormat::Hls) => "manifest-segment-duration-mismatch",
        (
            "one-tick-empty-edit" | "one-tick-media-time" | "unknown-edit-rate",
            OutputFormat::Hls | OutputFormat::Dash,
        ) => "audio-presentation-range-not-supported",
        _ => panic!("unexpected test mutation or output format"),
    }
}

#[test]
fn original_and_saved_plan_verification_refuse_a_missing_one_sample_audio_trim() {
    use drmr_core::conversion::{
        VerificationSource,
        key_file::OperatorKeys,
        plan::{self, ExecutionPlanRequest},
    };
    for profile in [AAC_44100, AAC_48000].map(|profile| AudioProfile {
        final_samples: 1023,
        ..profile
    }) {
        let root = drmr_core::native::private_tempdir().unwrap();
        let (source, _) = audio_sources_at_rate(
            root.path(),
            (profile.rate != 48_000).then_some(profile.rate),
        );
        let packets = shorten_final_declaration(&source, profile);
        assert_eq!(profile.raw_samples() - profile.presented_samples(), 1);
        let source_before = common::snapshot_tree(source.parent().unwrap());
        let keys = OperatorKeys::default();
        for format in [OutputFormat::Hls, OutputFormat::Dash] {
            let output = root.path().join(format.as_str());
            let planned = converter()
                .create_execution_plan(ExecutionPlanRequest {
                    input: VerificationSource {
                        input: &source,
                        input_keys: &keys.input,
                        hls_keys: &keys.hls_input,
                    },
                    output: &output,
                    format: Some(format),
                    target: TargetProtection::Clear,
                })
                .unwrap();
            let saved = root.path().join(format!("{}.plan.json", format.as_str()));
            plan::write_new(&saved, &planned).unwrap();
            let plan_before = std::fs::read(&saved).unwrap();
            let loaded = plan::read(&saved).unwrap();
            all_checks(
                &converter()
                    .execute_plan(&loaded, &keys)
                    .unwrap()
                    .verification,
            );
            all_checks(
                &converter()
                    .verify_plan(&loaded, &keys)
                    .unwrap()
                    .verification,
            );
            let output_before = common::snapshot_tree(&output);
            let init = output.join("track-0000/init.mp4");
            assert_two_standard_edits(&init, profile, packets[0][0]);
            let bytes = std::fs::read(&init).unwrap();
            let (_, no_edit) = edit_mutations(&bytes, profile)
                .into_iter()
                .find(|(label, _)| *label == "missing-own-edit")
                .unwrap();
            println!(
                "partial-audio-edit-check: rate={} format={} mutation=missing-one-sample-trim",
                profile.rate,
                format.as_str()
            );
            std::fs::write(&init, no_edit).unwrap();
            // An unbound analysis may classify an unedited file as ordinary audio.
            // Comparing with the original must still preserve the declared sample range.
            let expected = if format == OutputFormat::Dash {
                "dash-presentation-duration-mismatch"
            } else {
                "audio-presentation-range-mismatch"
            };
            refuse_range(verify_clear_range(&source, &output, format), &[expected]);
            refuse_range(
                converter()
                    .verify_plan(&loaded, &keys)
                    .map(|report| report.verification),
                &[expected],
            );
            std::fs::write(&init, bytes).unwrap();
            all_checks(&verify_clear_range(&source, &output, format).unwrap());
            all_checks(
                &converter()
                    .verify_plan(&loaded, &keys)
                    .unwrap()
                    .verification,
            );
            assert_eq!(common::snapshot_tree(&output), output_before);
            assert_eq!(std::fs::read(&saved).unwrap(), plan_before);
            assert_eq!(
                common::snapshot_tree(source.parent().unwrap()),
                source_before
            );
        }
    }
}

fn invalid_source_declaration(source: &Path, profile: AudioProfile, kind: &str) {
    let original = std::fs::read_to_string(source).unwrap();
    let mut lines: Vec<_> = original.lines().map(str::to_owned).collect();
    let resources: Vec<_> = lines
        .iter()
        .enumerate()
        .filter(|(_, line)| !line.is_empty() && !line.starts_with('#'))
        .map(|(index, name)| (index, source.parent().unwrap().join(name)))
        .collect();
    assert_eq!(resources.len(), 6);
    if kind == "ambiguous-low-precision" {
        lines[resources.last().unwrap().0 - 1] = "#EXTINF:0.005,".into();
    } else if kind == "non-final-partial" {
        let frames = u32::try_from(packet_clocks(&resources[0].1).len()).unwrap();
        let samples = frames * 1024 - 784;
        lines[resources[0].0 - 1] = format!(
            "#EXTINF:{},",
            long_decimal(f64::from(samples) / f64::from(profile.rate))
        );
    } else {
        assert_eq!(kind, "whole-frame-or-larger-trim");
        let (last_at, last_path) = resources.last().unwrap();
        let (previous_at, previous_path) = &resources[resources.len() - 2];
        let mut combined = std::fs::read(previous_path).unwrap();
        combined.extend_from_slice(&std::fs::read(last_path).unwrap());
        std::fs::write(previous_path, combined).unwrap();
        let frames = u32::try_from(packet_clocks(previous_path).len()).unwrap();
        assert!(frames > 2);
        let samples = frames * 1024 - 1024 - 784;
        lines[*previous_at - 1] = format!(
            "#EXTINF:{},",
            long_decimal(f64::from(samples) / f64::from(profile.rate))
        );
        lines.drain(last_at - 1..=(*last_at));
    }
    std::fs::write(source, lines.join("\n") + "\n").unwrap();
}

#[test]
fn source_admission_refuses_ambiguous_nonfinal_and_whole_frame_audio_reductions() {
    use drmr_core::conversion::VerificationSource;
    for profile in [AAC_44100, AAC_48000] {
        for kind in [
            "ambiguous-low-precision",
            "non-final-partial",
            "whole-frame-or-larger-trim",
        ] {
            let root = drmr_core::native::private_tempdir().unwrap();
            let (source, _) = audio_sources_at_rate(
                root.path(),
                (profile.rate != 48_000).then_some(profile.rate),
            );
            invalid_source_declaration(&source, profile, kind);
            let before = common::snapshot_tree(source.parent().unwrap());
            let error = converter()
                .analyze_source(VerificationSource {
                    input: &source,
                    input_keys: &InputKeys::default(),
                    hls_keys: &HlsInputKeys::default(),
                })
                .unwrap_err();
            assert_eq!(error.0, "ts-segment-duration-mismatch", "{kind}");
            assert_eq!(common::snapshot_tree(source.parent().unwrap()), before);
        }
    }
}

fn video_rendition(directory: &Path, longer: bool) -> std::path::PathBuf {
    drmr_core::native::create_media_dir_all(directory).unwrap();
    let clip = Path::new(env!("CARGO_MANIFEST_DIR")).join("../drmr/tests/fixtures/bbb/clip.mp4");
    let mut argv = vec![
        "ffmpeg".into(),
        "-v".into(),
        "error".into(),
        "-nostdin".into(),
        "-threads".into(),
        "1".into(),
        "-i".into(),
        drmr_core::manifest_support::path::fspath(&clip).unwrap(),
        "-map".into(),
        "0:v:0".into(),
        "-an".into(),
    ];
    if longer {
        // Three cloned fixture frames make video longer than the partial AAC presentation.
        argv.extend([
            "-vf".into(),
            "tpad=stop_mode=clone:stop_duration=0.125".into(),
            "-c:v".into(),
            "libx264".into(),
            "-threads".into(),
            "1".into(),
            "-bf".into(),
            "0".into(),
            "-g".into(),
            "48".into(),
            "-frames:v".into(),
            "243".into(),
        ]);
    } else {
        argv.extend(["-c:v".into(), "copy".into()]);
    }
    let playlist = directory.join("media.m3u8");
    argv.extend([
        "-hls_time".into(),
        "2".into(),
        "-hls_playlist_type".into(),
        "vod".into(),
        "-hls_segment_filename".into(),
        drmr_core::manifest_support::path::fspath(&directory.join("segment-%03d.ts")).unwrap(),
        drmr_core::manifest_support::path::fspath(&playlist).unwrap(),
    ]);
    let result = ToolRunner::new(Arc::new(Secrets::default()), Arc::new(NoSink))
        .run_tool(
            &argv,
            "ffmpeg",
            &drmr_core::toolrun::RunOptions {
                timeout: Some(std::time::Duration::from_secs(30)),
                max_stdout_bytes: 65_536,
                max_output_chars: 0,
                log_stdout: false,
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(result.returncode, Some(0));
    playlist
}

fn av_dash_case(video_longer: bool) {
    use drmr_core::{
        conversion::{VerificationRequest, VerificationSource},
        inspection::TrackKind,
    };
    let root = drmr_core::native::private_tempdir().unwrap();
    let (audio, _) = audio_sources_at_rate(root.path(), Some(44_100));
    let audio_packets = shorten_final_declaration(&audio, AAC_44100);
    let video = video_rendition(&root.path().join("video"), video_longer);
    let original_video = root.path().join("original-video.ts");
    join_resources(&video, &original_video, false);
    let video_packets = observed_packet_clocks(&original_video, "v:0", false);
    assert_eq!(video_packets.len(), if video_longer { 243 } else { 240 });
    assert_eq!(video_packets[0][0], audio_packets[0][0]);
    let video_span =
        video_packets.last().unwrap()[0] + video_packets.last().unwrap()[2] - video_packets[0][0];
    let audio_span = Ratio::new(i64::try_from(PRESENTED_SAMPLES).unwrap(), 44_100);
    assert_eq!(video_span > audio_span, video_longer);
    let original_audio = root.path().join("original-audio.ts");
    join_resources(&audio, &original_audio, false);
    let audio_media = raw_audio(&original_audio, false, AAC_44100);
    let master = root.path().join("master.m3u8");
    let master_text = write_av_master(&master);
    let audio_before = common::snapshot_tree(audio.parent().unwrap());
    let video_before = common::snapshot_tree(video.parent().unwrap());
    let empty = InputKeys::default();
    let hls = HlsInputKeys::default();
    let analysis = converter()
        .analyze_source(VerificationSource {
            input: &master,
            input_keys: &empty,
            hls_keys: &hls,
        })
        .unwrap();
    assert!(analysis.input_admitted);
    assert_eq!(analysis.input.tracks.len(), 2);
    let output = root.path().join("dash");
    let converted = converter()
        .convert(ConversionRequest {
            input: &master,
            output: &output,
            format: OutputFormat::Dash,
            target: TargetProtection::Clear,
            input_keys: &empty,
            hls_input_keys: &hls,
            output_key: None,
            hls_key_uri: None,
            hls_output_key: None,
        })
        .unwrap();
    all_checks(&converted.verification);
    all_checks(
        &converter()
            .verify(VerificationRequest {
                source: VerificationSource {
                    input: &master,
                    input_keys: &empty,
                    hls_keys: &hls,
                },
                output: VerificationSource {
                    input: &output.join("manifest.mpd"),
                    input_keys: &empty,
                    hls_keys: &hls,
                },
            })
            .unwrap(),
    );
    assert_eq!(converted.output.tracks.len(), 2);
    let audio_track = converted
        .output
        .tracks
        .iter()
        .find(|track| track.kind == TrackKind::Audio)
        .unwrap();
    let video_track = converted
        .output
        .tracks
        .iter()
        .find(|track| track.kind == TrackKind::Video)
        .unwrap();
    assert_av_coded_media(
        root.path(),
        &output,
        &audio_track.id,
        &video_track.id,
        &audio_packets,
        &audio_media,
        &video_packets,
    );
    assert_dash_span(&output.join("manifest.mpd"), video_span.max(audio_span));
    assert_eq!(std::fs::read_to_string(&master).unwrap(), master_text);
    assert_eq!(common::snapshot_tree(audio.parent().unwrap()), audio_before);
    assert_eq!(common::snapshot_tree(video.parent().unwrap()), video_before);
}

fn write_av_master(master: &Path) -> String {
    use std::io::Write as _;
    let master_text = concat!(
        "#EXTM3U\n#EXT-X-VERSION:5\n",
        "#EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"audio\",NAME=\"Main\",DEFAULT=YES,AUTOSELECT=YES,URI=\"clear/media.m3u8\"\n",
        "#EXT-X-STREAM-INF:BANDWIDTH=2000000,AUDIO=\"audio\"\nvideo/media.m3u8\n"
    );
    drmr_core::native::create_media_new(master)
        .unwrap()
        .write_all(master_text.as_bytes())
        .unwrap();
    master_text.to_owned()
}

fn assert_av_coded_media(
    root: &Path,
    output: &Path,
    audio_track: &str,
    video_track: &str,
    audio_packets: &[[Ratio<i64>; 3]],
    audio_media: &RawAudio,
    video_packets: &[[Ratio<i64>; 3]],
) {
    let result_audio = root.join("result-audio.mp4");
    let result_video = root.join("result-video.mp4");
    join_dash_track_named(output, audio_track, &result_audio);
    join_dash_track_named(output, video_track, &result_video);
    assert_eq!(raw_audio(&result_audio, true, AAC_44100), *audio_media);
    let result_audio_packets = raw_mp4_packet_clocks(&result_audio);
    assert_eq!(result_audio_packets.len(), audio_packets.len());
    for (before, after) in audio_packets.iter().zip(&result_audio_packets) {
        assert_eq!(before[0], after[0]);
        assert_eq!(before[1], after[1]);
    }
    assert_eq!(
        result_audio_packets.last().unwrap()[0] + result_audio_packets.last().unwrap()[2],
        audio_packets.last().unwrap()[0] + audio_packets.last().unwrap()[2]
    );
    assert_eq!(
        observed_packet_clocks(&result_video, "v:0", true).as_slice(),
        video_packets
    );
    assert_two_standard_edits(
        &output.join(audio_track).join("init.mp4"),
        AAC_44100,
        audio_packets[0][0],
    );
}

#[test]
fn dash_uses_the_longer_role_without_widening_shorter_partial_audio_or_losing_video() {
    for video_longer in [false, true] {
        av_dash_case(video_longer);
    }
}
