//! The actual CLI retains every selected TS track in standalone fragmented MP4.
use drmr_core::{mp4, native};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    process::Command,
};

// Public fixture material, never production credentials.
const KID: &str = "00112233445566778899aabbccddeeff";
const KEY: &str = "101112131415161718191a1b1c1d1e1f";
const PRESENTED_SAMPLES: u64 = 431 * 1024 + 240;

fn cli(root: &Path, arguments: &[&str]) -> Value {
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
    let result = command.current_dir(root).args(arguments).output().unwrap();
    for bytes in [&result.stdout, &result.stderr] {
        let text = String::from_utf8_lossy(bytes);
        assert!(!text.to_ascii_lowercase().contains(KEY));
    }
    assert!(
        result.status.success(),
        "CLI {:?} failed: {} {}",
        arguments.first(),
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
    serde_json::from_slice(&result.stdout).unwrap()
}

fn private_json(path: &Path, value: &Value) {
    serde_json::to_writer(native::create_private_new(path).unwrap(), value).unwrap();
}

fn files(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    fn visit(root: &Path, at: &Path, files: &mut BTreeMap<PathBuf, Vec<u8>>) {
        for entry in std::fs::read_dir(at).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                visit(root, &path, files);
            } else {
                files.insert(
                    path.strip_prefix(root).unwrap().into(),
                    std::fs::read(path).unwrap(),
                );
            }
        }
    }
    let mut result = BTreeMap::new();
    visit(root, root, &mut result);
    result
}

fn source(root: &Path) {
    let clip = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/bbb/clip.mp4");
    for (name, options) in [
        ("video", vec!["-map", "0:v:0", "-c:v", "copy"]),
        (
            "audio",
            vec!["-map", "0:a:0", "-c:a", "aac", "-ar", "44100"],
        ),
    ] {
        let directory = root.join("source").join(name);
        native::create_media_dir_all(&directory).unwrap();
        // Only fixture preparation encodes the attributed ten-second repository clip.
        let generated = Command::new("ffmpeg")
            .args(["-v", "error", "-nostdin", "-threads", "1", "-i"])
            .arg(&clip)
            .args(options)
            .args([
                "-hls_time",
                "2",
                "-hls_playlist_type",
                "vod",
                "-hls_segment_filename",
            ])
            .arg(directory.join("segment-%d.ts"))
            .arg(directory.join("media.m3u8"))
            .output()
            .unwrap();
        assert!(
            generated.status.success(),
            "licensed TS fixture generation failed"
        );
    }
    let audio = root.join("source/audio/media.m3u8");
    let original = std::fs::read_to_string(&audio).unwrap();
    let mut lines: Vec<_> = original.lines().map(str::to_owned).collect();
    let final_resource = lines
        .iter()
        .rposition(|line| !line.is_empty() && !line.starts_with('#'))
        .unwrap();
    assert_eq!(
        lines
            .iter()
            .filter(|line| line.starts_with("#EXTINF:"))
            .count(),
        6
    );
    assert!(lines[final_resource - 1].starts_with("#EXTINF:"));
    lines[final_resource - 1] = format!("#EXTINF:{},", 240_f64 / 44_100_f64);
    std::fs::write(audio, lines.join("\n") + "\n").unwrap();
    std::fs::write(root.join("source/master.m3u8"), concat!(
        "#EXTM3U\n",
        "#EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"audio\",NAME=\"Main\",LANGUAGE=\"en\",URI=\"audio/media.m3u8\"\n",
        "#EXT-X-STREAM-INF:BANDWIDTH=1000000,RESOLUTION=320x180,AUDIO=\"audio\"\nvideo/media.m3u8\n"
    )).unwrap();
}

fn proof(report: &Value) {
    assert_eq!(report["schema_version"], 2);
    assert_eq!(report["passed"], true, "{report}");
    assert_eq!(report["required_depth"], "native");
    assert_eq!(
        report["decode_evidence"],
        json!({"status":"not-checked","reason":"not-requested"})
    );
    assert_eq!(report["license_playback_verified"], false);
    assert_eq!(report["checks"]["stream_metadata"], "pass");
    assert_eq!(report["checks"]["packet_payloads_equal"], true);
    assert_eq!(report["checks"]["packet_timing_equal"], true);
}

fn payload<'a>(bytes: &'a [u8], nodes: &[mp4::Mp4Box], kind: [u8; 4]) -> &'a [u8] {
    let found: Vec<_> = mp4::walk(nodes)
        .into_iter()
        .filter(|node| node.kind() == kind)
        .collect();
    assert_eq!(found.len(), 1);
    found[0].header.payload(bytes)
}

