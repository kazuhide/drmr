//! Published configuration examples pass through the same readers as CLI jobs.
use drmr_core::{
    conversion::{
        ConversionError, DrmSystem,
        key_file::{self, BundleOperatorKeys},
        plan::bundle::{BundleOutputIntent, ManifestValidation, read_output_spec},
    },
    inspection::{OutputFormat, TargetProtection},
    native,
};
use serde_json::{Value, json};
use std::{
    collections::BTreeSet,
    io::Write as _,
    path::{Path, PathBuf},
};

const PROFILES: [(&str, OutputFormat, TargetProtection); 13] = [
    ("dash-cbcs", OutputFormat::Dash, TargetProtection::Cbcs),
    ("dash-cenc", OutputFormat::Dash, TargetProtection::Cenc),
    ("dash-clear", OutputFormat::Dash, TargetProtection::Clear),
    ("hls-cbcs", OutputFormat::Hls, TargetProtection::Cbcs),
    ("hls-cenc", OutputFormat::Hls, TargetProtection::Cenc),
    ("hls-clear", OutputFormat::Hls, TargetProtection::Clear),
    (
        "hls-hls-aes128",
        OutputFormat::Hls,
        TargetProtection::HlsAes128,
    ),
    ("hls-ts-clear", OutputFormat::HlsTs, TargetProtection::Clear),
    (
        "hls-ts-hls-aes128",
        OutputFormat::HlsTs,
        TargetProtection::HlsAes128,
    ),
    (
        "hls-ts-hls-sample-aes",
        OutputFormat::HlsTs,
        TargetProtection::HlsSampleAes,
    ),
    ("mp4-cbcs", OutputFormat::Mp4, TargetProtection::Cbcs),
    ("mp4-cenc", OutputFormat::Mp4, TargetProtection::Cenc),
    ("mp4-clear", OutputFormat::Mp4, TargetProtection::Clear),
];
const INPUT_MODES: [&str; 4] = ["clear", "cenc-cbcs", "hls-aes128", "hls-sample-aes"];

fn repository() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn example(kind: &str, name: &str) -> PathBuf {
    repository()
        .join("docs/examples")
        .join(kind)
        .join(format!("{name}.json"))
}

fn json_files(kind: &str) -> BTreeSet<PathBuf> {
    std::fs::read_dir(repository().join("docs/examples").join(kind))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "json")
        })
        .collect()
}

fn private_copy<T>(bytes: &[u8], read: impl FnOnce(&Path) -> T) -> T {
    let owner = native::private_tempdir().unwrap();
    let path = owner.path().join("configuration.json");
    let mut file = native::create_private_new(&path).unwrap();
    file.write_all(bytes).unwrap();
    drop(file);
    read(&path)
}

fn read_spec(value: &Value) -> Result<Vec<BundleOutputIntent>, ConversionError> {
    private_copy(&serde_json::to_vec(value).unwrap(), read_output_spec)
}

fn read_keys(value: &Value) -> Result<BundleOperatorKeys, ConversionError> {
    private_copy(&serde_json::to_vec(value).unwrap(), key_file::read_bundle)
}

fn public_json(kind: &str, name: &str) -> Value {
    serde_json::from_slice(&std::fs::read(example(kind, name)).unwrap()).unwrap()
}

#[test]
fn every_published_output_recipe_has_the_named_typed_profile() {
    let expected = PROFILES
        .iter()
        .map(|(name, _, _)| example("outputs", name))
        .collect();
    assert_eq!(json_files("outputs"), expected);
    for (name, format, target) in PROFILES {
        let outputs = read_output_spec(&example("outputs", name))
            .unwrap_or_else(|error| panic!("{name}: {error}"));
        assert_eq!(outputs.len(), 1, "{name}");
        let output = &outputs[0];
        assert_eq!(output.format, format, "{name}");
        assert_eq!(output.target, target, "{name}");
        assert_eq!(name, format!("{}-{}", output.id, target.as_str()));
        assert!(output.drms.is_empty(), "{name}");
        assert_eq!(output.conformance, ManifestValidation::None, "{name}");
        assert_eq!(output.processing, None, "{name}");
        assert_eq!(output.dash_addressing, None, "{name}");
    }
}

