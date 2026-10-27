//! New output intent is strict and identity-bound without changing older plan bytes.
use super::*;
use crate::conversion::{
    layout::{LayoutMode, SourceLayout, SourceTrack},
    output_options::OutputOptions,
};
use crate::inspection::TrackKind;

pub(super) fn options(source: &SourceInventory) -> OutputOptions {
    selected_options(source, false)
}

pub(super) fn current_options(source: &SourceInventory) -> OutputOptions {
    selected_options(source, true)
}

fn selected_options(source: &SourceInventory, current: bool) -> OutputOptions {
    let tracks = [SourceTrack {
        source: source.entrypoint.file_name().unwrap().into(),
        selector: 0,
        kind: TrackKind::Video,
        playlist: None,
        media: vec![source.entrypoint.file_name().unwrap().into()],
    }];
    let layout = if current {
        SourceLayout::resolve_for_processing(
            LayoutMode::Source,
            OutputFormat::Dash,
            ProcessingMode::Stitch,
            &tracks,
        )
    } else {
        SourceLayout::resolve_legacy_for_processing(
            LayoutMode::Source,
            OutputFormat::Dash,
            ProcessingMode::Stitch,
            &tracks,
        )
    }
    .unwrap();
    OutputOptions {
        layout: Some(layout),
        dash_addressing: Some(crate::conversion::dash_addressing::DashAddressing::TemplateDuration),
    }
}

#[test]
fn filename_intent_is_required_only_by_version_nine_and_bound_to_its_identity() {
    let root = crate::native::private_tempdir().unwrap();
    for schema in [7, 9] {
        for current in [false, true] {
            let mut plan: ExecutionPlan = serde_json::from_value(value(root.path())).unwrap();
            plan.schema_version = schema;
            plan.output_options = Some(selected_options(&plan.source, current));
            plan.plan_id = identity(&plan).unwrap();
            if current != (schema == 9) {
                assert_eq!(validate(&plan).unwrap_err(), INVALID);
                continue;
            }
            validate(&plan).unwrap();
            let bytes = serde_json::to_vec(&plan).unwrap();
            assert_eq!(
                bytes
                    .windows(b"media_names".len())
                    .any(|w| w == b"media_names"),
                current
            );
            let reopened: ExecutionPlan = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(serde_json::to_vec(&reopened).unwrap(), bytes);
            let file = root.path().join(format!("schema-{schema}.json"));
            write_new(&file, &plan).unwrap();
            assert_eq!(read_saved(&file).unwrap(), SavedPlan::Single(plan));
        }
    }
}

#[test]
fn version_nine_keeps_required_fields_strict_and_checks_filename_edits() {
    let root = crate::native::private_tempdir().unwrap();
    let mut plan: ExecutionPlan = serde_json::from_value(value(root.path())).unwrap();
    plan.schema_version = 9;
    plan.output_options = Some(current_options(&plan.source));
    plan.plan_id = identity(&plan).unwrap();
    validate(&plan).unwrap();
    for field in ["required_depth", "processing", "output_options"] {
        for replacement in [None, Some(serde_json::Value::Null)] {
            let mut edited = serde_json::to_value(&plan).unwrap();
            if let Some(value) = replacement {
                edited[field] = value;
            } else {
                edited.as_object_mut().unwrap().remove(field);
            }
            let parsed = serde_json::from_value::<ExecutionPlan>(edited);
            assert!(parsed.is_err() || validate(&parsed.unwrap()).is_err());
        }
    }
    let mut edited = serde_json::to_value(&plan).unwrap();
    edited["output_options"]["layout"]["tracks"][0]["media_names"]["initialization"] =
        serde_json::json!("different.mp4");
    let edited: ExecutionPlan = serde_json::from_value(edited).unwrap();
    assert_eq!(validate(&edited).unwrap_err(), INVALID);
    assert!(!plan.intent.output.exists());
}

fn value(root: &Path) -> serde_json::Value {
    let plan = tests::sample(root);
    let mut value = serde_json::to_value(&plan).unwrap();
    value["schema_version"] = serde_json::json!(7);
    value["required_depth"] = serde_json::json!("native");
    value["processing"] = serde_json::json!({"requested":"auto","resolved":"stitch","boundaries":"resegment","segment_duration_ms":2000});
    value["output_options"] = serde_json::to_value(options(&plan.source)).unwrap();
    value
}