fn mp4_profile(path: &Path, target: &str) {
    let bytes = std::fs::read(path).unwrap();
    let nodes = mp4::parse_boxes(&bytes).unwrap();
    assert!(nodes.iter().any(|node| node.kind() == *b"moof"));
    let brands = payload(&bytes, &nodes, *b"ftyp");
    assert!(
        !brands
            .as_chunks::<4>()
            .0
            .iter()
            .any(|brand| brand.starts_with(b"cmf"))
    );
    let movie = payload(&bytes, &nodes, *b"mvhd");
    let movie_scale = u32::from_be_bytes(movie[12..16].try_into().unwrap());
    let tracks: Vec<_> = mp4::walk(&nodes)
        .into_iter()
        .filter(|node| node.kind() == *b"trak")
        .collect();
    assert_eq!(tracks.len(), 2);
    let mut audio_found = false;
    for track in tracks {
        let children = mp4::parse_boxes(track.header.payload(&bytes)).unwrap();
        let handler = payload(track.header.payload(&bytes), &children, *b"hdlr");
        if handler.get(8..12) != Some(b"soun") {
            continue;
        }
        audio_found = true;
        let media = payload(track.header.payload(&bytes), &children, *b"mdhd");
        let language_at = if media[0] == 0 { 20 } else { 32 };
        let language = u16::from_be_bytes(media[language_at..language_at + 2].try_into().unwrap());
        assert_eq!(
            language,
            (5 << 10) | (14 << 5) | 7,
            "en is represented by the MP4 code eng"
        );
        let edit = payload(track.header.payload(&bytes), &children, *b"elst");
        assert_eq!(&edit[..8], [1, 0, 0, 0, 0, 0, 0, 2]);
        let duration = u64::from_be_bytes(edit[28..36].try_into().unwrap());
        assert_eq!(
            u128::from(duration) * 44_100,
            u128::from(PRESENTED_SAMPLES) * u128::from(movie_scale)
        );
    }
    assert!(audio_found);
    let schemes: Vec<_> = mp4::walk(&nodes)
        .into_iter()
        .filter(|node| node.kind() == *b"schm")
        .map(|node| {
            mp4::Schm::parse(node.header.payload(&bytes))
                .unwrap()
                .scheme_type
        })
        .collect();
    if target == "clear" {
        assert_eq!(schemes, [] as [[u8; 4]; 0]);
    } else {
        assert_eq!(schemes, vec![target.as_bytes(); 2]);
    }
}

fn converted(report: &Value, target: &str) {
    assert_eq!(report["schema_version"], 2);
    assert_eq!(report["output"]["format"], "mp4");
    assert_eq!(report["output"]["protection"], target);
    assert_eq!(report["output"]["tracks"].as_array().unwrap().len(), 2);
    assert_eq!(report["processing"]["route"], "fragmented-mp4");
    assert_eq!(report["processing"]["boundaries"], "not-applicable");
    proof(&report["verification"]);
}

fn saved_mp4_plan(root: &Path, target: &str, output: &str, plan_name: &str) -> (Value, Vec<u8>) {
    let candidate = cli(
        root,
        &[
            "plan",
            "source/master.m3u8",
            "--to",
            target,
            "--format",
            "mp4",
            "--json",
        ],
    );
    assert_eq!(candidate["execution_available"], true);
    assert_eq!(candidate["input"]["media_verified"], false);
    let plan = cli(
        root,
        &[
            "plan",
            "source/master.m3u8",
            "--to",
            target,
            "--format",
            "mp4",
            "--output",
            output,
            "--save",
            plan_name,
            "--json",
        ],
    );
    assert_eq!(plan["schema_version"], 9);
    assert_eq!(plan["kind"], "execution-plan");
    assert_eq!(plan["source_format"], "hls-ts");
    assert_eq!(plan["intent"]["format"], "mp4");
    assert_eq!(plan["intent"]["target"], target);
    assert_eq!(plan["required_depth"], "native");
    let saved = std::fs::read(root.join(plan_name)).unwrap();
    assert_eq!(serde_json::from_slice::<Value>(&saved).unwrap(), plan);
    assert!(!root.join(output).exists());
    (plan, saved)
}