fn assert_input_role(value: &Value, mode: &str, name: &str) {
    // The reader validates key bytes; the published fields select their intended role.
    if mode == "clear" {
        assert!(value.get("input").is_none(), "{name}");
        return;
    }
    let input = value["input"].as_object().unwrap();
    assert_eq!(input.len(), 1, "{name}");
    let field = if mode == "cenc-cbcs" {
        "keys"
    } else {
        "hls_keys"
    };
    let keys = input[field].as_array().unwrap();
    assert_eq!(keys.len(), 1, "{name}");
    let key = &keys[0];
    assert_eq!(key.get("kid").is_some(), mode == "cenc-cbcs", "{name}");
    assert_eq!(key.get("uri").is_some(), mode != "cenc-cbcs", "{name}");
    assert_eq!(key.get("iv").is_some(), mode == "hls-sample-aes", "{name}");
    for output in value["outputs"].as_object().unwrap().values() {
        assert!(key["key"] != output["key"], "{name}: separate key domains");
        if mode == "cenc-cbcs" && output.get("kid").is_some() {
            assert!(key["kid"] != output["kid"], "{name}: separate KID domains");
        }
    }
}

#[test]
fn every_published_key_bundle_matches_its_source_role_and_named_output() {
    let mut expected = BTreeSet::new();
    for (profile, _, target) in PROFILES {
        for mode in INPUT_MODES {
            let name = format!("{profile}-from-{mode}");
            if target == TargetProtection::Clear && mode == "clear" {
                assert!(
                    !example("keys", &name).exists(),
                    "{name}: no key file needed"
                );
                continue;
            }
            expected.insert(example("keys", &name));
        }
    }
    assert_eq!(expected.len(), 48);
    let actual = json_files("keys");
    assert_eq!(actual, expected);
    for path in actual {
        let name = path.file_stem().unwrap().to_str().unwrap();
        let (profile, mode) = name.split_once("-from-").unwrap();
        let bytes = std::fs::read(&path).unwrap();
        let keys = private_copy(&bytes, key_file::read_bundle)
            .unwrap_or_else(|error| panic!("{name}: {error}"));
        let value: Value = serde_json::from_slice(&bytes).unwrap();
        assert_input_role(&value, mode, name);
        let outputs = read_output_spec(&example("outputs", profile)).unwrap();
        let output = &outputs[0];
        if output.target == TargetProtection::Clear {
            assert!(keys.outputs.is_empty(), "{name}");
            continue;
        }
        assert_eq!(keys.outputs.len(), 1, "{name}");
        let key = keys
            .outputs
            .get(&output.id)
            .unwrap_or_else(|| panic!("{name}: matching output ID"));
        let sample_key = matches!(
            output.target,
            TargetProtection::Cenc | TargetProtection::Cbcs
        );
        assert_eq!(key.output.is_some(), sample_key, "{name}");
        assert_eq!(key.hls_output.is_some(), !sample_key, "{name}");
        assert!(key.signaling.is_none(), "{name}");
        let hls = matches!(output.format, OutputFormat::Hls | OutputFormat::HlsTs);
        assert_eq!(key.hls_output_uri().is_some(), hls, "{name}");
        if hls {
            assert!(
                key.hls_output_uri() == value["outputs"][&output.id]["hls_key_uri"].as_str(),
                "{name}: output URI mapping"
            );
        }
        if let Some(hls_key) = &key.hls_output {
            assert_eq!(
                hls_key.associated_iv().is_some(),
                output.target == TargetProtection::HlsSampleAes,
                "{name}"
            );
        }
    }
}

fn documented_json() -> Vec<Value> {
    let text = std::fs::read_to_string(repository().join("docs/multiple-outputs.md"))
        .unwrap()
        .replace("\r\n", "\n");
    text.split("```json\n")
        .skip(1)
        .map(|block| serde_json::from_str(block.split_once("\n```").unwrap().0).unwrap())
        .collect()
}

fn assert_drm_bindings(outputs: &[BundleOutputIntent], keys: &BundleOperatorKeys) {
    assert_eq!(outputs.len(), keys.outputs.len());
    for output in outputs {
        let key = &keys.outputs[&output.id];
        assert_eq!(key.signaling.as_ref().unwrap().systems(), output.drms);
    }
}