#[test]
fn version_seven_requires_identity_bound_output_options() {
    let root = crate::native::private_tempdir().unwrap();
    let parsed = serde_json::from_value::<ExecutionPlan>(value(root.path()));
    assert!(parsed.is_ok(), "version-seven output options must parse");
    let mut plan = parsed.unwrap();
    plan.plan_id = identity(&plan).unwrap();
    validate(&plan).unwrap();
    let file = root.path().join("source-layout.plan.json");
    write_new(&file, &plan).unwrap();
    assert_eq!(read_saved(&file).unwrap(), SavedPlan::Single(plan.clone()));
    let mut edited = serde_json::to_value(&plan).unwrap();
    edited["output_options"]["dash_addressing"] = serde_json::json!("list");
    assert_eq!(
        validate(&serde_json::from_value(edited).unwrap()).unwrap_err(),
        INVALID
    );
    for replacement in [
        serde_json::Value::Null,
        serde_json::json!({"dash_addressing":null}),
        serde_json::json!({"unknown":"private"}),
    ] {
        let mut invalid = serde_json::to_value(&plan).unwrap();
        invalid["output_options"] = replacement;
        let parsed = serde_json::from_value::<ExecutionPlan>(invalid);
        assert!(parsed.is_err() || validate(&parsed.unwrap()).is_err());
    }
    let mut missing = serde_json::to_value(&plan).unwrap();
    missing.as_object_mut().unwrap().remove("output_options");
    let parsed = serde_json::from_value::<ExecutionPlan>(missing);
    assert!(parsed.is_err() || validate(&parsed.unwrap()).is_err());
}

#[test]
fn old_single_schemas_omit_new_fields_and_keep_their_exact_identity() {
    let root = crate::native::private_tempdir().unwrap();
    let mut plan = tests::sample(root.path());
    for version in [1, 3, 5] {
        plan.schema_version = version;
        plan.required_depth = (version >= 3).then_some(ValidationDepth::Native);
        plan.processing = (version == 5).then(|| {
            ProcessingSelection::resolve(
                ProcessingMode::Stitch,
                OutputFormat::Mp4,
                OutputFormat::Dash,
            )
            .unwrap()
        });
        plan.plan_id = identity(&plan).unwrap();
        let bytes = serde_json::to_vec(&plan).unwrap();
        let old: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert!(old.get("output_options").is_none());
        let reopened: ExecutionPlan = serde_json::from_slice(&bytes).unwrap();
        validate(&reopened).unwrap();
        assert_eq!(serde_json::to_vec(&reopened).unwrap(), bytes);
        let mut changed = old;
        changed["output_options"] = serde_json::to_value(options(&plan.source)).unwrap();
        let parsed = serde_json::from_value::<ExecutionPlan>(changed);
        assert!(parsed.is_err() || validate(&parsed.unwrap()).is_err());
    }
}

#[test]
fn source_selectors_must_belong_to_the_saved_inventory_before_any_media_work() {
    let root = crate::native::private_tempdir().unwrap();
    let parsed = serde_json::from_value::<ExecutionPlan>(value(root.path()));
    assert!(
        parsed.is_ok(),
        "new plan schema must parse before checking membership"
    );
    let mut plan = parsed.unwrap();
    let mut changed = serde_json::to_value(&plan).unwrap();
    changed["output_options"]["layout"]["tracks"][0]["source"] = serde_json::json!("unlisted.mp4");
    plan = serde_json::from_value(changed).unwrap();
    plan.plan_id = identity(&plan).unwrap();
    assert_eq!(validate(&plan).unwrap_err(), INVALID);
    assert!(!plan.intent.output.exists());
}

#[test]
fn nested_inventory_selectors_are_relative_to_the_entrypoint_parent() {
    let root = crate::native::private_tempdir().unwrap();
    let mut inventory = tests::sample(root.path()).source;
    inventory.entrypoint = "nested/source.mp4".into();
    inventory.files[0].path = inventory.entrypoint.clone();
    let mut selected = options(&inventory);
    validate_options(
        &selected,
        &inventory,
        OutputFormat::Mp4,
        OutputFormat::Dash,
        ProcessingMode::Stitch,
    )
    .unwrap();
    selected.layout.as_mut().unwrap().tracks[0].source = inventory.entrypoint.clone();
    assert_eq!(
        validate_options(
            &selected,
            &inventory,
            OutputFormat::Mp4,
            OutputFormat::Dash,
            ProcessingMode::Stitch,
        )
        .unwrap_err(),
        INVALID
    );
}

