//! A partial AAC presentation and explicit DRM signaling share the saved CLI workflow.
use drmr_core::{mp4, native};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    process::Command,
};

// Public fixture keys only; the two outputs deliberately use independent domains.
const KID: &str = "000102030405060708090a0b0c0d0e0f";
const DASH_KEY: &str = "101112131415161718191a1b1c1d1e1f";
const HLS_KEY: &str = "202122232425262728292a2b2c2d2e2f";
const LICENSE: &str = "https://license.example.invalid/partial-aac-signaling";
const SKD: &str = "skd://partial-aac-signaling";
// Complete version-one Widevine PSSH: one matching KID and no opaque payload.
const WV_HEX: &str = "000000347073736801000000edef8ba979d64acea3c827dcd51d21ed00000001000102030405060708090a0b0c0d0e0f00000000";
const WV_B64: &str = "AAAANHBzc2gBAAAA7e+LqXnWSs6jyCfc1R0h7QAAAAEAAQIDBAUGBwgJCgsMDQ4PAAAAAA==";
const CODED_FRAMES: u64 = 432;
const PRESENTED_SAMPLES: u64 = (CODED_FRAMES - 1) * 1024 + 240;

fn path(value: &Path) -> String {
    drmr_core::manifest_support::path::fspath(value).unwrap()
}

fn private_json(path: &Path, value: &Value) {
    serde_json::to_writer(native::create_private_new(path).unwrap(), value).unwrap();
}

fn cli(arguments: &[&str], secrets: &[&str]) -> Value {
    let mut command = Command::new(env!("CARGO_BIN_EXE_drmr"));
    for (name, _) in std::env::vars_os() {
        if name
            .to_string_lossy()
            .to_ascii_uppercase()
            .starts_with("DRM_")
        {
            command.env_remove(name);
        }
    }
    let result = command.args(arguments).output().unwrap();
    for bytes in [&result.stdout, &result.stderr] {
        let text = String::from_utf8_lossy(bytes);
        for secret in secrets {
            assert!(
                !text.contains(*secret),
                "operator setting appeared in CLI output"
            );
            assert!(
                !text.contains(secret.to_ascii_uppercase().as_str()),
                "operator setting appeared in CLI output"
            );
        }
    }
    assert!(
        result.status.success(),
        "CLI exited {:?}: {}",
        result.status.code(),
        String::from_utf8_lossy(&result.stderr)
    );
    serde_json::from_slice(&result.stdout).unwrap()
}

fn files(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    std::fs::read_dir(root)
        .unwrap()
        .map(|entry| {
            let file = entry.unwrap().path();
            assert!(file.is_file());
            (
                file.file_name().unwrap().into(),
                std::fs::read(file).unwrap(),
            )
        })
        .collect()
}

fn packets(source: &Path) -> Value {
    let result = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-select_streams",
            "a:0",
            "-show_packets",
            "-show_streams",
            "-show_entries",
            "stream=codec_name,sample_rate,channels,time_base:packet=pts,dts,duration",
            "-of",
            "json",
        ])
        .arg(source)
        .output()
        .unwrap();
    assert!(result.status.success(), "fixture packet probe failed");
    let observed: Value = serde_json::from_slice(&result.stdout).unwrap();
    let stream = &observed["streams"][0];
    assert_eq!(stream["codec_name"], "aac");
    assert_eq!(stream["sample_rate"], "44100");
    assert_eq!(stream["channels"], 2);
    assert_eq!(stream["time_base"], "1/90000");
    for packet in observed["packets"].as_array().unwrap() {
        for clock in ["pts", "dts", "duration"] {
            assert!(packet[clock].as_u64().is_some());
        }
        assert_eq!(packet["pts"], packet["dts"]);
    }
    observed["packets"].clone()
}

