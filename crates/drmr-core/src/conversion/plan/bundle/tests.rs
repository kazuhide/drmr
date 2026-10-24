use super::*;
use crate::conversion::plan::{self, SavedPlan};

fn intent(id: &str) -> BundleOutputIntent {
    BundleOutputIntent {
        processing: None,
        id: id.into(),
        format: OutputFormat::Dash,
        target: TargetProtection::Clear,
        drms: Vec::new(),
        conformance: ManifestValidation::None,
    }
}

fn sample(root: &Path) -> BundlePlan {
    let input = root.join("source.mp4");
    std::fs::write(&input, b"\0\0\0\x18ftypisom original data").unwrap();
    let frozen = FrozenSource::capture(&input, &crate::concurrency::Stop::new()).unwrap();
    let mut plan = BundlePlan {
        processing: None,
        kind: KIND.into(),
        schema_version: 2,
        required_depth: None,
        plan_id: String::new(),
        source: frozen.inventory().clone(),
        source_format: OutputFormat::Mp4,
        output_root: root.canonicalize().unwrap().join("result"),
        outputs: vec![intent("dash"), intent("alternate")],
    };
    plan.plan_id = identity(&plan).unwrap();
    plan
}

fn private_write(path: &Path, bytes: &[u8]) {
    crate::native::create_private_new(path)
        .unwrap()
        .write_all(bytes)
        .unwrap();
}

#[test]
fn version_six_binds_one_resolved_processing_choice_per_output() {
    let root = crate::native::private_tempdir().unwrap();
    let mut value = serde_json::to_value(sample(root.path())).unwrap();
    value["schema_version"] = serde_json::json!(6);
    value["required_depth"] = serde_json::json!("native");
    value["processing"] = serde_json::json!([
        {"id":"dash", "selection":{"requested":"auto", "resolved":"stitch", "boundaries":"resegment", "segment_duration_ms":2000}},
        {"id":"alternate", "selection":{"requested":"stitch", "resolved":"stitch", "boundaries":"resegment", "segment_duration_ms":2000}}
    ]);
    let parsed = serde_json::from_value::<BundlePlan>(value);
    assert!(
        parsed.is_ok(),
        "version-six processing must be a supported schema"
    );
    let mut plan = parsed.unwrap();
    plan.plan_id = identity(&plan).unwrap();
    validate(&plan).unwrap();
    let saved = root.path().join("processing.bundle.json");
    plan::write_bundle(&saved, &plan).unwrap();
    assert_eq!(
        plan::read_saved(&saved).unwrap(),
        SavedPlan::Bundle(plan.clone())
    );
    let mut missing = serde_json::to_value(&plan).unwrap();
    missing["processing"].as_array_mut().unwrap().pop();
    assert!(validate(&serde_json::from_value(missing).unwrap()).is_err());
    let mut changed = serde_json::to_value(&plan).unwrap();
    changed["processing"][0]["selection"]["requested"] = "stitch".into();
    assert!(validate(&serde_json::from_value(changed).unwrap()).is_err());
    plan.schema_version = 4;
    plan.plan_id = identity(&plan).unwrap();
    assert_eq!(validate(&plan).unwrap_err(), INVALID);
}

#[test]
fn output_spec_version_two_accepts_per_output_processing_without_changing_version_one() {
    let root = crate::native::private_tempdir().unwrap();
    let body = |version| {
        serde_json::json!({"schema_version":version,"outputs":[{
            "id":"dash", "format":"dash", "target":"clear", "drms":[], "processing":"stitch"
        }]})
    };
    let new = root.path().join("outputs-v2.json");
    std::fs::write(&new, serde_json::to_vec(&body(2)).unwrap()).unwrap();
    assert!(read_output_spec(&new).is_ok());
    let old = root.path().join("outputs-v1.json");
    std::fs::write(&old, serde_json::to_vec(&body(1)).unwrap()).unwrap();
    assert_eq!(read_output_spec(&old).unwrap_err().0, "output-spec-invalid");
}

#[test]
fn bundle_minimum_depth_is_bound_without_changing_legacy_identity() {
    let root = crate::native::private_tempdir().unwrap();
    let mut plan = sample(root.path());
    assert_eq!(plan.minimum_depth(), ValidationDepth::Decoded);
    let legacy_id = plan.plan_id.clone();
    assert!(
        serde_json::to_value(&plan)
            .unwrap()
            .get("required_depth")
            .is_none()
    );
    plan.schema_version = 4;
    plan.required_depth = Some(ValidationDepth::Native);
    plan.plan_id = identity(&plan).unwrap();
    assert_ne!(plan.plan_id, legacy_id);
    validate(&plan).unwrap();
    let saved = root.path().join("native.plan.json");
    plan::write_bundle(&saved, &plan).unwrap();
    assert_eq!(
        plan::read_saved(&saved).unwrap(),
        SavedPlan::Bundle(plan.clone())
    );
    plan.required_depth = Some(ValidationDepth::Decoded);
    assert_eq!(validate(&plan).unwrap_err(), INVALID);
    plan.plan_id = identity(&plan).unwrap();
    validate(&plan).unwrap();
}