pub(super) fn native_input(root: &Path) -> PathBuf {
    let input = root.join("source.mp4");
    std::fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../drmr/tests/fixtures/bbb/clip.mp4"),
        &input,
    )
    .unwrap();
    input
}

pub(super) fn native_command() -> (Converter, Arc<crate::events::Collector>) {
    let events = Arc::new(crate::events::Collector::default());
    let runner = Arc::new(crate::toolrun::ToolRunner::new(
        Arc::new(crate::toolrun::Secrets::new()),
        events.clone(),
    ));
    (
        Converter::new(runner, "must-not-run-packager", "must-not-run-decoder")
            .with_new_output_defaults(),
        events,
    )
}

pub(super) fn assert_no_tools(events: &crate::events::Collector) {
    assert!(events.records().iter().all(|record| {
        record.get("event").and_then(serde_json::Value::as_str) != Some("command")
    }));
}

fn native_plan(command: &Converter, input: &Path, output: &Path) -> ExecutionPlan {
    command
        .create_execution_plan(ExecutionPlanRequest {
            input: VerificationSource {
                input,
                input_keys: &super::super::InputKeys::default(),
                hls_keys: &super::super::HlsInputKeys::default(),
            },
            output,
            format: Some(OutputFormat::Dash),
            target: TargetProtection::Clear,
        })
        .unwrap()
}

#[test]
fn native_source_rejects_a_rehashed_output_map_before_packaging() {
    let root = crate::native::private_tempdir().unwrap();
    let input = native_input(root.path());
    let output = root.path().join("result");
    let (command, events) = native_command();
    let mut plan = native_plan(&command, &input, &output);
    assert_eq!(plan.schema_version, 9);
    assert_eq!(plan.minimum_depth(), ValidationDepth::Native);
    let map = plan
        .output_options
        .as_mut()
        .unwrap()
        .layout
        .as_mut()
        .unwrap();
    for track in &mut map.tracks {
        let playlist = track.playlist.file_name().unwrap().to_os_string();
        track.media_directory = "different-directory".into();
        track.playlist = track.media_directory.join(playlist);
    }
    plan.plan_id = identity(&plan).unwrap();
    validate(&plan).unwrap();
    assert_eq!(
        command
            .execute_plan(&plan, &OperatorKeys::default())
            .unwrap_err()
            .0,
        "execution-plan-output-options-changed"
    );
    assert_no_tools(&events);
    assert!(!output.exists());
    assert!(input.exists());
}

#[test]
fn native_plan_reopens_saved_choices_independently_of_current_defaults() {
    let root = crate::native::private_tempdir().unwrap();
    let input = native_input(root.path());
    let output = root.path().join("result");
    let (command, events) = native_command();
    let saved = native_plan(&command, &input, &output);
    let frozen = command.reopen_source(&saved.source).unwrap();
    let input = frozen.entrypoint();
    let command = command.using_frozen_source(&frozen);
    let qualified = command
        .validate_source(
            VerificationSource {
                input,
                input_keys: &super::super::InputKeys::default(),
                hls_keys: &super::super::HlsInputKeys::default(),
            },
            SourceValidationPurpose::Conversion,
        )
        .unwrap();
    let current = command
        .with_layout(LayoutMode::Normalized)
        .with_dash_addressing(crate::conversion::dash_addressing::DashAddressing::Base);
    let restored = current.using_output_options(saved.output_options.as_ref());
    revalidate_options(
        &restored,
        input,
        &qualified,
        saved.intent.format,
        saved.processing_mode(),
        saved.output_options.as_ref(),
    )
    .unwrap();
    assert_no_tools(&events);
    assert!(!output.exists());
}

#[test]
fn native_plan_refuses_changed_source_bytes_before_external_commands() {
    let root = crate::native::private_tempdir().unwrap();
    let input = native_input(root.path());
    let output = root.path().join("result");
    let (command, events) = native_command();
    let plan = native_plan(&command, &input, &output);
    let mut bytes = std::fs::read(&input).unwrap();
    let last = bytes.len() - 1;
    bytes[last] ^= 1;
    std::fs::write(&input, bytes).unwrap();
    assert_eq!(
        command
            .execute_plan(&plan, &OperatorKeys::default())
            .unwrap_err(),
        CHANGED
    );
    assert_no_tools(&events);
    assert!(!output.exists());
}
