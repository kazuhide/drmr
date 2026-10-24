use super::*;

fn selection(mode: &str, boundaries: &str) -> Value {
    json!({"requested":mode,"resolved":mode,"boundaries":boundaries,
        "segment_duration_ms":if boundaries=="resegment" {json!(2000)} else {Value::Null}})
}

#[test]
fn resolved_actual_modes_and_legacy_absence_are_projected_without_backend_text() {
    use drmr_core::inspection::{InputPreparation, ProcessingRouteId, RequiredTool};
    let verification = json!({"schema_version":2,"required_depth":"native"});
    let input = json!({"format":"hls","protection":"clear"});
    for route in [
        ProcessingRouteId::Fmp4Segments,
        ProcessingRouteId::MpegTsSegments,
    ] {
        let actual = ActualProcessing::new(route, InputPreparation::OriginalSegments);
        let value = serde_json::to_value(&actual).unwrap();
        assert_eq!(processing(&value, &verification, &input).unwrap(), value);
        let mut legacy = value.clone();
        legacy.as_object_mut().unwrap().remove("mode");
        legacy.as_object_mut().unwrap().remove("boundaries");
        assert_eq!(processing(&legacy, &verification, &input).unwrap(), legacy);
        for (field, malformed) in [
            ("mode", json!("PRIVATE_MODE")),
            ("mode", json!("auto")),
            ("mode", Value::Null),
            ("boundaries", json!("resegment")),
            ("boundaries", json!("PRIVATE_BOUNDARY")),
        ] {
            let mut changed = value.clone();
            changed[field] = malformed;
            assert!(processing(&changed, &verification, &input).is_err());
        }
        let mut changed = value;
        changed["input_preparation"] = json!("complete-file");
        assert!(processing(&changed, &verification, &input).is_err());
        changed["input_preparation"] = json!("original-segments");
        changed["explanation"] = json!("PRIVATE_BACKEND_TEXT");
        assert!(processing(&changed, &verification, &input).is_err());
    }
    let mut actual = ActualProcessing::new(
        ProcessingRouteId::MpegTsSegments,
        InputPreparation::OriginalSegments,
    );
    let decoded = json!({"schema_version":2,"required_depth":"decoded"});
    actual.required_tools = vec![RequiredTool::Ffmpeg, RequiredTool::Ffprobe];
    assert!(processing(&serde_json::to_value(&actual).unwrap(), &decoded, &input).is_err());
    actual.required_tools.push(RequiredTool::ShakaPackager);
    assert!(processing(&serde_json::to_value(&actual).unwrap(), &decoded, &input).is_ok());
    assert!(
        processing(
            &serde_json::to_value(&actual).unwrap(),
            &verification,
            &input
        )
        .is_err()
    );
}

#[test]
fn saved_processing_schemas_keep_frozen_choices_without_private_source_paths() {
    let mut single = json!({"kind":"execution-plan","schema_version":5,"plan_id":"a".repeat(64),
        "source_format":"hls","source":{"root":"PRIVATE_SOURCE_PATH"},
        "intent":{"format":"dash","target":"cbcs","output":"PRIVATE_OUTPUT_PATH"},
        "required_depth":"native","processing":selection("segments","preserve")});
    let out = plan(&single).unwrap().0;
    assert_eq!(out["processing"], single["processing"]);
    assert!(!out.to_string().contains("PRIVATE_"));
    single["processing"]["resolved"] = json!("auto");
    assert!(plan(&single).is_err());
    let bundle = json!({"kind":"execution-plan","schema_version":6,"plan_id":"b".repeat(64),
        "source_format":"hls-ts","source":{"root":"PRIVATE_SOURCE_PATH"},"output_root":"PRIVATE_OUTPUT_ROOT",
        "required_depth":"native","outputs":[
            {"id":"hls","format":"hls-ts","target":"clear","drms":[],"conformance":"none","processing":"segments"},
            {"id":"dash","format":"dash","target":"clear","drms":[],"conformance":"none","processing":"stitch"}],
        "processing":[{"id":"hls","selection":selection("segments","preserve")},
            {"id":"dash","selection":selection("stitch","resegment")}]});
    let out = plan(&bundle).unwrap().0;
    assert_eq!(out["processing"], bundle["processing"]);
    assert_eq!(out["outputs"], bundle["outputs"]);
    assert!(!out.to_string().contains("PRIVATE_"));
    for changed in [
        json!(null),
        json!([]),
        json!([{"id":"PRIVATE_ID","selection":selection("segments","preserve")}]),
    ] {
        let mut invalid = bundle.clone();
        invalid["processing"] = changed;
        assert!(plan(&invalid).is_err());
    }
    let mut legacy = bundle;
    legacy["schema_version"] = json!(4);
    assert!(plan(&legacy).is_err());
}