#[test]
fn strict_saved_dispatch_accepts_both_schemas_and_preserves_private_no_replace() {
    let root = crate::native::private_tempdir().unwrap();
    let plan = sample(root.path());
    let path = root.path().join("bundle.plan.json");
    plan::write_bundle(&path, &plan).unwrap();
    assert_eq!(
        plan::read_saved(&path).unwrap(),
        SavedPlan::Bundle(plan.clone())
    );
    let original = std::fs::read(&path).unwrap();
    assert_eq!(plan::write_bundle(&path, &plan).unwrap_err(), EXISTS);
    assert_eq!(std::fs::read(&path).unwrap(), original);
    assert!(plan::read(&path).is_err());
    let mut legacy = plan::ExecutionPlan {
        processing: None,
        kind: KIND.into(),
        schema_version: 1,
        required_depth: None,
        plan_id: String::new(),
        source: plan.source.clone(),
        source_format: OutputFormat::Mp4,
        intent: plan::OutputIntent {
            format: OutputFormat::Dash,
            target: TargetProtection::Clear,
            output: root.path().canonicalize().unwrap().join("legacy-result"),
        },
    };
    legacy.plan_id = super::super::identity(&legacy).unwrap();
    let path = root.path().join("legacy.plan.json");
    plan::write_new(&path, &legacy).unwrap();
    assert_eq!(plan::read_saved(&path).unwrap(), SavedPlan::Single(legacy));
    assert!(!plan.output_root.exists());
    assert!(!format!("{plan:?}").contains(root.path().to_str().unwrap()));
}

#[test]
fn saved_dispatch_rejects_unknown_nested_and_duplicate_fields() {
    let root = crate::native::private_tempdir().unwrap();
    let plan = sample(root.path());
    let original = serde_json::to_value(&plan).unwrap();
    for (index, field) in ["command", "headers", "key", "intent", "measurements"]
        .iter()
        .enumerate()
    {
        let mut value = original.clone();
        value[*field] = "confidential-sentinel".into();
        let path = root.path().join(format!("unknown-{index}.json"));
        private_write(&path, &serde_json::to_vec(&value).unwrap());
        assert_eq!(plan::read_saved(&path).unwrap_err(), INVALID);
    }
    let mut nested = original;
    nested["outputs"][0]["output"] = "arbitrary-child".into();
    let path = root.path().join("nested.json");
    private_write(&path, &serde_json::to_vec(&nested).unwrap());
    assert_eq!(plan::read_saved(&path).unwrap_err(), INVALID);
    let text = serde_json::to_string(&plan).unwrap();
    for (index, duplicate) in [
        text.replacen(
            "\"schema_version\":2",
            "\"schema_version\":2,\"schema_version\":1",
            1,
        ),
        text.replacen("\"id\":\"dash\"", "\"id\":\"dash\",\"id\":\"other\"", 1),
        text.replacen(
            "\"conformance\":\"none\"",
            "\"conformance\":\"none\",\"conformance\":\"required\"",
            1,
        ),
    ]
    .iter()
    .enumerate()
    {
        let path = root.path().join(format!("duplicate-{index}.json"));
        private_write(&path, duplicate.as_bytes());
        assert_eq!(plan::read_saved(&path).unwrap_err(), INVALID);
    }
}

#[test]
fn every_output_condition_and_order_is_pinned_by_the_plan_identity() {
    let root = crate::native::private_tempdir().unwrap();
    let plan = sample(root.path());
    for change in 0..7 {
        let mut modified = plan.clone();
        match change {
            0 => modified.outputs[0].id = "different".into(),
            1 => modified.outputs[0].format = OutputFormat::Hls,
            2 => modified.outputs[0].target = TargetProtection::Cbcs,
            3 => {
                modified.outputs[0].target = TargetProtection::Cbcs;
                modified.outputs[0].drms.push(DrmSystem::Widevine);
            }
            4 => modified.outputs[0].conformance = ManifestValidation::Required,
            5 => modified.outputs.reverse(),
            _ => modified.output_root = root.path().canonicalize().unwrap().join("different"),
        }
        assert_eq!(validate(&modified).unwrap_err(), INVALID, "change {change}");
    }
}