#[test]
fn native_ts_mp4_cli_analyzes_plans_converts_and_independently_verifies_every_profile() {
    let owner = native::private_tempdir().unwrap();
    let root = owner.path().join("CLI media files");
    native::create_private_dir_all(&root).unwrap();
    native::create_private_dir_all(&root.join("plans")).unwrap();
    source(&root);
    let original = files(&root.join("source"));
    let analysis = cli(&root, &["analyze", "source/master.m3u8", "--json"]);
    assert_eq!(analysis["kind"], "input-analysis");
    assert_eq!(analysis["schema_version"], 2);
    assert_eq!(analysis["actual_format"], "hls-ts");
    assert_eq!(analysis["input_admitted"], true);
    assert!(
        analysis["eligible_formats"]
            .as_array()
            .unwrap()
            .contains(&json!("mp4"))
    );
    proof(&analysis["verification"]);
    for target in ["clear", "cenc", "cbcs"] {
        let keys_name = format!("{target} keys.json");
        private_json(
            &root.join(&keys_name),
            &if target == "clear" {
                json!({"schema_version":1})
            } else {
                json!({"schema_version":1,"output":{"kid":KID,"key":KEY}})
            },
        );
        let plan_name = format!("plans/{target} plan.json");
        let output = format!("{target} result.mp4");
        let (plan, saved) = saved_mp4_plan(&root, target, &output, &plan_name);
        converted(
            &cli(
                &root,
                &[
                    "convert",
                    "--plan",
                    &plan_name,
                    "--keys-file",
                    &keys_name,
                    "--json",
                ],
            ),
            target,
        );
        mp4_profile(&root.join(&output), target);
        let verified = cli(
            &root,
            &[
                "verify",
                "--plan",
                &plan_name,
                "--keys-file",
                &keys_name,
                "--json",
            ],
        );
        assert_eq!(verified["kind"], "plan-verification");
        assert_eq!(verified["plan_id"], plan["plan_id"]);
        assert_eq!(verified["source_unchanged"], true);
        assert_eq!(verified["output_matches_plan"], true);
        proof(&verified["verification"]);
        proof(&cli(
            &root,
            &[
                "verify",
                &output,
                "--source",
                "source/master.m3u8",
                "--keys-file",
                &keys_name,
                "--json",
            ],
        ));
        let direct = format!("{target} direct.mp4");
        converted(
            &cli(
                &root,
                &[
                    "convert",
                    "source/master.m3u8",
                    "--format",
                    "mp4",
                    "--to",
                    target,
                    "--output",
                    &direct,
                    "--keys-file",
                    &keys_name,
                    "--json",
                ],
            ),
            target,
        );
        mp4_profile(&root.join(&direct), target);
        assert_eq!(std::fs::read(root.join(&plan_name)).unwrap(), saved);
    }
    assert_eq!(files(&root.join("source")), original);
}

#[test]
fn native_ts_mp4_and_dash_share_one_bundle_without_losing_the_audio_tail_or_a_track() {
    let owner = native::private_tempdir().unwrap();
    let root = owner.path();
    source(root);
    let original = files(&root.join("source"));
    private_json(
        &root.join("outputs.json"),
        &json!({"schema_version":1,"outputs":[
            {"id":"mp4","format":"mp4","target":"cbcs","drms":[]},
            {"id":"dash","format":"dash","target":"clear","drms":[]}
        ]}),
    );
    private_json(
        &root.join("keys.json"),
        &json!({"schema_version":2,"outputs":{"mp4":{"kid":KID,"key":KEY}}}),
    );
    let plan = cli(
        root,
        &[
            "plan",
            "source/master.m3u8",
            "--outputs",
            "outputs.json",
            "--output",
            "saved result",
            "--save",
            "bundle.plan.json",
            "--json",
        ],
    );
    assert_eq!(plan["schema_version"], 10);
    assert_eq!(plan["outputs"].as_array().unwrap().len(), 2);
    let saved = std::fs::read(root.join("bundle.plan.json")).unwrap();
    let result = cli(
        root,
        &[
            "convert",
            "--plan",
            "bundle.plan.json",
            "--keys-file",
            "keys.json",
            "--json",
        ],
    );
    assert_eq!(result["kind"], "bundle-conversion");
    bundle_converted(root, "saved result", &result);
    let verified = cli(
        root,
        &[
            "verify",
            "--plan",
            "bundle.plan.json",
            "--keys-file",
            "keys.json",
            "--json",
        ],
    );
    assert_eq!(verified["kind"], "bundle-verification");
    assert_eq!(verified["plan_id"], plan["plan_id"]);
    assert_eq!(verified["passed"], true);
    assert_eq!(verified["source_unchanged"], true);
    for output in verified["outputs"].as_array().unwrap() {
        assert_eq!(output["output_matches_plan"], true);
        proof(&output["verification"]);
    }
    let direct = cli(
        root,
        &[
            "convert",
            "source/master.m3u8",
            "--outputs",
            "outputs.json",
            "--keys-file",
            "keys.json",
            "--output",
            "direct result",
            "--json",
        ],
    );
    bundle_converted(root, "direct result", &direct);
    assert_eq!(std::fs::read(root.join("bundle.plan.json")).unwrap(), saved);
    assert_eq!(files(&root.join("source")), original);
}

fn bundle_converted(root: &Path, name: &str, report: &Value) {
    assert_eq!(report["outputs"].as_array().unwrap().len(), 2);
    for row in report["outputs"].as_array().unwrap() {
        let conversion = &row["conversion"];
        assert_eq!(conversion["output"]["tracks"].as_array().unwrap().len(), 2);
        proof(&conversion["verification"]);
        if row["id"] == "mp4" {
            converted(conversion, "cbcs");
            mp4_profile(&root.join(name).join("mp4/media.mp4"), "cbcs");
        } else {
            assert_eq!(row["id"], "dash");
            assert_eq!(conversion["output"]["format"], "dash");
            assert!(root.join(name).join("dash/manifest.mpd").is_file());
            assert_eq!(conversion["verification"]["native_manifest"]["errors"], 0);
        }
    }
}
