use super::*;
use drmr_core::{
    conversion::layout::{LayoutMode, SourceLayout, SourceTrack},
    inspection::TrackKind,
};

fn source_layout() -> Value {
    serde_json::to_value(
        SourceLayout::resolve_from_tracks(
            LayoutMode::Source,
            OutputFormat::Dash,
            &[SourceTrack {
                source: "PRIVATE_SOURCE/media.m3u8".into(),
                selector: 0,
                kind: TrackKind::Video,
                playlist: Some("PRIVATE_SOURCE/media.m3u8".into()),
                media: vec!["PRIVATE_MEDIA/segment.m4s".into()],
            }],
        )
        .unwrap(),
    )
    .unwrap()
}

fn saved(schema: u64, mode: &str) -> Value {
    let selection = json!({"requested":"stitch","resolved":"stitch","boundaries":"resegment","segment_duration_ms":6000});
    let options = json!({"layout":source_layout(),"dash_addressing":mode});
    let mut value = json!({"kind":"execution-plan","schema_version":schema,"plan_id":"a".repeat(64),
        "source_format":"hls","source":{"root":"PRIVATE_ROOT"},"required_depth":"native"});
    if schema == 7 {
        value["intent"] = json!({"format":"dash","target":"clear","output":"PRIVATE_OUTPUT"});
        value["processing"] = selection;
        value["output_options"] = options;
    } else {
        value["output_root"] = json!("PRIVATE_OUTPUT");
        value["outputs"] = json!([{"id":"dash","format":"dash","target":"clear","drms":[],"conformance":"none","processing":"stitch","dash_addressing":mode}]);
        value["processing"] = json!([{"id":"dash","selection":selection}]);
        value["output_options"] = json!([{"id":"dash","options":options}]);
    }
    value
}

#[test]
fn saved_output_options_are_closed_path_free_and_bound_to_each_new_output() {
    for schema in [7, 8] {
        for mode in ["template", "template-timeline", "list", "base"] {
            let value = saved(schema, mode);
            let out = plan(&value).unwrap().0;
            let options = if schema == 7 {
                &out["output_options"]
            } else {
                &out["output_options"][0]["options"]
            };
            assert_eq!(options, &json!({"layout":"source","dash_addressing":mode}));
            assert!(!out.to_string().contains("PRIVATE_"));
        }
        for malformed in [Value::Null, json!({}), json!([])] {
            let mut value = saved(schema, "list");
            value["output_options"] = malformed;
            assert!(plan(&value).is_err());
        }
        let mut value = saved(schema, "list");
        value.as_object_mut().unwrap().remove("output_options");
        assert!(plan(&value).is_err());
    }
}

#[test]
fn saved_output_options_refuse_unknown_fields_nulls_and_foreign_bindings() {
    for schema in [7, 8] {
        for (field, malformed) in [
            ("layout", Value::Null),
            ("layout", json!({"mode":"normalized","tracks":[]})),
            ("dash_addressing", Value::Null),
            ("dash_addressing", json!("template-duration")),
            ("private", json!("PRIVATE_SOURCE")),
        ] {
            let mut value = saved(schema, "list");
            let options = if schema == 7 {
                &mut value["output_options"]
            } else {
                &mut value["output_options"][0]["options"]
            };
            options[field] = malformed;
            assert!(plan(&value).is_err(), "schema {schema}, field {field}");
        }
    }
    for field in ["output_options", "processing"] {
        let mut value = saved(8, "list");
        value[field][0]["id"] = json!("other");
        assert!(plan(&value).is_err());
    }
    let mut value = saved(8, "list");
    value["outputs"][0]["dash_addressing"] = json!("base");
    assert!(plan(&value).is_err());
    for schema in [5, 6] {
        let mut value = saved(schema + 2, "list");
        value["schema_version"] = json!(schema);
        assert!(plan(&value).is_err());
    }
}

#[test]
fn output_refusals_remain_failed_reports_without_private_details() {
    for code in [
        "dash-nominal-duration-not-representable",
        "source-layout-collision",
    ] {
        let value = json!({"schema_version":1,"command":"convert","status":"failed",
            "error":{"code":code,"hint":"PRIVATE_OUTPUT_DETAIL","last_activity":"verification"}});
        let projected = project(OperationName::Convert, None, None, &value).unwrap();
        assert!(!projected.success);
        assert_eq!(projected.kind, "failure");
        assert_eq!(projected.value["error"]["code"], code);
        assert!(
            !projected
                .value
                .to_string()
                .contains("PRIVATE_OUTPUT_DETAIL")
        );
    }
}
