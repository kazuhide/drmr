//! Saved processing choices apply to actual outputs and fresh independent verification.
#[path = "support/processing_acceptance.rs"]
mod acceptance;
mod common;

use acceptance::{
    assert_native, assert_original_segments, assert_processing, assert_verified, converter,
    fixture_mp4, native_converter, output, saved_segments, source, transport,
};
use drmr_core::{
    conversion::{
        ConversionRequest, DrmSystem, HlsInputKeys, InputKeys, ProcessingMode, ValidationDepth,
        conformance::ManifestValidationTools,
        key_file::{BundleOperatorKeys, OperatorKeys},
        plan::{
            self, SavedPlan,
            bundle::{BundlePlanRequest, BundleVerificationReport},
        },
        processing::BoundaryHandling,
    },
    inspection::{OutputFormat, TargetProtection},
};
use std::{fs, io::Write as _};

#[test]
fn segments_requires_a_segmented_source_before_any_external_tool_or_output() {
    let root = common::private_tempdir().unwrap();
    let input = root.path().join("source.mp4");
    let destination = root.path().join("output");
    fixture_mp4(&input);
    let original = fs::read(&input).unwrap();
    let (command, observed) = native_converter(ProcessingMode::Segments);
    let error = command
        .convert(ConversionRequest {
            input: &input,
            output: &destination,
            format: OutputFormat::Dash,
            target: TargetProtection::Clear,
            input_keys: &InputKeys::default(),
            hls_input_keys: &HlsInputKeys::default(),
            output_key: None,
            hls_key_uri: None,
            hls_output_key: None,
        })
        .unwrap_err();
    assert_eq!(error.0, "processing-segments-source-required");
    assert!(!destination.exists());
    assert_eq!(fs::read(&input).unwrap(), original);
    assert_native(&observed);
}

#[test]
fn forced_segments_refuses_a_container_change_without_tools_or_output() {
    let root = common::private_tempdir().unwrap();
    let input = transport(&root.path().join("source"));
    let original = common::snapshot_tree(input.parent().unwrap());
    let destination = root.path().join("output");
    let (command, observed) = native_converter(ProcessingMode::Segments);
    let error = command
        .convert(ConversionRequest {
            input: &input,
            output: &destination,
            format: OutputFormat::Dash,
            target: TargetProtection::Clear,
            input_keys: &InputKeys::default(),
            hls_input_keys: &HlsInputKeys::default(),
            output_key: None,
            hls_key_uri: None,
            hls_output_key: None,
        })
        .unwrap_err();
    assert_eq!(error.0, "processing-container-change-not-supported");
    assert!(!destination.exists());
    assert_eq!(common::snapshot_tree(input.parent().unwrap()), original);
    assert_native(&observed);
}

#[test]
fn saved_segments_with_omitted_format_retains_ts_and_executes_under_default_auto() {
    let root = common::private_tempdir().unwrap();
    let input = transport(&root.path().join("source"));
    let original = common::snapshot_tree(input.parent().unwrap());
    let destination = root.path().join("output");
    let keys = OperatorKeys::default();
    let (planner, plan_events) = native_converter(ProcessingMode::Segments);
    assert_eq!(
        planner.default_output_format(&input).unwrap(),
        OutputFormat::HlsTs
    );
    let saved = saved_segments(&planner, &input, &destination);
    assert_eq!(saved.schema_version, 9);
    assert_eq!(saved.source_format, OutputFormat::HlsTs);
    assert_eq!(saved.intent.format, OutputFormat::HlsTs);
    let choice = saved.processing.unwrap();
    assert_eq!(choice.requested, ProcessingMode::Segments);
    assert_eq!(choice.resolved, ProcessingMode::Segments);
    assert_eq!(choice.boundaries, BoundaryHandling::Preserve);
    assert_eq!(choice.segment_duration_ms, None);
    let path = root.path().join("conversion.plan.json");
    plan::write_new(&path, &saved).unwrap();
    let SavedPlan::Single(loaded) = plan::read_saved(&path).unwrap() else {
        panic!("schema-five plan must reopen as one output");
    };
    assert_eq!(saved, loaded);
    assert!(!destination.exists());
    let (command, observed) = native_converter(ProcessingMode::Auto);
    let converted = command.execute_plan(&loaded, &keys).unwrap();
    assert_processing(
        &converted,
        ProcessingMode::Segments,
        BoundaryHandling::Preserve,
    );
    assert_verified(&converted.verification, ValidationDepth::Native);
    assert_original_segments(&input, &destination.join("master.m3u8"), false);
    let verified = command.verify_plan(&loaded, &keys).unwrap();
    assert!(verified.source_unchanged && verified.output_matches_plan);
    assert_verified(&verified.verification, ValidationDepth::Native);
    assert_eq!(common::snapshot_tree(input.parent().unwrap()), original);
    assert_native(&plan_events);
    assert_native(&observed);
}

