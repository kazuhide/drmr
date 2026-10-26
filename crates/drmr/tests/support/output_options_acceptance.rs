use super::invoke;
use serde_json::{Value, json};
use std::{path::Path, process::Command};

fn source(path: &Path) {
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/bbb/clip.mp4");
    let result = Command::new("ffmpeg")
        .args(["-v", "error", "-nostdin", "-i"])
        .arg(fixture)
        .args([
            "-map",
            "0:v:0",
            "-an",
            "-c",
            "copy",
            "-map_metadata",
            "-1",
            "-bitexact",
            "-movflags",
            "delay_moov+default_base_moof+frag_keyframe",
        ])
        .arg(path)
        .output()
        .unwrap();
    assert!(result.status.success(), "fixture preparation failed");
}

fn report(args: &[&str]) -> Value {
    let output = invoke(args);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

#[test]
fn current_single_intent_keeps_source_layout_and_each_dash_choice_through_execution() {
    let root = drmr_core::native::private_tempdir().unwrap();
    let input = root.path().join("source.mp4");
    source(&input);
    let original = std::fs::read(&input).unwrap();
    for (index, mode) in [
        None,
        Some("template"),
        Some("template-timeline"),
        Some("list"),
        Some("base"),
    ]
    .into_iter()
    .enumerate()
    {
        let output = root.path().join(format!("result-{index}"));
        let saved = root.path().join(format!("plan-{index}.json"));
        let mut args = vec![
            "plan",
            input.to_str().unwrap(),
            "--to",
            "clear",
            "--format",
            "dash",
            "--output",
            output.to_str().unwrap(),
            "--save",
            saved.to_str().unwrap(),
            "--json",
        ];
        if let Some(mode) = mode {
            args.extend(["--dash-addressing", mode]);
        }
        let plan = report(&args);
        assert_eq!(plan["schema_version"], 7);
        assert_eq!(plan["output_options"]["layout"]["mode"], "source");
        assert_eq!(
            plan["output_options"]["dash_addressing"],
            mode.unwrap_or("template")
        );
        let converted = report(&["convert", "--plan", saved.to_str().unwrap(), "--json"]);
        assert_eq!(converted["verification"]["passed"], true);
        let manifest = std::fs::read_to_string(output.join("manifest.mpd")).unwrap();
        match mode.unwrap_or("template") {
            "template" => {
                assert!(manifest.contains("<SegmentTemplate"));
                assert!(!manifest.contains("<SegmentTimeline"));
            }
            "template-timeline" => {
                assert!(manifest.contains("<SegmentTemplate"));
                assert!(manifest.contains("<SegmentTimeline"));
            }
            "list" => assert!(manifest.contains("<SegmentList")),
            "base" => assert!(manifest.contains("<SegmentBase")),
            _ => unreachable!(),
        }
        let verified = report(&["verify", "--plan", saved.to_str().unwrap(), "--json"]);
        assert_eq!(verified["verification"]["passed"], true);
        assert_eq!(verified["output_matches_plan"], true);
    }
    assert_eq!(std::fs::read(input).unwrap(), original);
}

#[test]
fn bundle_dash_overrides_take_precedence_and_never_reach_other_formats() {
    let root = drmr_core::native::private_tempdir().unwrap();
    let input = root.path().join("source.mp4");
    source(&input);
    let specification = root.path().join("outputs.json");
    std::fs::write(
        &specification,
        serde_json::to_vec(&json!({"schema_version":3,"outputs":[
            {"id":"dash","format":"dash","target":"clear","drms":[],"dash_addressing":"list"},
            {"id":"hls","format":"hls","target":"clear","drms":[]}
        ]}))
        .unwrap(),
    )
    .unwrap();
    let saved = root.path().join("plan.json");
    let output = root.path().join("result");
    let plan = report(&[
        "plan",
        input.to_str().unwrap(),
        "--outputs",
        specification.to_str().unwrap(),
        "--dash-addressing",
        "base",
        "--output",
        output.to_str().unwrap(),
        "--save",
        saved.to_str().unwrap(),
        "--json",
    ]);
    assert_eq!(plan["schema_version"], 8);
    assert_eq!(
        plan["output_options"][0]["options"]["dash_addressing"],
        "list"
    );
    assert!(
        plan["output_options"][1]["options"]
            .get("dash_addressing")
            .is_none()
    );
    for options in plan["output_options"].as_array().unwrap() {
        assert_eq!(options["options"]["layout"]["mode"], "source");
    }
    let converted = report(&["convert", "--plan", saved.to_str().unwrap(), "--json"]);
    assert!(
        converted["outputs"]
            .as_array()
            .unwrap()
            .iter()
            .all(|row| row["conversion"]["verification"]["passed"] == true)
    );
    let verified = report(&["verify", "--plan", saved.to_str().unwrap(), "--json"]);
    assert_eq!(verified["passed"], true);
}

#[test]
fn an_explicit_dash_choice_requires_at_least_one_dash_output() {
    let root = drmr_core::native::private_tempdir().unwrap();
    let specification = root.path().join("outputs.json");
    std::fs::write(&specification, br#"{"schema_version":3,"outputs":[{"id":"mp4","format":"mp4","target":"clear","drms":[]}]}"#).unwrap();
    for args in [
        vec![
            "convert",
            "missing.mp4",
            "--to",
            "clear",
            "--format",
            "hls",
            "--output",
            "result",
            "--dash-addressing",
            "template",
            "--json",
        ],
        vec![
            "plan",
            "missing.mp4",
            "--to",
            "clear",
            "--format",
            "mp4",
            "--output",
            "result",
            "--save",
            "plan.json",
            "--dash-addressing",
            "base",
            "--json",
        ],
        vec![
            "plan",
            "missing.mp4",
            "--outputs",
            specification.to_str().unwrap(),
            "--output",
            "result",
            "--save",
            "plan.json",
            "--dash-addressing",
            "template",
            "--json",
        ],
    ] {
        let result = invoke(&args);
        assert_eq!(result.status.code(), Some(1));
        let report: Value = serde_json::from_slice(&result.stdout).unwrap();
        assert_eq!(report["error"]["code"], "dash-addressing-requires-dash");
    }
}