fn source(root: &Path, clip: &Path) -> (PathBuf, u64) {
    native::create_media_dir_all(root).unwrap();
    let input = root.join("media.m3u8");
    // The attributed repository clip supplies the source; only fixture preparation encodes.
    let result = Command::new("ffmpeg")
        .args(["-v", "error", "-nostdin", "-threads", "1", "-i"])
        .arg(clip)
        .args([
            "-map",
            "0:a:0",
            "-c:a",
            "aac",
            "-ar",
            "44100",
            "-hls_time",
            "2",
            "-hls_playlist_type",
            "vod",
            "-hls_segment_filename",
        ])
        .arg(root.join("segment-%d.ts"))
        .arg(&input)
        .output()
        .unwrap();
    assert!(result.status.success(), "AAC fixture preparation failed");
    let original = std::fs::read_to_string(&input).unwrap();
    let mut lines: Vec<_> = original.lines().map(str::to_owned).collect();
    let resources: Vec<_> = lines
        .iter()
        .enumerate()
        .filter(|(_, line)| !line.is_empty() && !line.starts_with('#'))
        .map(|(index, name)| {
            assert!(!name.contains(['/', '\\', ':']));
            (index, root.join(name))
        })
        .collect();
    assert_eq!(resources.len(), 6);
    let before = files(root);
    let mut complete_packets = Vec::new();
    for (index, (_, segment)) in resources.iter().enumerate() {
        let observed = packets(segment);
        let rows = observed.as_array().unwrap();
        assert_ne!(rows.as_slice(), &[] as &[Value]);
        if index == resources.len() - 1 {
            assert_eq!(
                rows.len(),
                1,
                "the final resource contains a complete AAC frame"
            );
        }
        complete_packets.extend(rows.iter().cloned());
    }
    assert_eq!(u64::try_from(complete_packets.len()).unwrap(), CODED_FRAMES);
    let final_declaration = resources.last().unwrap().0 - 1;
    assert!(lines[final_declaration].starts_with("#EXTINF:"));
    let seconds = 240_f64 / 44_100_f64;
    let declaration = seconds.to_string();
    assert!(declaration.split_once('.').unwrap().1.len() > 9);
    lines[final_declaration] = format!("#EXTINF:{declaration},");
    assert_eq!(
        original.lines().zip(&lines).filter(|(a, b)| a != b).count(),
        1
    );
    std::fs::write(&input, lines.join("\n") + "\n").unwrap();
    for (name, bytes) in before {
        if name != Path::new("media.m3u8") {
            assert_eq!(std::fs::read(root.join(name)).unwrap(), bytes);
        }
    }
    (input, complete_packets[0]["pts"].as_u64().unwrap())
}

fn payload<'a>(bytes: &'a [u8], nodes: &[mp4::Mp4Box], kind: [u8; 4]) -> &'a [u8] {
    let matching: Vec<_> = mp4::walk(nodes)
        .into_iter()
        .filter(|node| node.kind() == kind)
        .collect();
    assert_eq!(matching.len(), 1);
    matching[0].header.payload(bytes)
}

fn scale(bytes: &[u8]) -> u32 {
    let at = match bytes[0] {
        0 => 12,
        1 => 20,
        _ => panic!("unsupported movie header version"),
    };
    u32::from_be_bytes(bytes[at..at + 4].try_into().unwrap())
}