#[test]
fn saved_boundary_guarantees_cannot_be_edited_or_overridden_before_publication() {
    let root = common::private_tempdir().unwrap();
    let input = transport(&root.path().join("source"));
    let original = common::snapshot_tree(input.parent().unwrap());
    let destination = root.path().join("output");
    let keys = OperatorKeys::default();
    let (planner, _) = native_converter(ProcessingMode::Segments);
    let saved = saved_segments(&planner, &input, &destination);
    let (command, observed) = native_converter(ProcessingMode::Auto);
    let mut changed = saved.clone();
    changed.processing.as_mut().unwrap().boundaries = BoundaryHandling::Resegment;
    assert_eq!(
        command.execute_plan(&changed, &keys).unwrap_err().0,
        "execution-plan-invalid"
    );
    let path = root.path().join("edited.plan.json");
    drmr_core::native::create_private_new(&path)
        .unwrap()
        .write_all(&serde_json::to_vec(&changed).unwrap())
        .unwrap();
    assert_eq!(
        plan::read_saved(&path).unwrap_err().0,
        "execution-plan-invalid"
    );
    let (overriding, override_events) = native_converter(ProcessingMode::Stitch);
    assert_eq!(
        overriding.execute_plan(&saved, &keys).unwrap_err().0,
        "processing-plan-override-not-supported"
    );
    assert!(!destination.exists());
    assert_eq!(common::snapshot_tree(input.parent().unwrap()), original);
    assert_native(&observed);
    assert_native(&override_events);
}

#[test]
fn source_resource_mutation_is_refused_before_saved_segments_can_publish() {
    let root = common::private_tempdir().unwrap();
    let input = transport(&root.path().join("source"));
    let destination = root.path().join("output");
    let keys = OperatorKeys::default();
    let (planner, _) = native_converter(ProcessingMode::Segments);
    let saved = saved_segments(&planner, &input, &destination);
    let segment = acceptance::segments(&input)[0].path.clone();
    let original = fs::read(&segment).unwrap();
    let mut changed = original.clone();
    *changed.last_mut().unwrap() ^= 1;
    fs::write(&segment, &changed).unwrap();
    let (command, observed) = native_converter(ProcessingMode::Auto);
    assert_eq!(
        command.execute_plan(&saved, &keys).unwrap_err().0,
        "execution-plan-source-changed"
    );
    assert!(!destination.exists());
    assert_eq!(fs::read(&segment).unwrap(), changed);
    assert_native(&observed);
    fs::write(&segment, original).unwrap();
}

#[test]
fn fresh_saved_verification_refuses_changed_current_output() {
    let root = common::private_tempdir().unwrap();
    let input = transport(&root.path().join("source"));
    let original = common::snapshot_tree(input.parent().unwrap());
    let destination = root.path().join("output");
    let keys = OperatorKeys::default();
    let (planner, _) = native_converter(ProcessingMode::Segments);
    let saved = saved_segments(&planner, &input, &destination);
    let (command, observed) = native_converter(ProcessingMode::Auto);
    let converted = command.execute_plan(&saved, &keys).unwrap();
    assert_verified(&converted.verification, ValidationDepth::Native);
    let segment = acceptance::segments(&destination.join("master.m3u8"))[0]
        .path
        .clone();
    let mut bytes = fs::read(&segment).unwrap();
    bytes[0] ^= 1;
    fs::write(&segment, bytes).unwrap();
    let verified = command.verify_plan(&saved, &keys);
    assert!(match verified {
        Ok(report) => !report.verification.passed,
        Err(_) => true,
    });
    assert_eq!(common::snapshot_tree(input.parent().unwrap()), original);
    assert_native(&observed);
}

