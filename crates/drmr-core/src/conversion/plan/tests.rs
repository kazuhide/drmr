use super::*;

fn sample(root: &Path) -> ExecutionPlan {
    let input = root.join("source.mp4");
    std::fs::write(&input, b"\0\0\0\x18ftypisom original data").unwrap();
    let frozen = FrozenSource::capture(&input, &crate::concurrency::Stop::new()).unwrap();
    let mut plan = ExecutionPlan {
        kind: KIND.into(),
        schema_version: 1,
        required_depth: None,
        plan_id: String::new(),
        source: frozen.inventory().clone(),
        source_format: OutputFormat::Mp4,
        intent: OutputIntent {
            format: OutputFormat::Dash,
            target: TargetProtection::Cbcs,
            output: root.canonicalize().unwrap().join("result"),
        },
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
fn required_depth_is_bound_in_new_plans_and_legacy_fields_stay_strict() {
    let root = crate::native::private_tempdir().unwrap();
    let mut plan = sample(root.path());
    assert_eq!(plan.minimum_depth(), ValidationDepth::Decoded);
    let legacy = serde_json::to_value(&plan).unwrap();
    assert!(legacy.get("required_depth").is_none());
    for value in [serde_json::Value::Null, serde_json::json!("native")] {
        let mut invalid = legacy.clone();
        invalid["required_depth"] = value;
        let parsed = serde_json::from_value::<ExecutionPlan>(invalid);
        assert!(parsed.is_err() || validate(&parsed.unwrap()).is_err());
    }
    plan.schema_version = 3;
    plan.required_depth = Some(ValidationDepth::Native);
    plan.plan_id = identity(&plan).unwrap();
    validate(&plan).unwrap();
    let native_id = plan.plan_id.clone();
    plan.required_depth = Some(ValidationDepth::Decoded);
    assert_eq!(validate(&plan).unwrap_err(), INVALID);
    plan.plan_id = identity(&plan).unwrap();
    assert_ne!(plan.plan_id, native_id);
    validate(&plan).unwrap();
    assert_eq!(
        ValidationDepth::Native.max(plan.minimum_depth()),
        ValidationDepth::Decoded
    );
}

#[test]
fn schema_rejects_unknown_duplicate_and_legacy_fields_before_media_work() {
    let root = crate::native::private_tempdir().unwrap();
    let plan = sample(root.path());
    let original = serde_json::to_value(&plan).unwrap();
    for (index, field) in ["command", "tool", "headers", "key", "measurements"]
        .iter()
        .enumerate()
    {
        let mut value = original.clone();
        value[*field] = "confidential-sentinel".into();
        let path = root.path().join(format!("unknown-{index}.json"));
        private_write(&path, &serde_json::to_vec(&value).unwrap());
        assert_eq!(read(&path).unwrap_err(), INVALID);
    }
    let mut value = original;
    value["intent"]["command"] = "confidential-sentinel".into();
    let path = root.path().join("nested.json");
    private_write(&path, &serde_json::to_vec(&value).unwrap());
    assert_eq!(read(&path).unwrap_err(), INVALID);
    let encoded = serde_json::to_string(&plan).unwrap();
    let duplicate = encoded.replacen(
        "\"schema_version\":1",
        "\"schema_version\":1,\"schema_version\":1",
        1,
    );
    let path = root.path().join("duplicate.json");
    private_write(&path, duplicate.as_bytes());
    assert_eq!(read(&path).unwrap_err(), INVALID);
    assert!(!plan.intent.output.exists());
}

#[test]
fn source_output_overlap_and_output_path_edits_are_refused() {
    let root = crate::native::private_tempdir().unwrap();
    let mut plan = sample(root.path());
    plan.intent.output = plan.source.root.join(&plan.source.entrypoint);
    plan.plan_id = identity(&plan).unwrap();
    assert_eq!(validate(&plan).unwrap_err(), UNSAFE);
    plan.intent.output = plan.source.root.join("nested-output");
    plan.source_format = OutputFormat::HlsTs;
    plan.plan_id = identity(&plan).unwrap();
    assert_eq!(validate(&plan).unwrap_err(), UNSAFE);
    plan.source_format = OutputFormat::Mp4;
    plan.intent.output = PathBuf::from("relative-result");
    plan.plan_id = identity(&plan).unwrap();
    assert_eq!(validate(&plan).unwrap_err(), INVALID);
    // Joining onto a Windows verbatim path would normalize away the parent component.
    let mut escaped = root.path().canonicalize().unwrap().into_os_string();
    escaped.push(std::path::MAIN_SEPARATOR_STR);
    escaped.push("..");
    escaped.push(std::path::MAIN_SEPARATOR_STR);
    escaped.push("escaped");
    plan.intent.output = PathBuf::from(escaped);
    assert!(plan.intent.output.is_absolute());
    assert!(
        plan.intent
            .output
            .components()
            .any(|part| matches!(part, Component::ParentDir))
    );
    plan.plan_id = identity(&plan).unwrap();
    assert_eq!(validate(&plan).unwrap_err(), INVALID);
}

#[test]
fn private_plan_save_does_not_modify_original_or_grant_cleanup_authority() {
    let root = crate::native::private_tempdir().unwrap();
    let plan = sample(root.path());
    let path = root.path().join("conversion.plan.json");
    let original = std::fs::read(plan.source.root.join(&plan.source.entrypoint)).unwrap();
    write_new(&path, &plan).unwrap();
    assert_eq!(read(&path).unwrap(), plan);
    assert_eq!(write_new(&path, &plan).unwrap_err(), EXISTS);
    assert_eq!(
        std::fs::read(plan.source.root.join(&plan.source.entrypoint)).unwrap(),
        original
    );
    assert!(!plan.intent.output.exists());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert_eq!(read(&path).unwrap_err(), INVALID);
    }
}

#[test]
fn unsupported_versions_and_oversize_files_are_not_executable() {
    let root = crate::native::private_tempdir().unwrap();
    let mut plan = sample(root.path());
    plan.schema_version = 2;
    plan.plan_id = identity(&plan).unwrap();
    assert_eq!(validate(&plan).unwrap_err(), INVALID);
    let path = root.path().join("oversize.json");
    crate::native::create_private_new(&path)
        .unwrap()
        .set_len(LIMIT + 1)
        .unwrap();
    assert_eq!(read(&path).unwrap_err(), INVALID);
    assert!(!plan.intent.output.exists());
}