fn fragments(output: &Path) -> Vec<PathBuf> {
    let mut result: Vec<_> = std::fs::read_dir(output.join("track-0000"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|file| file.extension().is_some_and(|extension| extension == "m4s"))
        .collect();
    result.sort_by_key(|file| {
        file.file_stem()
            .unwrap()
            .to_str()
            .unwrap()
            .strip_prefix("seg_")
            .unwrap()
            .parse::<u64>()
            .unwrap()
    });
    assert!(result.len() > 1);
    result
}

fn assert_audio_init_and_count(output: &Path, origin: u64) -> Vec<u8> {
    let init = std::fs::read(output.join("track-0000/init.mp4")).unwrap();
    let nodes = mp4::parse_boxes(&init).unwrap();
    assert_eq!(scale(payload(&init, &nodes, *b"mvhd")), 4_410_000);
    assert_eq!(scale(payload(&init, &nodes, *b"mdhd")), 90_000);
    assert_eq!(
        mp4::Schm::parse(payload(&init, &nodes, *b"schm"))
            .unwrap()
            .scheme_type,
        *b"cbcs"
    );
    let edit = payload(&init, &nodes, *b"elst");
    assert_eq!(edit.len(), 48);
    assert_eq!(&edit[..8], [1, 0, 0, 0, 0, 0, 0, 2]);
    assert_eq!(
        u64::from_be_bytes(edit[8..16].try_into().unwrap()),
        origin * 49
    );
    assert_eq!(i64::from_be_bytes(edit[16..24].try_into().unwrap()), -1);
    assert_eq!(&edit[24..28], [0, 1, 0, 0]);
    assert_eq!(
        u64::from_be_bytes(edit[28..36].try_into().unwrap()),
        PRESENTED_SAMPLES * 100
    );
    assert_eq!(u64::from_be_bytes(edit[36..44].try_into().unwrap()), origin);
    assert_eq!(&edit[44..48], [0, 1, 0, 0]);
    let mut count = 0_u64;
    for (index, segment) in fragments(output).iter().enumerate() {
        let bytes = std::fs::read(segment).unwrap();
        let nodes = mp4::parse_boxes(&bytes).unwrap();
        if index == 0 {
            let time = payload(&bytes, &nodes, *b"tfdt");
            let first = match time[0] {
                0 => u64::from(u32::from_be_bytes(time[4..8].try_into().unwrap())),
                1 => u64::from_be_bytes(time[4..12].try_into().unwrap()),
                _ => panic!("unsupported decode-time version"),
            };
            assert_eq!(first, origin);
        }
        for run in mp4::walk(&nodes)
            .into_iter()
            .filter(|node| node.kind() == *b"trun")
        {
            let body = run.header.payload(&bytes);
            count += u64::from(u32::from_be_bytes(body[4..8].try_into().unwrap()));
        }
    }
    assert_eq!(count, CODED_FRAMES);
    init
}

fn assert_drm(output: &Path, origin: u64, pr: &drmr_core::playready::PlayReadyObjects) {
    let dash = output.join("dash");
    let init = assert_audio_init_and_count(&dash, origin);
    let nodes = mp4::parse_boxes(&init).unwrap();
    let systems: Vec<_> = mp4::walk(&nodes)
        .into_iter()
        .filter(|node| node.kind() == *b"pssh")
        .map(|node| {
            mp4::Pssh::parse(node.header.payload(&init))
                .unwrap()
                .system_id
        })
        .collect();
    assert_eq!(systems.len(), 2);
    assert!(systems.contains(&drmr_core::playready::WIDEVINE_SYSTEM_ID));
    assert!(systems.contains(&drmr_core::playready::PLAYREADY_SYSTEM_ID));
    let mpd = std::fs::read_to_string(dash.join("manifest.mpd")).unwrap();
    for expected in [
        "urn:mpeg:dash:mp4protection:2011",
        "value=\"cbcs\"",
        "urn:uuid:edef8ba9-79d6-4ace-a3c8-27dcd51d21ed",
        "urn:uuid:9a04f079-9840-4286-ab92-e65be0885f95",
        &format!("<cenc:pssh>{WV_B64}</cenc:pssh>"),
        &format!("<cenc:pssh>{}</cenc:pssh>", pr.pssh_b64),
        &format!("<mspr:pro>{}</mspr:pro>", pr.pro_b64),
    ] {
        assert!(mpd.contains(expected), "required DASH signaling is absent");
    }
    let hls = output.join("hls");
    assert_audio_init_and_count(&hls, origin);
    let playlist = std::fs::read_to_string(hls.join("track-0000/media.m3u8")).unwrap();
    let version = playlist
        .lines()
        .find_map(|line| line.strip_prefix("#EXT-X-VERSION:"))
        .unwrap()
        .parse::<u32>()
        .unwrap();
    assert!(version >= 6);
    assert!(playlist.contains("#EXT-X-MAP:"));
    let declarations: Vec<_> = playlist
        .lines()
        .filter(|line| line.starts_with("#EXT-X-KEY:"))
        .collect();
    assert_eq!(declarations.len(), 1);
    for expected in [
        "METHOD=SAMPLE-AES",
        &format!("URI=\"{SKD}\""),
        "KEYFORMAT=\"com.apple.streamingkeydelivery\"",
        "KEYFORMATVERSIONS=\"1\"",
    ] {
        assert!(
            declarations[0].contains(expected),
            "required FairPlay signaling is absent"
        );
    }
}

fn assert_verification(report: &Value) {
    assert_eq!(report["passed"], true);
    assert_eq!(report["media_verified"], true);
    assert_eq!(report["license_playback_verified"], false);
    assert_eq!(report["checks"]["stream_metadata"], "pass");
    for check in [
        "packet_payloads_equal",
        "packet_timing_equal",
        "decoded_content_equal",
    ] {
        assert_eq!(report["checks"][check], true);
    }
    let lint = &report["native_manifest"];
    assert_eq!(lint["errors"], 0);
    assert_eq!(lint["incomplete"], false);
    assert_eq!(lint["truncated"], false);
    assert!(lint["document_count"].as_u64().unwrap() >= 1);
}

fn settings(root: &Path) -> (PathBuf, PathBuf) {
    let spec = root.join("outputs.json");
    private_json(
        &spec,
        &json!({"schema_version":1,"outputs":[
            {"id":"dash","format":"dash","target":"cbcs","drms":["widevine","playready"]},
            {"id":"hls","format":"hls","target":"cbcs","drms":["fairplay"]}
        ]}),
    );
    let keys = root.join("keys.json");
    private_json(
        &keys,
        &json!({"schema_version":2,"outputs":{
            "dash":{"kid":KID,"key":DASH_KEY,"signaling":{"system":"multi-drm",
                "widevine_pssh":WV_HEX,"playready_license_url":LICENSE}},
            "hls":{"kid":"ffeeddccbbaa99887766554433221100","key":HLS_KEY,
                "hls_key_uri":SKD,"signaling":{"system":"multi-drm","fairplay":true}}
        }}),
    );
    (spec, keys)
}

fn analyze(input: &Path, secrets: &[&str]) {
    let analyzed = cli(
        &["analyze", &path(input), "--max-workers", "1", "--json"],
        secrets,
    );
    assert_eq!(analyzed["input_admitted"], true);
    assert_eq!(analyzed["actual_format"], "hls-ts");
    assert_eq!(analyzed["track_protection"], json!(["clear"]));
    assert_eq!(analyzed["input_checks"]["stream_metadata"], "pass");
    for check in [
        "packet_payloads_equal",
        "packet_timing_equal",
        "decoded_content_equal",
    ] {
        assert_eq!(analyzed["input_checks"][check], true);
    }
}

fn saved_plan(
    input: &Path,
    spec: &Path,
    output: &Path,
    plan: &Path,
    secrets: &[&str],
) -> (Value, Vec<u8>) {
    let planned = cli(
        &[
            "plan",
            &path(input),
            "--outputs",
            &path(spec),
            "--output",
            &path(output),
            "--save",
            &path(plan),
            "--max-workers",
            "1",
            "--json",
        ],
        secrets,
    );
    assert_eq!(planned["schema_version"], 2);
    assert_eq!(planned["kind"], "execution-plan");
    assert_eq!(planned["outputs"].as_array().unwrap().len(), 2);
    let plan_before = std::fs::read(plan).unwrap();
    assert_eq!(
        serde_json::from_slice::<Value>(&plan_before).unwrap(),
        planned
    );
    assert!(!output.exists());
    (planned, plan_before)
}

#[test]
fn partial_aac_tail_saved_bundle_preserves_complete_audio_and_three_drm_declarations() {
    let owner = native::private_tempdir().unwrap();
    let clip = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/bbb/clip.mp4");
    let clip_before = std::fs::read(&clip).unwrap();
    let (input, origin) = source(&owner.path().join("source"), &clip);
    let source_before = files(input.parent().unwrap());
    let output = owner.path().join("result");
    let plan = owner.path().join("conversion.plan.json");
    let (spec, keys) = settings(owner.path());
    let pr = drmr_core::playready::build_playready_v43(KID, LICENSE).unwrap();
    let secrets = [
        DASH_KEY,
        HLS_KEY,
        LICENSE,
        SKD,
        WV_HEX,
        WV_B64,
        &pr.pro_b64,
        &pr.pssh_b64,
        &pr.pssh_hex,
    ];
    analyze(&input, &secrets);
    let (planned, plan_before) = saved_plan(&input, &spec, &output, &plan, &secrets);
    let converted = cli(
        &[
            "convert",
            "--plan",
            &path(&plan),
            "--keys-file",
            &path(&keys),
            "--max-workers",
            "1",
            "--json",
        ],
        &secrets,
    );
    assert_eq!(converted["kind"], "bundle-conversion");
    assert_eq!(converted["plan_id"], planned["plan_id"]);
    for row in converted["outputs"].as_array().unwrap() {
        assert_verification(&row["conversion"]["verification"]);
        assert_eq!(row["signaling"]["verified"], true);
    }
    assert_drm(&output, origin, &pr);
    let verified = cli(
        &[
            "verify",
            "--plan",
            &path(&plan),
            "--keys-file",
            &path(&keys),
            "--max-workers",
            "1",
            "--json",
        ],
        &secrets,
    );
    assert_eq!(verified["kind"], "bundle-verification");
    assert_eq!(verified["plan_id"], planned["plan_id"]);
    assert_eq!(verified["passed"], true);
    assert_eq!(verified["source_unchanged"], true);
    assert_eq!(verified["outputs"].as_array().unwrap().len(), 2);
    for row in verified["outputs"].as_array().unwrap() {
        assert_eq!(row["output_matches_plan"], true);
        assert_eq!(row["signaling"]["verified"], true);
        assert_eq!(
            row["signaling"]["systems"],
            if row["id"] == "dash" {
                json!(["widevine", "playready"])
            } else {
                json!(["fairplay"])
            }
        );
        assert_verification(&row["verification"]);
    }
    assert_eq!(std::fs::read(&clip).unwrap(), clip_before);
    assert_eq!(files(input.parent().unwrap()), source_before);
    assert_eq!(std::fs::read(&plan).unwrap(), plan_before);
    eprintln!(
        "{}",
        json!({"workflow":"saved-plan-partial-aac-multi-drm",
        "sample_rate":44100,"coded_aac_frames":CODED_FRAMES,
        "raw_samples_per_channel":CODED_FRAMES * 1024,"presented_samples_per_channel":PRESENTED_SAMPLES,
        "outputs":["hls-cbcs-fairplay","dash-cbcs-widevine-playready"],
        "fresh_verification":true,"source_and_plan_unchanged":true,
        "operator_settings_absent_from_reports":true,"license_playback_verified":false})
    );
}