#[test]
fn native_clear_and_aes_bundle_reopens_and_preserves_every_original_segment() {
    let root = common::private_tempdir().unwrap();
    let input = transport(&root.path().join("source"));
    let original = common::snapshot_tree(input.parent().unwrap());
    let destination = root.path().join("bundle");
    let keys = acceptance::bundle_keys();
    let outputs = [
        output(
            "clear",
            OutputFormat::HlsTs,
            TargetProtection::Clear,
            Some(ProcessingMode::Segments),
        ),
        output(
            "aes",
            OutputFormat::HlsTs,
            TargetProtection::HlsAes128,
            Some(ProcessingMode::Segments),
        ),
    ];
    let (planner, plan_events) = native_converter(ProcessingMode::Auto);
    let saved = planner
        .create_bundle_plan(BundlePlanRequest {
            input: source(&input, &OperatorKeys::default()),
            output_root: &destination,
            outputs: &outputs,
        })
        .unwrap();
    assert_eq!(saved.schema_version, 10);
    assert!(
        saved
            .outputs
            .iter()
            .all(|intent| saved.processing_mode(&intent.id) == ProcessingMode::Segments)
    );
    let path = root.path().join("bundle.plan.json");
    plan::write_bundle(&path, &saved).unwrap();
    let SavedPlan::Bundle(loaded) = plan::read_saved(&path).unwrap() else {
        panic!("schema-six plan must reopen as a bundle");
    };
    assert_eq!(loaded, saved);
    assert!(!destination.exists());
    let tools = ManifestValidationTools::default();
    let (command, observed) = native_converter(ProcessingMode::Auto);
    let converted = command.execute_bundle_plan(&loaded, &keys, &tools).unwrap();
    assert_eq!(converted.outputs.len(), 2);
    for child in &converted.outputs {
        assert_processing(
            &child.conversion,
            ProcessingMode::Segments,
            BoundaryHandling::Preserve,
        );
        assert_verified(&child.conversion.verification, ValidationDepth::Native);
        assert_original_segments(
            &input,
            &destination.join(&child.id).join("master.m3u8"),
            child.id == "aes",
        );
    }
    let verified = command.verify_bundle_plan(&loaded, &keys, &tools).unwrap();
    assert_bundle_verified(&verified, ValidationDepth::Native);
    assert_eq!(common::snapshot_tree(input.parent().unwrap()), original);
    assert_native(&plan_events);
    assert_native(&observed);
}

#[test]
fn mixed_drm_plan_keeps_sibling_source_admission_and_saved_modes_separate() {
    let root = common::private_tempdir().unwrap();
    let input = transport(&root.path().join("source"));
    let original = common::snapshot_tree(input.parent().unwrap());
    let destination = root.path().join("bundle");
    let mut outputs = [
        output(
            "ts",
            OutputFormat::HlsTs,
            TargetProtection::HlsSampleAes,
            Some(ProcessingMode::Segments),
        ),
        output(
            "dash",
            OutputFormat::Dash,
            TargetProtection::Cbcs,
            Some(ProcessingMode::Stitch),
        ),
    ];
    outputs[0].drms = vec![DrmSystem::FairPlay];
    outputs[1].drms = vec![DrmSystem::PlayReady];
    let saved = converter(ProcessingMode::Auto)
        .create_bundle_plan(BundlePlanRequest {
            input: source(&input, &OperatorKeys::default()),
            output_root: &destination,
            outputs: &outputs,
        })
        .unwrap();
    assert_eq!(saved.schema_version, 10);
    assert_eq!(saved.processing_mode("ts"), ProcessingMode::Segments);
    assert_eq!(saved.processing_mode("dash"), ProcessingMode::Stitch);
    for choice in saved.processing.as_ref().unwrap() {
        let (requested, boundaries) = if choice.id == "ts" {
            (ProcessingMode::Segments, BoundaryHandling::Preserve)
        } else {
            assert_eq!(choice.id, "dash");
            (ProcessingMode::Stitch, BoundaryHandling::Resegment)
        };
        assert_eq!(choice.selection.requested, requested);
        assert_eq!(choice.selection.boundaries, boundaries);
    }
    assert!(!destination.exists());
    assert_eq!(common::snapshot_tree(input.parent().unwrap()), original);
}