#[test]
fn ids_count_profiles_and_drm_duplicates_fail_before_media_processing() {
    let root = crate::native::private_tempdir().unwrap();
    let original = sample(root.path());
    for id in [
        "",
        "../escape",
        "a/b",
        "a\\b",
        "A",
        "1name",
        "con",
        "lpt9",
        "ümlaut",
        "has space",
        "trailing.",
        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
    ] {
        let mut plan = original.clone();
        plan.outputs[0].id = id.into();
        plan.plan_id = identity(&plan).unwrap();
        assert_eq!(validate(&plan).unwrap_err(), INVALID, "id {id:?}");
    }
    for outputs in [
        vec![],
        vec![intent("same"), intent("same")],
        (0..9).map(|n| intent(&format!("output-{n}"))).collect(),
    ] {
        let mut plan = original.clone();
        plan.outputs = outputs;
        plan.plan_id = identity(&plan).unwrap();
        assert_eq!(validate(&plan).unwrap_err(), INVALID);
    }
    let mut output = intent("dash");
    output.target = TargetProtection::Cbcs;
    output.drms = vec![DrmSystem::Widevine, DrmSystem::Widevine];
    assert!(normalized(&[output.clone()]).is_err());
    output.drms = vec![DrmSystem::FairPlay];
    assert!(normalized(&[output.clone()]).is_err());
    output.format = OutputFormat::Hls;
    assert!(normalized(&[output.clone()]).is_ok());
    output.format = OutputFormat::HlsTs;
    assert!(normalized(&[output.clone()]).is_err());
    output.target = TargetProtection::HlsSampleAes;
    assert!(normalized(&[output]).is_ok());
}

#[test]
fn output_and_plan_paths_cannot_overlap_the_retained_graph() {
    let root = crate::native::private_tempdir().unwrap();
    let mut plan = sample(root.path());
    plan.output_root = plan.source.root.join(&plan.source.entrypoint);
    plan.plan_id = identity(&plan).unwrap();
    assert_eq!(validate(&plan).unwrap_err(), UNSAFE);
    plan.output_root = plan.source.root.join("nested");
    plan.source_format = OutputFormat::HlsTs;
    plan.plan_id = identity(&plan).unwrap();
    assert_eq!(validate(&plan).unwrap_err(), UNSAFE);
    plan.source_format = OutputFormat::Mp4;
    plan.output_root = PathBuf::from("relative");
    plan.plan_id = identity(&plan).unwrap();
    assert_eq!(validate(&plan).unwrap_err(), INVALID);
    let mut plan = sample(root.path());
    std::fs::create_dir(&plan.output_root).unwrap();
    assert_eq!(
        write_new(&plan.output_root.join("plan.json"), &plan).unwrap_err(),
        UNSAFE
    );
    assert!(!plan.output_root.join("plan.json").exists());
    plan.schema_version = 3;
    plan.plan_id = identity(&plan).unwrap();
    assert_eq!(validate(&plan).unwrap_err(), INVALID);
}

#[test]
fn public_output_recipe_is_bounded_strict_and_does_not_require_secret_permissions() {
    let root = crate::native::private_tempdir().unwrap();
    let path = root.path().join("outputs.json");
    std::fs::write(&path, r#"{"schema_version":1,"outputs":[{"id":"dash","format":"dash","target":"clear","drms":[]}]}"#).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
    }
    let outputs = read_output_spec(&path).unwrap();
    assert_eq!(outputs, vec![intent("dash")]);
    for bytes in [
        r#"{"schema_version":1,"schema_version":1,"outputs":[]}"#,
        r#"{"schema_version":1,"outputs":[],"command":"untrusted"}"#,
        r#"{"schema_version":1,"outputs":[{"id":"dash","id":"other","format":"dash","target":"clear","drms":[]}] }"#,
        r#"{"schema_version":1,"outputs":[{"id":"dash","format":"dash","target":"clear","drms":["unknown"]}] }"#,
        r#"{"schema_version":2,"outputs":[]}"#,
    ] {
        std::fs::write(&path, bytes).unwrap();
        assert_eq!(
            read_output_spec(&path).unwrap_err().0,
            "output-spec-invalid"
        );
    }
    std::fs::write(&path, vec![b' '; 65_537]).unwrap();
    assert_eq!(
        read_output_spec(&path).unwrap_err().0,
        "output-spec-invalid"
    );
}

#[test]
fn per_output_keys_must_match_exactly_the_encrypted_output_ids_and_drm_set() {
    let root = crate::native::private_tempdir().unwrap();
    let mut plan = sample(root.path());
    let mut keys = BundleOperatorKeys::default();
    keys.outputs.insert("dash".into(), OperatorKeys::default());
    assert_eq!(validate_keys(&plan, &keys).unwrap_err(), KEYS);
    keys.outputs.clear();
    plan.outputs[0].target = TargetProtection::Cbcs;
    assert_eq!(validate_keys(&plan, &keys).unwrap_err(), KEYS);
    keys.outputs.insert("dash".into(), OperatorKeys::default());
    plan.outputs[0].drms.push(DrmSystem::PlayReady);
    assert_eq!(validate_keys(&plan, &keys).unwrap_err(), DRM);
}