#[test]
fn documented_multi_drm_profiles_normalize_and_bind_named_keys() {
    let examples = documented_json();
    let recipe = examples
        .iter()
        .find(|value| value["outputs"].is_array())
        .unwrap();
    let config = examples
        .iter()
        .find(|value| value["outputs"].is_object())
        .unwrap();
    let outputs = read_spec(recipe).unwrap();
    let keys = read_keys(config).unwrap();
    assert_drm_bindings(&outputs, &keys);
    assert_eq!(outputs[0].drms, [DrmSystem::Widevine, DrmSystem::PlayReady]);
    assert_eq!(
        outputs[1].drms,
        [
            DrmSystem::Widevine,
            DrmSystem::PlayReady,
            DrmSystem::FairPlay
        ]
    );
    let mut reversed = recipe.clone();
    for output in reversed["outputs"].as_array_mut().unwrap() {
        output["drms"].as_array_mut().unwrap().reverse();
    }
    assert_eq!(read_spec(&reversed).unwrap(), outputs);

    let ts = examples
        .iter()
        .find(|value| value["id"] == "hls-ts")
        .unwrap();
    let ts_key = examples
        .iter()
        .find(|value| value.get("signaling").is_some())
        .unwrap();
    let ts_outputs = read_spec(&json!({"schema_version": 3, "outputs": [ts]})).unwrap();
    let ts_keys = read_keys(&json!({"schema_version": 2, "outputs": {"hls-ts": ts_key}})).unwrap();
    assert_drm_bindings(&ts_outputs, &ts_keys);
    assert_eq!(ts_outputs[0].target, TargetProtection::HlsSampleAes);
    assert_eq!(ts_outputs[0].drms, [DrmSystem::FairPlay]);
    let key = &ts_keys.outputs["hls-ts"];
    assert!(key.output.is_none());
    assert!(key.hls_output.as_ref().unwrap().associated_iv().is_some());
}

#[test]
fn malformed_public_recipe_settings_are_refused_by_the_real_reader() {
    let valid = public_json("outputs", "dash-cenc");
    for (label, pointer, replacement) in [
        ("schema", "/schema_version", json!(4)),
        ("path ID", "/outputs/0/id", json!("../dash")),
        (
            "unsupported target",
            "/outputs/0/target",
            json!("hls-sample-aes"),
        ),
        (
            "duplicate DRM",
            "/outputs/0/drms",
            json!(["widevine", "widevine"]),
        ),
        ("unsupported DRM", "/outputs/0/drms", json!(["fairplay"])),
    ] {
        let mut invalid = valid.clone();
        *invalid.pointer_mut(pointer).unwrap() = replacement;
        assert_eq!(
            read_spec(&invalid),
            Err(ConversionError("output-spec-invalid")),
            "{label}"
        );
    }
    let mut invalid = valid;
    invalid["outputs"][0]["unknown_setting"] = json!(true);
    assert_eq!(
        read_spec(&invalid),
        Err(ConversionError("output-spec-invalid"))
    );
}

#[test]
fn malformed_public_key_settings_are_refused_by_the_real_reader() {
    let valid = public_json("keys", "dash-cenc-from-cenc-cbcs");
    for (label, pointer, replacement) in [
        ("schema", "/schema_version", json!(1)),
        ("source KID", "/input/keys/0/kid", json!("invalid")),
        ("source key", "/input/keys/0/key", json!("00")),
        ("output key", "/outputs/dash/key", json!("invalid")),
    ] {
        let mut invalid = valid.clone();
        *invalid.pointer_mut(pointer).unwrap() = replacement;
        assert!(
            matches!(
                read_keys(&invalid),
                Err(ConversionError("key-file-invalid"))
            ),
            "{label}"
        );
    }
    let mut invalid = valid;
    invalid["outputs"]["dash"]["iv"] = json!("01");
    assert!(matches!(
        read_keys(&invalid),
        Err(ConversionError("key-file-invalid"))
    ));
    let mut hls = public_json("keys", "hls-ts-hls-sample-aes-from-hls-sample-aes");
    hls["input"]["hls_keys"][0]["uri"] = json!("");
    assert!(matches!(
        read_keys(&hls),
        Err(ConversionError("key-file-invalid"))
    ));
    let mut drm = documented_json()
        .into_iter()
        .find(|value| value["outputs"].is_object())
        .unwrap();
    drm["outputs"]["hls"]["kid"] = json!("101112131415161718191a1b1c1d1e1f");
    assert!(matches!(
        read_keys(&drm),
        Err(ConversionError("key-file-invalid"))
    ));
}