#[test]
fn mixed_bundle_fresh_verification_uses_each_saved_mode_and_saved_decode_floor() {
    let root = common::private_tempdir().unwrap();
    let input = transport(&root.path().join("source"));
    let original = common::snapshot_tree(input.parent().unwrap());
    let destination = root.path().join("bundle");
    let keys = BundleOperatorKeys::default();
    let outputs = [
        output(
            "ts",
            OutputFormat::HlsTs,
            TargetProtection::Clear,
            Some(ProcessingMode::Segments),
        ),
        output("dash", OutputFormat::Dash, TargetProtection::Clear, None),
    ];
    let saved = converter(ProcessingMode::Auto)
        .with_decode(true)
        .create_bundle_plan(BundlePlanRequest {
            input: source(&input, &OperatorKeys::default()),
            output_root: &destination,
            outputs: &outputs,
        })
        .unwrap();
    assert_eq!(saved.schema_version, 10);
    assert_eq!(saved.minimum_depth(), ValidationDepth::Decoded);
    assert_eq!(saved.processing_mode("ts"), ProcessingMode::Segments);
    assert_eq!(saved.processing_mode("dash"), ProcessingMode::Stitch);
    let choices = saved.processing.as_ref().unwrap();
    let dash = choices
        .iter()
        .find(|choice| choice.id == "dash")
        .unwrap()
        .selection;
    assert_eq!(dash.requested, ProcessingMode::Auto);
    assert_eq!(dash.boundaries, BoundaryHandling::Resegment);
    let target: u32 = fs::read_to_string(&input)
        .unwrap()
        .lines()
        .find_map(|line| line.strip_prefix("#EXT-X-TARGETDURATION:"))
        .unwrap()
        .parse()
        .unwrap();
    assert_eq!(dash.segment_duration_ms, Some(target * 1000));
    let path = root.path().join("mixed.plan.json");
    plan::write_bundle(&path, &saved).unwrap();
    let SavedPlan::Bundle(loaded) = plan::read_saved(&path).unwrap() else {
        panic!("mixed plan must reopen as a bundle");
    };
    assert_eq!(saved, loaded);
    let command = converter(ProcessingMode::Auto).with_decode(false);
    let tools = ManifestValidationTools::default();
    let converted = command.execute_bundle_plan(&loaded, &keys, &tools).unwrap();
    assert_eq!(converted.outputs.len(), 2);
    for child in &converted.outputs {
        let (mode, boundaries) = if child.id == "ts" {
            (ProcessingMode::Segments, BoundaryHandling::Preserve)
        } else {
            assert_eq!(child.id, "dash");
            (ProcessingMode::Stitch, BoundaryHandling::Resegment)
        };
        assert_processing(&child.conversion, mode, boundaries);
        assert_verified(&child.conversion.verification, ValidationDepth::Decoded);
    }
    assert_original_segments(&input, &destination.join("ts/master.m3u8"), false);
    let verified = command.verify_bundle_plan(&loaded, &keys, &tools).unwrap();
    assert_bundle_verified(&verified, ValidationDepth::Decoded);
    assert_eq!(common::snapshot_tree(input.parent().unwrap()), original);
}

fn assert_bundle_verified(report: &BundleVerificationReport, depth: ValidationDepth) {
    assert!(report.passed && report.source_unchanged);
    assert_eq!(report.outputs.len(), 2);
    for child in &report.outputs {
        assert!(child.output_matches_plan);
        assert_verified(&child.verification, depth);
    }
}
