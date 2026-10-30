//! The public CLI saves and verifies transport outputs from encrypted fragmented MP4.
use serde_json::{Value, json};
use std::{path::Path, process::Command};
const KID: &str = "000102030405060708090a0b0c0d0e0f";
const KEY: &str = "101112131415161718191a1b1c1d1e1f";
const IV: &str = "202122232425262728292a2b2c2d2e2f";
fn path(p: &Path) -> String {
    drmr_core::manifest_support::path::fspath(p).unwrap()
}
fn cli(args: &[String]) -> Value {
    let out = Command::new(env!("CARGO_BIN_EXE_drmr"))
        .args(args)
        .args(["--json", "--progress", "off"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    for bytes in [&out.stdout, &out.stderr] {
        assert!(!String::from_utf8_lossy(bytes).contains(KEY));
    }
    serde_json::from_slice(&out.stdout).unwrap()
}
fn checked(v: &Value) {
    assert_eq!(v["passed"], true);
    assert_eq!(v["required_depth"], "native");
    assert_eq!(v["decode_evidence"]["status"], "not-checked");
}
fn source(root: &Path) -> std::path::PathBuf {
    drmr_core::native::create_media_dir_all(root).unwrap();
    let clip = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/bbb/clip.mp4");
    let out = Command::new("packager")
        .args([
            "--single_threaded",
            "--generate_static_live_mpd",
            "--clear_lead",
            "0",
            "--segment_duration",
            "2",
            "--enable_raw_key_encryption",
            "--protection_scheme",
            "cenc",
            "--keys",
        ])
        .arg(format!("label=:key_id={KID}:key={KEY}"))
        .arg(format!(
            "in={},stream=video,init_segment={},segment_template={}",
            clip.display(),
            root.join("init.mp4").display(),
            root.join("seg-$Number$.m4s").display()
        ))
        .arg("--mpd_output")
        .arg(root.join("manifest.mpd"))
        .output()
        .unwrap();
    assert!(out.status.success());
    root.join("manifest.mpd")
}
fn write(path: &Path, value: &Value) {
    serde_json::to_writer(drmr_core::native::create_private_new(path).unwrap(), value).unwrap();
}
#[test]
fn saved_and_named_transport_outputs_use_the_common_cli_and_independent_checks() {
    let root = drmr_core::native::private_tempdir().unwrap();
    let input = source(&root.path().join("input"));
    let fairplay = json!({"key":KEY,"iv":IV,"hls_key_uri":"skd://synthetic-transport","signaling":{"system":"multi-drm","fairplay":true}});
    let input_keys = json!({"keys":[{"kid":KID,"key":KEY}]});
    for target in ["clear", "hls-sample-aes"] {
        let keys = root.path().join(format!("{target}.keys.json"));
        let output = root.path().join(target);
        let saved = root.path().join(format!("{target}.plan.json"));
        let mut config = json!({"schema_version":1,"input":input_keys});
        if target != "clear" {
            config["output"] = fairplay.clone();
        }
        write(&keys, &config);
        cli(&[
            "plan".into(),
            path(&input),
            "--to".into(),
            target.into(),
            "--format".into(),
            "hls-ts".into(),
            "--processing".into(),
            "stitch".into(),
            "--keys-file".into(),
            path(&keys),
            "--output".into(),
            path(&output),
            "--save".into(),
            path(&saved),
        ]);
        let converted = cli(&[
            "convert".into(),
            "--plan".into(),
            path(&saved),
            "--keys-file".into(),
            path(&keys),
        ]);
        checked(&converted["verification"]);
        let verified = cli(&[
            "verify".into(),
            "--plan".into(),
            path(&saved),
            "--keys-file".into(),
            path(&keys),
        ]);
        assert_eq!(verified["output_matches_plan"], true);
        checked(&verified["verification"]);
        checked(&cli(&[
            "verify".into(),
            path(&output.join("master.m3u8")),
            "--source".into(),
            path(&input),
            "--keys-file".into(),
            path(&keys),
        ]));
    }
    let recipe = root.path().join("outputs.json");
    let keys = root.path().join("bundle.keys.json");
    let output = root.path().join("bundle");
    let saved = root.path().join("bundle.plan.json");
    write(
        &recipe,
        &json!({"schema_version":3,"outputs":[{"id":"clear","format":"hls-ts","target":"clear","drms":[],"processing":"stitch"},{"id":"fairplay","format":"hls-ts","target":"hls-sample-aes","processing":"stitch","drms":["fairplay"]}]}),
    );
    write(
        &keys,
        &json!({"schema_version":2,"input":input_keys,"outputs":{"fairplay":fairplay}}),
    );
    cli(&[
        "plan".into(),
        path(&input),
        "--outputs".into(),
        path(&recipe),
        "--keys-file".into(),
        path(&keys),
        "--output".into(),
        path(&output),
        "--save".into(),
        path(&saved),
    ]);
    let converted = cli(&[
        "convert".into(),
        "--plan".into(),
        path(&saved),
        "--keys-file".into(),
        path(&keys),
    ]);
    assert_eq!(converted["outputs"].as_array().unwrap().len(), 2);
    for output in converted["outputs"].as_array().unwrap() {
        checked(&output["conversion"]["verification"]);
    }
    let verified = cli(&[
        "verify".into(),
        "--plan".into(),
        path(&saved),
        "--keys-file".into(),
        path(&keys),
    ]);
    assert_eq!(verified["passed"], true);
}
