//! Closed views of existing CLI reports. No raw result is an HTTP response.
use super::wire::{Error, Result};
use crate::common_protocol::OperationName;
use drmr_core::inspection::{ActualProcessing, Inspection, OutputFormat};
use serde::de::DeserializeOwned;
use serde_json::{Map, Value, json};

pub struct Projected {
    pub value: Value,
    pub kind: &'static str,
    pub success: bool,
}
fn bad() -> Error {
    Error::new(503, "result-invalid")
}
fn object<'a>(v: &'a Value, fields: &[&str]) -> Result<&'a Map<String, Value>> {
    let m = v.as_object().ok_or_else(bad)?;
    if m.keys().any(|k| !fields.contains(&k.as_str())) {
        return Err(bad());
    }
    Ok(m)
}
fn select(v: &Value, fields: &[&str]) -> Value {
    Value::Object(
        fields
            .iter()
            .filter_map(|k| v.get(*k).map(|v| ((*k).into(), v.clone())))
            .collect(),
    )
}
fn boolean(v: &Value) -> Result<bool> {
    v.as_bool().ok_or_else(bad)
}
fn integer(v: &Value) -> Result<u64> {
    v.as_u64().ok_or_else(bad)
}
fn text<'a>(v: &'a Value, allowed: &[&str]) -> Result<&'a str> {
    let s = v.as_str().ok_or_else(bad)?;
    allowed.contains(&s).then_some(s).ok_or_else(bad)
}
fn array(v: &Value, f: impl Fn(&Value) -> Result<Value>) -> Result<Value> {
    Ok(Value::Array(
        v.as_array()
            .ok_or_else(bad)?
            .iter()
            .map(f)
            .collect::<Result<Vec<_>>>()?,
    ))
}
fn typed<T: DeserializeOwned>(v: &Value) -> Result<T> {
    serde_json::from_value(v.clone()).map_err(|_| bad())
}
fn version(v: &Value, n: u64) -> Result<()> {
    if integer(&v["schema_version"])? == n {
        Ok(())
    } else {
        Err(bad())
    }
}
fn checks(v: &Value) -> Result<Value> {
    object(
        v,
        &[
            "stream_metadata",
            "packet_payloads_equal",
            "packet_timing_equal",
            "decoded_content_equal",
        ],
    )?;
    text(&v["stream_metadata"], &["pass", "fail"])?;
    for k in [
        "packet_payloads_equal",
        "packet_timing_equal",
        "decoded_content_equal",
    ] {
        boolean(&v[k])?;
    }
    Ok(select(
        v,
        &[
            "stream_metadata",
            "packet_payloads_equal",
            "packet_timing_equal",
            "decoded_content_equal",
        ],
    ))
}
fn checks_pass(v: &Value) -> bool {
    v["stream_metadata"] == "pass"
        && v["packet_payloads_equal"] == true
        && v["packet_timing_equal"] == true
        && v["decoded_content_equal"] == true
}
fn inspection(v: &Value) -> Result<Value> {
    object(
        v,
        &[
            "schema_version",
            "format",
            "evidence",
            "protection",
            "tracks",
            "drm_systems",
            "findings",
            "media_verified",
            "details",
        ],
    )?;
    version(v, 1)?;
    for t in v["tracks"].as_array().ok_or_else(bad)? {
        object(
            t,
            &["id", "kind", "codec", "protection", "key_ids", "pattern"],
        )?;
        if let Some(p) = t.get("pattern").filter(|p| !p.is_null()) {
            object(p, &["encrypted_blocks", "clear_blocks"])?;
        }
    }
    if let Some(details) = v.get("details").filter(|v| !v.is_null()) {
        object(details, &["track_protection", "periods"])?;
        object(&details["periods"], &["status", "count"])?;
        for t in details["track_protection"].as_array().ok_or_else(bad)? {
            object(
                t,
                &[
                    "track_id",
                    "evidence",
                    "cipher",
                    "encryption_unit",
                    "pattern",
                    "sample_coverage",
                    "sample_coverage_reason",
                    "hls_key_methods",
                ],
            )?;
            object(&t["pattern"], &["status", "value"])?;
            if let Some(p) = t["pattern"].get("value") {
                object(p, &["encrypted_blocks", "clear_blocks"])?;
            }
            if let Some(reason) = t.get("sample_coverage_reason") {
                text(
                    reason,
                    &[
                        "target-intent-not-sample-evidence",
                        "sample-auxiliary-data-not-inspected",
                    ],
                )?;
            }
        }
    }
    let i: Inspection = typed(v)?;
    for f in &i.findings {
        if !FIXED_INSPECTION_TOKENS.contains(&f.code.as_str()) {
            return Err(bad());
        }
    }
    for d in &i.drm_systems {
        if !(matches!(
            d.as_str(),
            "widevine" | "playready" | "fairplay" | "clearkey"
        ) || d.len() == 32 && d.bytes().all(|b| b.is_ascii_hexdigit()))
        {
            return Err(bad());
        }
    }
    let mut output = serde_json::to_value(i).map_err(|_| bad())?;
    for t in output["tracks"].as_array_mut().ok_or_else(bad)? {
        let m = t.as_object_mut().ok_or_else(bad)?;
        m.remove("id");
        m.remove("key_ids");
        if m.get("codec").and_then(Value::as_str).is_some_and(|s| {
            s.is_empty()
                || s.len() > 256
                || !s
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b".,_-".contains(&b))
        }) {
            m.insert("codec".into(), Value::Null);
        }
    }
    if let Some(rows) = output
        .get_mut("details")
        .and_then(|d| d.get_mut("track_protection"))
        .and_then(Value::as_array_mut)
    {
        for row in rows {
            row.as_object_mut().ok_or_else(bad)?.remove("track_id");
        }
    }
    Ok(output)
}
fn processing(v: &Value, verification: &Value, input: &Value) -> Result<Value> {
    use drmr_core::inspection::{ProcessingRouteId, RequiredTool};
    object(
        v,
        &[
            "route",
            "input_preparation",
            "reason_code",
            "explanation",
            "required_tools",
        ],
    )?;
    let p: ActualProcessing = typed(v)?;
    let mut expected = ActualProcessing::new(p.route, p.input_preparation);
    let decoded =
        verification["schema_version"] == 1 || verification["required_depth"] == "decoded";
    if decoded {
        expected
            .required_tools
            .splice(0..0, [RequiredTool::Ffmpeg, RequiredTool::Ffprobe]);
    } else {
        let encrypted_mp4 = input["format"] == "mp4"
            && matches!(input["protection"].as_str(), Some("cenc" | "cbcs"));
        let can_copy = encrypted_mp4
            && matches!(
                p.route,
                ProcessingRouteId::ProgressiveMp4
                    | ProcessingRouteId::FragmentedMp4
                    | ProcessingRouteId::DashPresentation
                    | ProcessingRouteId::Fmp4HlsPresentation
            );
        let copy_required = encrypted_mp4 && p.route == ProcessingRouteId::ProgressiveMp4;
        if copy_required || (can_copy && p.required_tools.first() == Some(&RequiredTool::Ffmpeg)) {
            expected.required_tools.insert(0, RequiredTool::Ffmpeg);
        }
    }
    if p != expected {
        return Err(bad());
    }
    serde_json::to_value(p).map_err(|_| bad())
}
fn reference(v: &Value) -> Result<Value> {
    object(v, &["url", "section", "revision"])?;
    text(&v["url"], NATIVE_URLS)?;
    text(&v["section"], NATIVE_SECTIONS)?;
    text(&v["revision"], NATIVE_REVISIONS)?;
    Ok(select(v, &["url", "section", "revision"]))
}
fn location(v: &Value) -> Result<Value> {
    if v.is_null() {
        return Ok(Value::Null);
    }
    object(v, &["document", "span", "line", "column"])?;
    for k in ["document", "line", "column"] {
        integer(&v[k])?;
    }
    object(&v["span"], &["start", "end"])?;
    integer(&v["span"]["start"])?;
    integer(&v["span"]["end"])?;
    Ok(select(v, &["document", "span", "line", "column"]))
}
fn reason(v: &Value) -> Result<Value> {
    text(
        v,
        &[
            "missing-linked-manifest",
            "unsupported-feature",
            "requires-media-evidence",
            "requires-delivery-evidence",
            "resource-limit",
            "invalid-prerequisite",
            "missing-reference-base",
        ],
    )?;
    Ok(v.clone())
}
fn native(v: &Value) -> Result<Value> {
    const F: &[&str] = &[
        "schema_version",
        "scope",
        "engine_revision",
        "profile",
        "profile_revision",
        "errors",
        "warnings",
        "truncated",
        "incomplete",
        "document_count",
        "workers_used",
        "diagnostics",
        "coverage",
    ];
    object(v, F)?;
    version(v, 1)?;
    text(&v["scope"], &["manifest-only"])?;
    text(&v["engine_revision"], &[env!("CARGO_PKG_VERSION")])?;
    text(
        &v["profile"],
        &[
            "hls-base-manifest",
            "apple-vod-manifest",
            "dash-static-manifest",
        ],
    )?;
    text(&v["profile_revision"], NATIVE_REVISIONS)?;
    for k in ["errors", "warnings", "document_count", "workers_used"] {
        integer(&v[k])?;
    }
    for k in ["truncated", "incomplete"] {
        boolean(&v[k])?;
    }
    let diagnostics = array(&v["diagnostics"], |d| {
        object(
            d,
            &[
                "rule_id",
                "severity",
                "message_code",
                "location",
                "reference",
            ],
        )?;
        text(&d["rule_id"], NATIVE_RULES)?;
        text(&d["message_code"], NATIVE_MESSAGES)?;
        text(&d["severity"], &["error", "warning"])?;
        Ok(
            json!({"rule_id":d["rule_id"],"severity":d["severity"],"message_code":d["message_code"],"location":location(&d["location"] )?,"reference":reference(&d["reference"])?}),
        )
    })?;
    let coverage = array(&v["coverage"], |c| {
        const F: &[&str] = &[
            "document",
            "rule_id",
            "scope",
            "severity",
            "status",
            "passed_checks",
            "failed_checks",
            "not_checked_checks",
            "not_checked_reasons",
            "reference",
        ];
        object(c, F)?;
        for k in [
            "document",
            "passed_checks",
            "failed_checks",
            "not_checked_checks",
        ] {
            integer(&c[k])?;
        }
        text(&c["rule_id"], NATIVE_RULES)?;
        text(&c["scope"], &["in-scope", "outside-scope"])?;
        text(&c["severity"], &["error", "warning"])?;
        let status = &c["status"];
        object(status, &["status", "reason"])?;
        let token = text(&status["status"], &["passed", "failed", "not-checked"])?;
        if token == "not-checked" {
            reason(&status["reason"])?;
        } else if status.get("reason").is_some() {
            return Err(bad());
        }
        array(&c["not_checked_reasons"], reason)?;
        reference(&c["reference"])?;
        Ok(select(c, F))
    })?;
    let mut output = select(v, F);
    output["diagnostics"] = diagnostics;
    output["coverage"] = coverage;
    Ok(output)
}
fn legacy_verification(v: &Value) -> Result<Value> {
    const F: &[&str] = &[
        "schema_version",
        "passed",
        "media_verified",
        "checks",
        "license_playback_verified",
        "native_manifest",
    ];
    object(v, F)?;
    version(v, 1)?;
    for k in ["passed", "media_verified", "license_playback_verified"] {
        boolean(&v[k])?;
    }
    let mut output = select(v, F);
    output["checks"] = checks(&v["checks"])?;
    if let Some(n) = v.get("native_manifest") {
        output["native_manifest"] = native(n)?;
    }
    Ok(output)
}
fn verification(v: &Value) -> Result<Value> {
    const F: &[&str] = &[
        "schema_version",
        "required_depth",
        "scope",
        "passed",
        "checks",
        "decode_evidence",
        "failure",
        "license_playback_verified",
        "native_manifest",
    ];
    if v["schema_version"] == 1 {
        return legacy_verification(v);
    }
    object(v, F)?;
    version(v, 2)?;
    let depth: drmr_core::conversion::ValidationDepth = typed(&v["required_depth"])?;
    let scope: drmr_core::conversion::VerificationScope = typed(&v["scope"])?;
    let checks: drmr_core::conversion::VerificationChecks = typed(&v["checks"])?;
    let decode: drmr_core::conversion::DecodeEvidence = typed(&v["decode_evidence"])?;
    if let Some(failure) = v.get("failure") {
        let _: drmr_core::conversion::VerificationFailure = typed(failure)?;
    }
    if boolean(&v["license_playback_verified"])? {
        return Err(bad());
    }
    let mut out = select(v, F);
    if let Some(manifest) = v.get("native_manifest") {
        out["native_manifest"] = native(manifest)?;
    }
    let expected = checks.is_pass()
        && decode.satisfies(depth, scope)
        && v.get("failure").is_none()
        && v.get("native_manifest").is_none_or(|n| {
            n["errors"] == 0 && n["truncated"] == false && n["incomplete"] == false
        });
    if boolean(&v["passed"])? != expected {
        return Err(bad());
    }
    Ok(out)
}
fn verified(v: &Value) -> bool {
    if v["schema_version"] == 2 {
        return v["passed"] == true && v.get("failure").is_none();
    }
    v["passed"] == true
        && v["media_verified"] == true
        && checks_pass(&v["checks"])
        && v.get("native_manifest")
            .is_none_or(|n| n["errors"] == 0 && n["truncated"] == false && n["incomplete"] == false)
}
fn media_report_version(v: &Value) -> Result<()> {
    match integer(&v["schema_version"])? {
        1 | 2 => Ok(()),
        _ => Err(bad()),
    }
}
fn comparison_verification(v: &Value, parent_version: &Value) -> Result<Value> {
    let out = verification(v)?;
    if &v["schema_version"] != parent_version
        || (v["schema_version"] == 2 && v["scope"] != "comparison")
    {
        return Err(bad());
    }
    Ok(out)
}
fn conversion(v: &Value, with_plan: bool) -> Result<Value> {
    const F: &[&str] = &[
        "schema_version",
        "plan_id",
        "input",
        "output",
        "verification",
        "processing",
        "measurements",
    ];
    object(v, F)?;
    media_report_version(v)?;
    let mut output = select(
        v,
        if with_plan {
            &F[..6]
        } else {
            &[
                "schema_version",
                "input",
                "output",
                "verification",
                "processing",
            ]
        },
    );
    output["input"] = inspection(&v["input"])?;
    output["output"] = inspection(&v["output"])?;
    output["verification"] = comparison_verification(&v["verification"], &v["schema_version"])?;
    if let Some(p) = v.get("processing") {
        output["processing"] = processing(p, &output["verification"], &output["input"])?;
    }
    Ok(output)
}
fn signaling(v: &Value) -> Result<Value> {
    object(v, &["systems", "verified"])?;
    let _: Vec<drmr_core::conversion::DrmSystem> = typed(&v["systems"])?;
    boolean(&v["verified"])?;
    Ok(select(v, &["systems", "verified"]))
}
fn conformance(v: &Value) -> Result<Value> {
    const F: &[&str] = &[
        "schema_version",
        "scope",
        "passed",
        "manifest_count",
        "checks",
        "hls_versions",
    ];
    if v.is_null() {
        return Ok(Value::Null);
    }
    object(v, F)?;
    version(v, 1)?;
    text(&v["scope"], &["manifest-only"])?;
    boolean(&v["passed"])?;
    integer(&v["manifest_count"])?;
    array(&v["checks"], |c| {
        object(
            c,
            &[
                "tool",
                "passed",
                "checked_manifest_count",
                "warning_count",
                "observed_version",
            ],
        )?;
        text(
            &c["tool"],
            &["apple-mediastreamvalidator", "dash-if-manifest"],
        )?;
        boolean(&c["passed"])?;
        integer(&c["checked_manifest_count"])?;
        integer(&c["warning_count"])?;
        if !c["observed_version"].is_null() {
            let s = c["observed_version"].as_str().ok_or_else(bad)?;
            let valid = s.strip_prefix("git:").map_or_else(
                || {
                    !s.is_empty()
                        && s.len() <= 64
                        && s.bytes().all(|b| b.is_ascii_digit() || b == b'.')
                },
                |h| h.len() == 40 && h.bytes().all(|b| b.is_ascii_hexdigit()),
            );
            if !valid {
                return Err(bad());
            }
        }
        Ok(select(
            c,
            &[
                "tool",
                "passed",
                "checked_manifest_count",
                "warning_count",
                "observed_version",
            ],
        ))
    })?;
    array(&v["hls_versions"], |h| {
        object(h, &["manifest_ordinal", "declared", "minimum"])?;
        integer(&h["manifest_ordinal"])?;
        integer(&h["minimum"])?;
        if !h["declared"].is_null() {
            integer(&h["declared"])?;
        }
        Ok(select(h, &["manifest_ordinal", "declared", "minimum"]))
    })?;
    Ok(select(v, F))
}
fn failure(v: &Value, operation: OperationName) -> Result<Value> {
    if v["command"] != operation.as_str() {
        return Err(bad());
    }
    text(&v["status"], &["failed", "cancelled"])?;
    object(&v["error"], &["code", "hint", "last_activity"])?;
    let code = text(&v["error"]["code"], FAILURE_CODES)?;
    let activity = v["error"]["last_activity"].as_str();
    if !v["error"]["last_activity"].is_null()
        && activity.is_none_or(|s| !crate::common_protocol::activity(s))
    {
        return Err(bad());
    }
    Ok(
        json!({"schema_version":1,"command":operation.as_str(),"status":v["status"],"error":{"code":code,"hint":"Review the error code and current source or saved plan before choosing the next operation.","last_activity":activity}}),
    )
}
fn analysis(v: &Value) -> Result<(Value, &'static str, bool)> {
    Ok({
        const LEGACY: &[&str] = &[
            "kind",
            "schema_version",
            "input_admitted",
            "input_checks",
            "input",
            "actual_format",
            "track_protection",
            "compatibility_notes",
            "eligible_formats",
            "source",
            "download",
            "measurements",
        ];
        let current = LEGACY
            .iter()
            .map(|field| {
                if *field == "input_checks" {
                    "verification"
                } else {
                    *field
                }
            })
            .collect::<Vec<_>>();
        let schema = integer(&v["schema_version"])?;
        let fields = match schema {
            1 => LEGACY,
            2 => current.as_slice(),
            _ => return Err(bad()),
        };
        object(v, fields)?;
        media_report_version(v)?;
        text(&v["kind"], &["input-analysis"])?;
        boolean(&v["input_admitted"])?;
        let _: OutputFormat = typed(&v["actual_format"])?;
        let _: Vec<OutputFormat> = typed(&v["eligible_formats"])?;
        array(&v["track_protection"], |x| {
            text(
                x,
                &[
                    "clear",
                    "cenc",
                    "cbcs",
                    "hls-aes128",
                    "hls-sample-aes",
                    "mixed",
                ],
            )?;
            Ok(x.clone())
        })?;
        array(&v["compatibility_notes"], |x| {
            text(x, &["ts-empty-payload-padding", "hls-empty-cue-pair"])?;
            Ok(x.clone())
        })?;
        let mut out = select(v, &fields[..9]);
        out["input"] = inspection(&v["input"])?;
        let success = if schema == 1 {
            out["input_checks"] = checks(&v["input_checks"])?;
            checks_pass(&v["input_checks"])
        } else {
            out["verification"] = verification(&v["verification"])?;
            if v["verification"]["scope"] != "source-admission" {
                return Err(bad());
            }
            verified(&out["verification"])
        };
        (out, "analysis", v["input_admitted"] == true && success)
    })
}
fn plan(v: &Value) -> Result<(Value, &'static str, bool)> {
    Ok({
        if v["kind"] != "execution-plan" {
            return Err(bad());
        }
        let schema = integer(&v["schema_version"])?;
        let mut out = if matches!(schema, 1 | 3) {
            object(
                v,
                &[
                    "kind",
                    "schema_version",
                    "plan_id",
                    "source_format",
                    "source",
                    "intent",
                    "measurements",
                    "required_depth",
                ],
            )?;
            object(&v["intent"], &["format", "target", "output"])?;
            let _: drmr_core::conversion::plan::OutputIntent = typed(&v["intent"])?;
            let mut out = select(v, &["kind", "schema_version", "plan_id", "source_format"]);
            out["intent"] = select(&v["intent"], &["format", "target"]);
            out
        } else if matches!(schema, 2 | 4) {
            object(
                v,
                &[
                    "kind",
                    "schema_version",
                    "plan_id",
                    "source_format",
                    "source",
                    "output_root",
                    "outputs",
                    "measurements",
                    "required_depth",
                ],
            )?;
            let _: Vec<drmr_core::conversion::plan::bundle::BundleOutputIntent> =
                typed(&v["outputs"])?;
            for o in v["outputs"].as_array().ok_or_else(bad)? {
                object(o, &["id", "format", "target", "drms", "conformance"])?;
            }
            select(
                v,
                &[
                    "kind",
                    "schema_version",
                    "plan_id",
                    "source_format",
                    "outputs",
                ],
            )
        } else {
            return Err(bad());
        };
        if schema >= 3 {
            let _: drmr_core::conversion::ValidationDepth = typed(&v["required_depth"])?;
            out["required_depth"] = v["required_depth"].clone();
        } else if v.get("required_depth").is_some() {
            return Err(bad());
        }
        let _: OutputFormat = typed(&v["source_format"])?;
        (out, "plan", true)
    })
}
fn single_conversion(v: &Value) -> Result<(Value, &'static str, bool)> {
    Ok({
        let out = conversion(v, true)?;
        let success = verified(&out["verification"]);
        (out, "conversion", success)
    })
}
fn bundle_output(o: &Value, verify: bool) -> Result<Value> {
    object(
        o,
        &[
            "id",
            "conversion",
            "output_matches_plan",
            "verification",
            "signaling",
            "conformance",
        ],
    )?;
    let id = o["id"].as_str().ok_or_else(bad)?;
    if !safe_id(id) {
        return Err(bad());
    }
    let mut out = json!({"id":id,"signaling":signaling(&o["signaling"] )?,"conformance":conformance(&o["conformance"])?});
    if verify {
        out["output_matches_plan"] = json!(boolean(&o["output_matches_plan"])?);
        out["verification"] = verification(&o["verification"])?;
    } else {
        out["conversion"] = conversion(&o["conversion"], false)?;
    }
    Ok(out)
}
fn bundle(operation: OperationName, v: &Value) -> Result<(Value, &'static str, bool)> {
    Ok({
        let verify = operation == OperationName::Verify;
        let expected = if verify {
            "bundle-verification"
        } else {
            "bundle-conversion"
        };
        if v["kind"] != expected {
            return Err(bad());
        }
        object(
            v,
            &[
                "kind",
                "schema_version",
                "plan_id",
                "source_unchanged",
                "passed",
                "outputs",
                "command",
                "status",
                "error",
                "measurements",
            ],
        )?;
        media_report_version(v)?;
        let outputs = array(&v["outputs"], |o| bundle_output(o, verify))?;
        let mut out = select(
            v,
            &[
                "kind",
                "schema_version",
                "plan_id",
                "source_unchanged",
                "passed",
            ],
        );
        out["outputs"] = outputs;
        let rows = out["outputs"].as_array().ok_or_else(bad)?;
        if !(1..=8).contains(&rows.len()) {
            return Err(bad());
        }
        for row in rows {
            let media = if verify {
                &row["verification"]
            } else {
                &row["conversion"]
            };
            if media["schema_version"] != v["schema_version"]
                || (verify && media["schema_version"] == 2 && media["scope"] != "comparison")
            {
                return Err(bad());
            }
        }
        let success = rows.iter().all(|o| {
            verified(if verify {
                &o["verification"]
            } else {
                &o["conversion"]["verification"]
            }) && (o["signaling"]["systems"]
                .as_array()
                .is_some_and(Vec::is_empty)
                || o["signaling"]["verified"] == true)
                && (o["conformance"].is_null() || o["conformance"]["passed"] == true)
        }) && if verify {
            boolean(&v["source_unchanged"])?
                && boolean(&v["passed"])?
                && rows.iter().all(|o| o["output_matches_plan"] == true)
        } else {
            true
        };
        (
            out,
            if verify { "verification" } else { "conversion" },
            success,
        )
    })
}
fn single_verification(v: &Value) -> Result<(Value, &'static str, bool)> {
    Ok({
        object(
            v,
            &[
                "kind",
                "schema_version",
                "plan_id",
                "source_unchanged",
                "output_matches_plan",
                "verification",
                "command",
                "status",
                "error",
                "measurements",
            ],
        )?;
        media_report_version(v)?;
        text(&v["kind"], &["plan-verification"])?;
        let mut out = select(
            v,
            &[
                "kind",
                "schema_version",
                "plan_id",
                "source_unchanged",
                "output_matches_plan",
            ],
        );
        out["verification"] = comparison_verification(&v["verification"], &v["schema_version"])?;
        let success = boolean(&v["source_unchanged"])?
            && boolean(&v["output_matches_plan"])?
            && verified(&out["verification"]);
        (out, "verification", success)
    })
}
pub fn project(
    operation: OperationName,
    schema: Option<u32>,
    plan_id: Option<&str>,
    v: &Value,
) -> Result<Projected> {
    if v.get("command").is_some() && v.get("kind").is_none() {
        object(v, &["schema_version", "command", "status", "error"])?;
        version(v, 1)?;
        return Ok(Projected {
            value: failure(v, operation)?,
            kind: "failure",
            success: false,
        });
    }
    let (mut value, kind, success) = match operation {
        OperationName::Analyze => analysis(v)?,
        OperationName::Plan => plan(v)?,
        OperationName::Convert if matches!(schema, Some(1 | 3)) => single_conversion(v)?,
        OperationName::Convert | OperationName::Verify if matches!(schema, Some(2 | 4)) => {
            bundle(operation, v)?
        }
        OperationName::Verify if matches!(schema, Some(1 | 3)) => single_verification(v)?,
        _ => return Err(bad()),
    };
    if operation != OperationName::Analyze {
        let id = value["plan_id"].as_str().ok_or_else(bad)?;
        if id.len() != 64
            || !id.bytes().all(|b| b.is_ascii_hexdigit())
            || plan_id.is_some_and(|p| p != id)
        {
            return Err(bad());
        }
    }
    if v.get("error").is_some() {
        let overlay = failure(v, operation)?;
        for k in ["command", "status", "error"] {
            value[k] = overlay[k].clone();
        }
    }
    Ok(Projected {
        value,
        kind,
        success,
    })
}
fn safe_id(id: &str) -> bool {
    let b = id.as_bytes();
    !b.is_empty()
        && b.len() <= 32
        && b[0].is_ascii_alphanumeric()
        && b.iter()
            .all(|x| x.is_ascii_alphanumeric() || b"_-".contains(x))
}

#[cfg(test)]
mod coverage_tests {
    use super::*;

    fn native_report() -> Value {
        json!({"schema_version":2,"required_depth":"native","scope":"comparison","passed":true,
            "checks":{"stream_metadata":"pass","packet_payloads_equal":true,"packet_timing_equal":true},
            "decode_evidence":{"status":"not-checked","reason":"not-requested"},"license_playback_verified":false})
    }

    #[test]
    fn native_and_decoded_report_evidence_cannot_be_confused() {
        let native = native_report();
        assert!(verified(&verification(&native).unwrap()));
        let mut required = native.clone();
        required["required_depth"] = json!("decoded");
        assert!(verification(&required).is_err());
        required["passed"] = json!(false);
        assert!(!verified(&verification(&required).unwrap()));
        required["passed"] = json!(true);
        required["decode_evidence"] = json!({"status":"content-compared","equal":true});
        assert!(verified(&verification(&required).unwrap()));
        required["decode_evidence"] = json!({"status":"source-decoded"});
        assert!(verification(&required).is_err());
        required["scope"] = json!("source-admission");
        assert!(verified(&verification(&required).unwrap()));
    }

    #[test]
    fn obsolete_booleans_and_a_failed_child_cannot_authorize_native_success() {
        let mut obsolete = native_report();
        obsolete["media_verified"] = json!(true);
        assert!(verification(&obsolete).is_err());
        let mut obsolete = native_report();
        obsolete["checks"]["decoded_content_equal"] = json!(true);
        assert!(verification(&obsolete).is_err());
        let mut failed = native_report();
        failed["failure"] = json!("child-check-failed");
        assert!(verification(&failed).is_err());
        failed["passed"] = json!(false);
        assert!(!verified(&verification(&failed).unwrap()));
        let refused = project(OperationName::Analyze, None, None, &json!({
            "schema_version":1,"command":"analyze","status":"failed",
            "error":{"code":"native-codec-profile-not-supported","hint":"PRIVATE_TOOL_MARKER","last_activity":"verification"}
        })).unwrap();
        assert!(!refused.success);
        assert_eq!(
            refused.value["error"]["code"],
            "native-codec-profile-not-supported"
        );
        assert!(!refused.value.to_string().contains("PRIVATE_TOOL_MARKER"));
    }

    #[test]
    fn native_static_refusals_retain_their_code_without_private_detail_passthrough() {
        for code in [
            "invalid-avc-syntax",
            "native-aac-clock-mismatch",
            "native-aac-config-invalid",
            "native-aac-config-not-supported",
            "native-avc-access-unit-size-limit",
            "native-avc-sync-sample-mismatch",
            "native-cipher-counter-overflow",
            "native-codec-configuration-not-supported",
            "native-codec-profile-not-supported",
            "native-media-clock-overflow",
            "native-media-duration-mismatch",
            "native-mp4-invalid",
            "native-mp4-profile-not-supported",
            "native-presentation-metadata-invalid",
            "native-presentation-window-not-supported",
            "native-sample-evidence-incomplete",
            "native-sample-evidence-invalid",
            "native-sample-protection-mismatch",
            "native-source-changed",
            "native-track-changed",
            "native-video-dimensions-mismatch",
            "presentation-window-outside-media",
            "ts-aac-mp4-invalid",
            "ts-aac-mp4-write-failed",
            "ts-avc-picture-duration-not-supported",
            "ts-avc-separate-colour-plane-not-supported",
            "ts-avc-slice-groups-not-supported",
            "ts-avc-slice-order-not-supported",
            "ts-segment-evidence-incomplete",
            "ts-segment-evidence-limit",
            "ts-segment-evidence-missing",
            "ts-segment-evidence-unfinished",
            "ts-video-duration-clock-required",
            "ts-video-reorder-limit",
            "verification-native-evidence-required",
        ] {
            let payload = json!({"schema_version":1,"command":"analyze","status":"failed",
                "error":{"code":code,"hint":"PRIVATE_NATIVE_DETAIL","last_activity":"verification"}});
            let projected = project(OperationName::Analyze, None, None, &payload).unwrap();
            assert!(!projected.success);
            assert_eq!(projected.value["error"]["code"], code);
            assert!(
                !projected
                    .value
                    .to_string()
                    .contains("PRIVATE_NATIVE_DETAIL")
            );
        }
        for code in [
            "native-private-sentinel",
            "ts-private-sentinel",
            "observer-stopped",
        ] {
            let payload = json!({"schema_version":1,"command":"analyze","status":"failed",
                "error":{"code":code,"hint":"PRIVATE_NATIVE_DETAIL","last_activity":"verification"}});
            assert!(project(OperationName::Analyze, None, None, &payload).is_err());
        }
    }

    #[test]
    fn processing_tools_keep_native_copy_decode_and_legacy_contracts_distinct() {
        use drmr_core::inspection::{InputPreparation, ProcessingRouteId, RequiredTool};
        let native = native_report();
        let clear = json!({"format":"mp4","protection":"clear"});
        let encrypted = json!({"format":"mp4","protection":"cenc"});
        let mut actual = ActualProcessing::new(
            ProcessingRouteId::ProgressiveMp4,
            InputPreparation::CompleteFile,
        );
        let value = |report: &ActualProcessing| serde_json::to_value(report).unwrap();
        assert!(processing(&value(&actual), &native, &clear).is_ok());
        assert!(processing(&value(&actual), &native, &encrypted).is_err());
        actual.required_tools = vec![RequiredTool::Ffmpeg];
        assert!(processing(&value(&actual), &native, &encrypted).is_ok());
        assert!(processing(&value(&actual), &native, &clear).is_err());
        actual.required_tools.push(RequiredTool::Ffprobe);
        assert!(processing(&value(&actual), &native, &encrypted).is_err());
        let mut decoded = native.clone();
        decoded["required_depth"] = json!("decoded");
        assert!(processing(&value(&actual), &decoded, &clear).is_ok());
        assert!(processing(&value(&actual), &json!({"schema_version":1}), &clear).is_ok());
        actual.required_tools.clear();
        assert!(processing(&value(&actual), &json!({"schema_version":1}), &clear).is_err());
    }
}

const NATIVE_RULES: &[&str] = &[
    "APPLE-HLS-AUDIO-DESCRIPTION",
    "APPLE-HLS-AVERAGE-BANDWIDTH",
    "APPLE-HLS-HD-VARIANTS",
    "APPLE-HLS-IFRAME-AVERAGE-BANDWIDTH",
    "APPLE-HLS-LANGUAGE",
    "APPLE-HLS-MEDIA-ALIGNMENT",
    "APPLE-HLS-VARIANT-ATTRIBUTES",
    "APPLE-HLS-VARIANTS",
    "APPLE-HLS-VIDEO-CONDITIONS",
    "APPLE-HLS-VOD",
    "DASH-ADDRESSING-COVERAGE",
    "DASH-ATTRIBUTE-COVERAGE",
    "DASH-ATTRIBUTE-LEXICAL",
    "DASH-CENC-DEFAULT-KID",
    "DASH-CENC-PSSH-BOX",
    "DASH-DESCRIPTOR-SCHEME",
    "DASH-DRM-PAYLOAD",
    "DASH-ELEMENT-PLACEMENT",
    "DASH-EXECUTION-LIMIT",
    "DASH-FEATURE-COVERAGE",
    "DASH-FULL-XSD-SCHEMATRON",
    "DASH-MEDIA-EVIDENCE",
    "DASH-MPD-MIN-BUFFER-TIME",
    "DASH-MPD-PERIOD",
    "DASH-MPD-PROFILES",
    "DASH-MPD-ROOT",
    "DASH-PERIOD-TIMING",
    "DASH-PRESENTATION-REPRESENTATIONS",
    "DASH-PROFILE-STATIC",
    "DASH-PROTECTION-BASE64",
    "DASH-PROTECTION-PLACEMENT",
    "DASH-REPRESENTATION-ATTRIBUTES",
    "DASH-REPRESENTATION-IDENTITY",
    "DASH-SEGMENT-ADDRESSING",
    "DASH-TEMPLATE-INTEGERS",
    "DASH-TEMPLATE-TIMESCALE",
    "DASH-TEMPLATE-TOKENS",
    "DASH-TIMELINE-ARITHMETIC",
    "DASH-TIMELINE-ENTRIES",
    "DASH-XML-ENCODING",
    "DASH-XML-EXTERNAL",
    "DASH-XML-LIMIT",
    "DASH-XML-SYNTAX",
    "HLS-ATTRIBUTE-TYPES",
    "HLS-ATTRIBUTES",
    "HLS-ATTRIBUTES-UNIQUE",
    "HLS-BYTE-RANGE",
    "HLS-CHANNEL-EXTENSION",
    "HLS-CHANNELS",
    "HLS-CLOSED-CAPTIONS",
    "HLS-DATE-RANGE",
    "HLS-DATE-RANGE-CLIENT",
    "HLS-DATE-RANGE-COVERAGE",
    "HLS-DATE-RANGE-IDENTITY",
    "HLS-DATE-RANGE-OVERLAP",
    "HLS-DATE-RANGE-PDT",
    "HLS-DATE-RANGE-SCTE",
    "HLS-DATE-RANGE-TIME",
    "HLS-DATE-TIME",
    "HLS-DELIVERY-EVIDENCE",
    "HLS-EXTINF",
    "HLS-GRAPH-BINDINGS",
    "HLS-GRAPH-COVERAGE",
    "HLS-GRAPH-DATE-RANGES",
    "HLS-GRAPH-INDEPENDENT-DUPLICATE",
    "HLS-GRAPH-PDT-MAPPING",
    "HLS-GRAPH-PDT-PRESENCE",
    "HLS-GRAPH-PLAYLIST-KIND",
    "HLS-GRAPH-PLAYLIST-TYPES",
    "HLS-GRAPH-REFERENCES",
    "HLS-GRAPH-START",
    "HLS-GRAPH-START-DUPLICATE",
    "HLS-GRAPH-TARGET-DURATIONS",
    "HLS-HDCP-LEVEL",
    "HLS-HEADER",
    "HLS-I-FRAME-ATTRIBUTES",
    "HLS-KEY",
    "HLS-KEY-METHOD",
    "HLS-KEY-METHOD-CONSISTENCY",
    "HLS-LIMITS",
    "HLS-MAP",
    "HLS-MAP-KEY-IV",
    "HLS-MAP-RANGE",
    "HLS-MEDIA-EVIDENCE",
    "HLS-NFC",
    "HLS-PARSE-DEPENDENT",
    "HLS-PLAYLIST-KIND",
    "HLS-PLAYLIST-TYPE",
    "HLS-RENDITION",
    "HLS-RENDITION-AUTOSELECT",
    "HLS-RENDITION-DEFAULT",
    "HLS-RENDITION-GROUPS",
    "HLS-RENDITION-NAMES",
    "HLS-RENDITION-REFERENCES",
    "HLS-SCTE-EVIDENCE",
    "HLS-SEGMENT-ASSOCIATION",
    "HLS-SEQUENCE",
    "HLS-SESSION-DATA",
    "HLS-SESSION-DATA-DUPLICATES",
    "HLS-SESSION-KEY",
    "HLS-SESSION-KEY-DUPLICATES",
    "HLS-SESSION-KEY-MATCH",
    "HLS-START",
    "HLS-STATE-COVERAGE",
    "HLS-TAG-CARDINALITY",
    "HLS-TAG-SYNTAX",
    "HLS-TARGET-DURATION",
    "HLS-TEXT",
    "HLS-UNKNOWN-ATTRIBUTES",
    "HLS-UNKNOWN-TAGS",
    "HLS-URI",
    "HLS-UTF8",
    "HLS-VARIANT",
    "HLS-VARIANT-ASSOCIATION",
    "HLS-VERSION-MINIMUM",
    "HLS-VERSION-VALUE",
    "MANIFEST-GRAPH-VALID",
    "MANIFEST-RESOURCE-LIMIT",
];

const NATIVE_MESSAGES: &[&str] = &[
    "APPLE-HLS-AUDIO-DESCRIPTION",
    "APPLE-HLS-AVERAGE-BANDWIDTH",
    "APPLE-HLS-HD-VARIANTS",
    "APPLE-HLS-IFRAME-AVERAGE-BANDWIDTH",
    "APPLE-HLS-LANGUAGE",
    "APPLE-HLS-MEDIA-ALIGNMENT",
    "APPLE-HLS-VARIANT-ATTRIBUTES",
    "APPLE-HLS-VARIANTS",
    "APPLE-HLS-VIDEO-CONDITIONS",
    "APPLE-HLS-VOD",
    "DASH-ADDRESSING-COVERAGE",
    "DASH-ATTRIBUTE-COVERAGE",
    "DASH-ATTRIBUTE-LEXICAL",
    "DASH-CENC-DEFAULT-KID",
    "DASH-CENC-PSSH-BOX",
    "DASH-DESCRIPTOR-SCHEME",
    "DASH-DRM-PAYLOAD",
    "DASH-ELEMENT-PLACEMENT",
    "DASH-EXECUTION-LIMIT",
    "DASH-FEATURE-COVERAGE",
    "DASH-FULL-XSD-SCHEMATRON",
    "DASH-MEDIA-EVIDENCE",
    "DASH-MPD-MIN-BUFFER-TIME",
    "DASH-MPD-PERIOD",
    "DASH-MPD-PROFILES",
    "DASH-MPD-ROOT",
    "DASH-PERIOD-TIMING",
    "DASH-PRESENTATION-REPRESENTATIONS",
    "DASH-PROFILE-STATIC",
    "DASH-PROTECTION-BASE64",
    "DASH-PROTECTION-PLACEMENT",
    "DASH-REPRESENTATION-ATTRIBUTES",
    "DASH-REPRESENTATION-IDENTITY",
    "DASH-SEGMENT-ADDRESSING",
    "DASH-TEMPLATE-INTEGERS",
    "DASH-TEMPLATE-TIMESCALE",
    "DASH-TEMPLATE-TOKENS",
    "DASH-TIMELINE-ARITHMETIC",
    "DASH-TIMELINE-ENTRIES",
    "DASH-XML-ENCODING",
    "DASH-XML-EXTERNAL",
    "DASH-XML-LIMIT",
    "DASH-XML-SYNTAX",
    "HLS-ATTRIBUTE-TYPES",
    "HLS-ATTRIBUTES",
    "HLS-ATTRIBUTES-UNIQUE",
    "HLS-BYTE-RANGE",
    "HLS-CHANNEL-EXTENSION",
    "HLS-CHANNELS",
    "HLS-CLOSED-CAPTIONS",
    "HLS-DATE-RANGE",
    "HLS-DATE-RANGE-CLIENT",
    "HLS-DATE-RANGE-COVERAGE",
    "HLS-DATE-RANGE-IDENTITY",
    "HLS-DATE-RANGE-OVERLAP",
    "HLS-DATE-RANGE-PDT",
    "HLS-DATE-RANGE-SCTE",
    "HLS-DATE-RANGE-TIME",
    "HLS-DATE-TIME",
    "HLS-DELIVERY-EVIDENCE",
    "HLS-EXTINF",
    "HLS-GRAPH-BINDINGS",
    "HLS-GRAPH-COVERAGE",
    "HLS-GRAPH-DATE-RANGES",
    "HLS-GRAPH-INDEPENDENT-DUPLICATE",
    "HLS-GRAPH-PDT-MAPPING",
    "HLS-GRAPH-PDT-PRESENCE",
    "HLS-GRAPH-PLAYLIST-KIND",
    "HLS-GRAPH-PLAYLIST-TYPES",
    "HLS-GRAPH-REFERENCES",
    "HLS-GRAPH-START",
    "HLS-GRAPH-START-DUPLICATE",
    "HLS-GRAPH-TARGET-DURATIONS",
    "HLS-HDCP-LEVEL",
    "HLS-HEADER",
    "HLS-I-FRAME-ATTRIBUTES",
    "HLS-KEY",
    "HLS-KEY-METHOD",
    "HLS-KEY-METHOD-CONSISTENCY",
    "HLS-LIMITS",
    "HLS-MAP",
    "HLS-MAP-KEY-IV",
    "HLS-MAP-RANGE",
    "HLS-MEDIA-EVIDENCE",
    "HLS-NFC",
    "HLS-PARSE-DEPENDENT",
    "HLS-PLAYLIST-KIND",
    "HLS-PLAYLIST-TYPE",
    "HLS-RENDITION",
    "HLS-RENDITION-AUTOSELECT",
    "HLS-RENDITION-DEFAULT",
    "HLS-RENDITION-GROUPS",
    "HLS-RENDITION-NAMES",
    "HLS-RENDITION-REFERENCES",
    "HLS-SCTE-EVIDENCE",
    "HLS-SEGMENT-ASSOCIATION",
    "HLS-SEQUENCE",
    "HLS-SESSION-DATA",
    "HLS-SESSION-DATA-DUPLICATES",
    "HLS-SESSION-KEY",
    "HLS-SESSION-KEY-DUPLICATES",
    "HLS-SESSION-KEY-MATCH",
    "HLS-START",
    "HLS-STATE-COVERAGE",
    "HLS-TAG-CARDINALITY",
    "HLS-TAG-SYNTAX",
    "HLS-TARGET-DURATION",
    "HLS-TEXT",
    "HLS-UNKNOWN-ATTRIBUTES",
    "HLS-UNKNOWN-TAGS",
    "HLS-URI",
    "HLS-UTF8",
    "HLS-VARIANT",
    "HLS-VARIANT-ASSOCIATION",
    "HLS-VERSION-MINIMUM",
    "HLS-VERSION-VALUE",
    "MANIFEST-GRAPH-VALID",
    "MANIFEST-RESOURCE-LIMIT",
    "apple-hls-audio-description-selection-invalid",
    "apple-hls-average-bandwidth-required",
    "apple-hls-finite-vod-required",
    "apple-hls-highest-resolution-variants-recommended",
    "apple-hls-iframe-average-bandwidth-required",
    "apple-hls-media-alignment-not-inspected",
    "apple-hls-multiple-video-variants-required",
    "apple-hls-rendition-language-required",
    "apple-hls-variant-attributes-required",
    "apple-hls-video-conditions-unknown",
    "hls-attribute-list-invalid",
    "hls-attribute-name-duplicated",
    "hls-attribute-value-type-invalid",
    "hls-audio-channel-count-invalid",
    "hls-automatic-renditions-indistinguishable",
    "hls-byte-range-invalid",
    "hls-channel-extension-not-checked",
    "hls-child-date-range-sets-differ",
    "hls-child-playlist-kind-invalid",
    "hls-child-playlist-not-supplied",
    "hls-child-playlist-types-differ",
    "hls-child-target-durations-differ",
    "hls-client-attribute-type-invalid",
    "hls-closed-captions-none-inconsistent",
    "hls-date-range-attribute-conflict",
    "hls-date-range-declaration-invalid",
    "hls-date-range-end-time-invalid",
    "hls-date-range-limit",
    "hls-date-range-program-date-time-required",
    "hls-date-ranges-v1",
    "hls-date-time-invalid",
    "hls-delivery-not-inspected",
    "hls-effective-encryption-methods-differ",
    "hls-encrypted-map-iv-required",
    "hls-encryption-method-not-checked",
    "hls-extension-attribute-not-checked",
    "hls-extension-tag-not-checked",
    "hls-following-date-range-overlap-invalid",
    "hls-foreign-playlist-binding",
    "hls-hdcp-level-invalid",
    "hls-header-invalid",
    "hls-iframe-attribute-not-permitted",
    "hls-independent-segments-duplicated-in-child",
    "hls-key-declaration-invalid",
    "hls-map-byte-range-invalid",
    "hls-map-byte-range-requires-context",
    "hls-map-declaration-invalid",
    "hls-master-media-tags-mixed",
    "hls-media-bytes-not-inspected",
    "hls-playlist-reference-limit",
    "hls-playlist-type-invalid",
    "hls-program-date-time-media-mapping-not-inspected",
    "hls-program-date-time-presence-differs",
    "hls-protocol-version-too-low",
    "hls-rendition-declaration-invalid",
    "hls-rendition-default-duplicated",
    "hls-rendition-group-reference-invalid",
    "hls-rendition-groups-inconsistent",
    "hls-rendition-name-duplicated",
    "hls-resource-limit",
    "hls-rules-require-valid-text",
    "hls-scte35-hexadecimal-invalid",
    "hls-scte35-payload-not-inspected",
    "hls-segment-duration-association-invalid",
    "hls-segment-duration-invalid",
    "hls-sequence-declaration-invalid",
    "hls-session-data-declaration-invalid",
    "hls-session-data-duplicated",
    "hls-session-key-declaration-invalid",
    "hls-session-key-duplicated",
    "hls-session-key-matching-invalid",
    "hls-singleton-tag-duplicated",
    "hls-start-declaration-invalid",
    "hls-start-duplicated-in-child",
    "hls-start-values-differ",
    "hls-state-rules-resource-limited",
    "hls-tag-value-unexpected",
    "hls-target-duration-invalid",
    "hls-text-layout-invalid",
    "hls-text-not-normalized",
    "hls-uri-invalid",
    "hls-utf8-invalid",
    "hls-variant-declaration-invalid",
    "hls-variant-playlist-association-invalid",
    "hls-version-invalid",
    "manifest-graph-invalid",
    "manifest-only",
    "manifest-resource-limit",
];

const NATIVE_URLS: &[&str] = &[
    "https://conformance.dashif.org/faq",
    "https://dashif.org/Guidelines-TimingModel/",
    "https://developer.apple.com/streaming/hls-authoring-specification-for-apple-devices.html",
    "https://github.com/Dash-Industry-Forum/DASH-IF-Conformance/blob/v3.1.0/resources/xsd/DASH5th.xsd",
    "https://github.com/kazuhide/drmr/tree/main/crates/drmr-manifest#evidence-and-profiles",
    "https://github.com/kazuhide/drmr/tree/main/crates/drmr-manifest#resource-limits",
    "https://learn.microsoft.com/en-us/playready/specifications/mpeg-dash-playready",
    "https://www.rfc-editor.org/rfc/rfc8216.html",
    "https://www.w3.org/TR/2026/NOTE-eme-initdata-cenc-20260604/",
    "https://www.w3.org/TR/REC-xml/",
    "https://www.w3.org/TR/xmlschema11-2/#base64Binary",
];

const NATIVE_SECTIONS: &[&str] = &[
    "1 Format; 4 Common SystemID and PSSH Box Format",
    "2 Documents",
    "2.1.3 Including Track Encryption Box Fields in the MPD; 2.2.5 KID Byte Order",
    "2.13,2.27",
    "3",
    "3.3.17 base64Binary",
    "4.1",
    "4.2",
    "4.3",
    "4.3.1.1",
    "4.3.1.2",
    "4.3.2.1",
    "4.3.2.2",
    "4.3.2.4",
    "4.3.2.5",
    "4.3.2.6-4.3.2.7",
    "4.3.2.7",
    "4.3.2.7.1",
    "4.3.3.1",
    "4.3.3.2-4.3.3.3",
    "4.3.3.5",
    "4.3.4",
    "4.3.4.1",
    "4.3.4.1.1",
    "4.3.4.2",
    "4.3.4.2-4.3.4.3",
    "4.3.4.3",
    "4.3.4.4",
    "4.3.4.5",
    "4.3.5",
    "4.3.5.2",
    "6",
    "6.2.3",
    "6.2.4",
    "7",
    "8 Period timing; 9.1 Sample timeline; 18.3 Explicit addressing (ordinary timing descriptions)",
    "8.1,8.3,8.7",
    "8.10",
    "8.6",
    "9.1-9.4,9.15",
    "9.10",
    "9.14",
    "9.2,9.9,9.15",
    "9.9",
    "MPDtype, RepresentationBaseType, RepresentationType, SegmentTemplateType, SegmentTimelineType",
    "Native unambiguous representation identity policy",
    "Supported attribute declarations and simple types",
    "Validation steps",
    "resource-limits",
];

const NATIVE_REVISIONS: &[&str] = &[
    "2026-10-21.1",
    "Commit Snapshot 2024-10-24",
    "DASH-IF v3.1.0 fifth-edition schema",
    "Microsoft specification v1.3 2018-02-15",
    "RFC 8216 (2017-08)",
    "W3C Group Note 2026-06-04",
    "W3C Recommendation 2012-04-05",
    "XML 1.0 fifth edition",
    "retrieved 2026-10-21",
];

const FIXED_INSPECTION_TOKENS: &[&str] = &[
    "ac-3",
    "aes-128-cbc",
    "aes-128-ctr",
    "cenc-hls-output-not-supported",
    "convert-one-output-format-per-invocation",
    "dash-invalid-xml",
    "dash-no-representations",
    "dash-presentation",
    "dash-unknown-protection-scheme",
    "dash-unrecognized-content-protection",
    "dash-unsupported-namespace",
    "dash-unsupported-protection-location",
    "dash-unsupported-root",
    "decrypt-input",
    "ec-3",
    "encrypt-output",
    "finite-progressive-profile-must-pass",
    "fragmented-mp4",
    "fragmented-output-profile-and-presentation-windows-must-pass",
    "hls-aes128",
    "hls-aes128-requires-hls-output",
    "hls-ambiguous-playlist",
    "hls-encrypted-map-iv-missing",
    "hls-invalid-attributes",
    "hls-invalid-bandwidth",
    "hls-invalid-duration",
    "hls-invalid-header",
    "hls-invalid-key-format",
    "hls-invalid-line",
    "hls-invalid-media-type",
    "hls-map-uri-missing",
    "hls-no-media-declarations",
    "hls-partial-segments-unverified",
    "hls-playlist-limit",
    "hls-protection-unresolved",
    "hls-reference-cycle",
    "hls-reference-limit",
    "hls-reference-missing",
    "hls-reference-outside-root",
    "hls-reference-unreadable",
    "hls-remote-reference",
    "hls-sample-aes",
    "hls-sample-aes-input-key-and-iv-required",
    "hls-sample-aes-requires-hls-ts-output",
    "hls-segment-missing",
    "hls-segment-missing-duration",
    "hls-session-key-not-media-evidence",
    "hls-total-size-limit",
    "hls-ts",
    "hls-ts-output-requires-transport-stream-input",
    "hls-ts-source-protection-not-supported",
    "hls-unknown-key-declaration",
    "input-not-regular-file",
    "input-unreadable",
    "invalid-codec-label",
    "invalid-key-id",
    "invalid-mp4-box",
    "invalid-mp4-metadata",
    "invalid-pssh",
    "kebab-case",
    "local-mp4-or-fragmented-manifest-profile-must-pass",
    "local-transport-stream-profile-and-absolute-timeline-must-pass",
    "local-transport-stream-profile-must-pass",
    "manifest-depth-limit",
    "manifest-invalid-utf8",
    "manifest-key-limit",
    "manifest-line-limit",
    "manifest-protection-limit",
    "manifest-size-limit",
    "manifest-track-limit",
    "media-samples-not-verified",
    "media-validation-required",
    "mpeg-ts-dash",
    "mpeg-ts-hls",
    "mpeg-ts-hls-ts",
    "mpeg-ts-sample-aes-hls",
    "multiple-mp4-movie-boxes",
    "no-supported-processing-route",
    "output-format-has-no-processing-route",
    "progressive-mp4",
    "progressive-mp4-profile-validation-required",
    "remote-input-requires-policy",
    "repackage-media",
    "sample-aes-transport-profile-must-pass",
    "sample-auxiliary-data-not-inspected",
    "sample-encryption-container-unverified",
    "sample-groups-not-verified",
    "shaka-packager",
    "source-profile-validation-required",
    "source-protection-not-supported",
    "source-protection-unresolved",
    "target-intent-not-sample-evidence",
    "unrecognized-input",
    "unrecognized-or-incomplete-sample-protection",
    "validate-source",
    "verify-output",
    "write-manifests",
    "xml-doctype-forbidden",
];

const FAILURE_CODES: &[&str] = &[
    "audio-presentation-edit-not-supported",
    "cancelled",
    "cenc-hls-output-not-supported",
    "dash-invalid-xml",
    "dash-unsupported-root",
    "decoded-presentation-range-unavailable",
    "download-cleanup-unconfirmed",
    "download-destination-unsafe",
    "download-directory-exists",
    "download-graph-limit",
    "download-output-overlap",
    "download-source-changed",
    "download-source-not-supported",
    "download-source-unavailable",
    "download-url-invalid",
    "duplicate-key-id",
    "execution-plan-destination-unsafe",
    "execution-plan-drm-mismatch",
    "execution-plan-exists",
    "execution-plan-invalid",
    "execution-plan-not-supported",
    "execution-plan-output-keys-mismatch",
    "execution-plan-source-changed",
    "fetch-policy-invalid",
    "finite-audio-tail-not-supported",
    "fragment-metadata-size-limit",
    "fragment-presentation-edit-not-supported",
    "fragment-presentation-window-incomplete",
    "fragment-timeline-gap-or-overlap",
    "fragment-timing-not-supported",
    "hls-ad-cue-not-supported",
    "hls-ambiguous-playlist",
    "hls-audio-association-not-supported",
    "hls-byte-ranges-not-yet-supported",
    "hls-complete-vod-required",
    "hls-data-after-endlist",
    "hls-duplicate-header",
    "hls-encrypted-map-explicit-iv-required",
    "hls-input-key-required",
    "hls-inspection-failed",
    "hls-invalid-attributes",
    "hls-invalid-bandwidth",
    "hls-invalid-duration",
    "hls-invalid-header",
    "hls-invalid-key-format",
    "hls-invalid-line",
    "hls-invalid-media-sequence",
    "hls-invalid-media-type",
    "hls-invalid-target-duration",
    "hls-invalid-utf8",
    "hls-key-format-not-supported",
    "hls-key-iv-conflict",
    "hls-key-method-not-supported",
    "hls-key-uri-missing",
    "hls-key-uri-required",
    "hls-manifest-unreadable-or-too-large",
    "hls-map-changes-not-supported",
    "hls-map-required",
    "hls-map-uri-missing",
    "hls-master-feature-not-supported",
    "hls-master-not-yet-supported",
    "hls-media-sequence-overflow",
    "hls-media-uri-required",
    "hls-multiplexed-media-not-supported",
    "hls-nested-protection-not-supported",
    "hls-playlist-limit",
    "hls-presentation-feature-not-supported",
    "hls-protocol-version-too-low",
    "hls-reference-limit",
    "hls-reference-missing",
    "hls-reference-unreadable",
    "hls-rendition-metadata-not-supported",
    "hls-resource-count-limit",
    "hls-resource-not-confined-local",
    "hls-resource-unreadable",
    "hls-sample-explicit-iv-required",
    "hls-sample-output-iv-required",
    "hls-sample-protection-unresolved",
    "hls-segment-duration-missing",
    "hls-segment-missing",
    "hls-tag-not-supported",
    "hls-total-size-limit",
    "hls-unexpected-master-resource",
    "hls-variant-metadata-not-supported",
    "hls-variant-uri-required",
    "hls-version-not-supported",
    "inband-events-not-supported",
    "input-format-not-supported",
    "input-inspection-failed",
    "input-key-required",
    "input-not-regular-file",
    "input-not-regular-or-too-large",
    "input-protection-not-supported",
    "input-unreadable",
    "invalid-aes128-key",
    "invalid-avc-syntax",
    "invalid-content-key",
    "invalid-encrypted-segment",
    "invalid-fetch-policy",
    "invalid-hls-key-uri",
    "invalid-input-media",
    "invalid-key-id",
    "invalid-localized-hls-media",
    "invalid-max-workers",
    "invalid-media-probe",
    "invalid-mp4-box",
    "invalid-mp4-metadata",
    "invalid-or-duplicate-key-uri",
    "invalid-output-key-uri",
    "invalid-output-media",
    "invalid-packaged-resource",
    "invalid-packet-evidence",
    "invalid-segment-iv",
    "invalid-ts-access-unit-evidence",
    "invalid-ts-streams",
    "invalid-verification-result",
    "key-count-limit",
    "key-file-invalid",
    "manifest-changed-during-validation",
    "manifest-conformance-failed",
    "manifest-depth-limit",
    "manifest-fragment-timeline-mismatch",
    "manifest-invalid-utf8",
    "manifest-key-limit",
    "manifest-line-limit",
    "manifest-lint-configuration-invalid",
    "manifest-lint-profile-unsupported",
    "manifest-protection-limit",
    "manifest-segment-duration-mismatch",
    "manifest-size-limit",
    "manifest-track-limit",
    "manifest-unreadable",
    "manifest-validation-feature-unsupported",
    "manifest-validation-format-unsupported",
    "manifest-validation-graph-limit",
    "manifest-validation-hls-invalid",
    "manifest-validation-reference-unsupported",
    "manifest-validation-workspace-changed",
    "manifest-validation-workspace-failed",
    "manifest-validator-cleanup-unconfirmed",
    "manifest-validator-failed",
    "manifest-validator-not-configured",
    "manifest-validator-output-limit",
    "manifest-validator-report-invalid",
    "manifest-validator-timeout",
    "manifest-validator-unavailable",
    "manifest-validator-version-changed",
    "media-copy-failed",
    "media-metadata-unreadable",
    "media-packets-missing",
    "media-size-limit",
    "media-tool-failed",
    "media-tool-timeout",
    "media-track-not-supported",
    "mp4-box-count-limit",
    "mp4-initialization-missing",
    "mp4-metadata-size-limit",
    "mp4-requires-one-complete-presentation",
    "mp4-sample-descriptions-missing",
    "multi-track-mp4-output-not-supported",
    "multiple-mp4-movie-boxes",
    "native-aac-clock-mismatch",
    "native-aac-config-invalid",
    "native-aac-config-not-supported",
    "native-avc-access-unit-size-limit",
    "native-avc-sync-sample-mismatch",
    "native-cipher-counter-overflow",
    "native-codec-configuration-not-supported",
    "native-codec-profile-not-supported",
    "native-manifest-lint-failed",
    "native-media-clock-overflow",
    "native-media-duration-mismatch",
    "native-mp4-invalid",
    "native-mp4-profile-not-supported",
    "native-presentation-metadata-invalid",
    "native-presentation-window-not-supported",
    "native-sample-evidence-incomplete",
    "native-sample-evidence-invalid",
    "native-sample-protection-mismatch",
    "native-source-changed",
    "native-track-changed",
    "native-video-dimensions-mismatch",
    "output-exists-or-publish-failed",
    "output-exists-or-unsafe",
    "output-iv-not-used",
    "output-key-id-mismatch",
    "output-key-not-used",
    "output-key-required",
    "output-key-role-mismatch",
    "output-parent-unavailable",
    "output-parent-unsafe",
    "output-path-invalid",
    "output-protection-mismatch",
    "output-signaling-invalid",
    "output-signaling-private-file-invalid",
    "output-signaling-profile-conflict",
    "output-signaling-profile-not-supported",
    "output-signaling-verification-failed",
    "output-spec-invalid",
    "output-staging-failed",
    "packaged-hls-empty",
    "packaged-hls-map-missing",
    "packaged-hls-playlist-missing",
    "packaged-resource-unreadable",
    "packaged-segment-count-limit",
    "packaged-segments-missing",
    "packet-evidence-count-limit",
    "packet-timing-unavailable",
    "planned-source-changed",
    "presentation-metadata-mismatch",
    "presentation-selection-not-representable",
    "presentation-track-content-mismatch",
    "presentation-track-count-mismatch",
    "presentation-track-role-mismatch",
    "presentation-window-not-supported",
    "presentation-window-outside-media",
    "private-file-permissions-failed",
    "private-files-not-supported-on-platform",
    "probe-result-size-limit",
    "progressive-cbcs-profile-not-supported",
    "progressive-cenc-profile-not-supported",
    "progressive-mp4-profile-not-supported",
    "remote-input-requires-policy",
    "resource-budget",
    "round-trip-verification-failed",
    "sample-groups-not-supported",
    "secure-random-unavailable",
    "segment-cipher-invalid",
    "segment-duration-limit",
    "segment-encryption-failed",
    "segment-read-failed",
    "segment-write-failed",
    "single-track-mp4-required",
    "source-decode-comparison-failed",
    "source-decryption-verification-failed",
    "source-graph-limit",
    "source-inventory-invalid",
    "source-media-verification-failed",
    "source-normalization-verification-failed",
    "stream-metadata-unavailable",
    "stream-selector-mismatch",
    "synthetic-failure",
    "target-requires-hls-output",
    "target-requires-ts-output",
    "temporary-cleanup-unconfirmed",
    "temporary-file-unavailable",
    "temporary-key-write-failed",
    "temporary-workspace-parent-unsafe",
    "temporary-workspace-unavailable",
    "track-count-limit",
    "track-role-mismatch",
    "ts-aac-mp4-invalid",
    "ts-aac-mp4-write-failed",
    "ts-aac-sample-rate-not-supported",
    "ts-access-unit-count-mismatch",
    "ts-access-unit-size-limit",
    "ts-adaptation-metadata-not-supported",
    "ts-audio-only-timing-proof-not-supported",
    "ts-avc-picture-duration-not-supported",
    "ts-avc-separate-colour-plane-not-supported",
    "ts-avc-slice-groups-not-supported",
    "ts-avc-slice-order-not-supported",
    "ts-clock-timescale-not-supported",
    "ts-clock-wrap-or-discontinuity-not-supported",
    "ts-codec-configuration-not-supported",
    "ts-compatibility-view-invalid",
    "ts-continuity-mismatch",
    "ts-decoded-sample-count-mismatch",
    "ts-encrypted-pmt-must-fit-one-packet",
    "ts-input-requires-generic-hls-output",
    "ts-mixed-protection-not-supported",
    "ts-nal-type-not-supported",
    "ts-original-access-unit-count-mismatch",
    "ts-original-evidence-invalid",
    "ts-original-evidence-mismatch",
    "ts-original-evidence-required",
    "ts-original-pending-limit",
    "ts-output-map-not-supported",
    "ts-output-profile-not-supported",
    "ts-pes-incomplete-at-segment-boundary",
    "ts-pes-metadata-not-supported",
    "ts-pes-size-limit",
    "ts-pes-timestamps-required",
    "ts-presentation-layout-not-supported",
    "ts-program-descriptors-not-supported",
    "ts-program-or-configuration-changed",
    "ts-progressive-frame-evidence-required",
    "ts-sample-description-not-supported",
    "ts-sample-encryption-invalid",
    "ts-sample-key-declaration-not-supported",
    "ts-sample-pending-limit",
    "ts-sample-projection-invalid",
    "ts-sample-size-limit",
    "ts-section-crc-mismatch",
    "ts-segment-duration-mismatch",
    "ts-segment-evidence-incomplete",
    "ts-segment-evidence-limit",
    "ts-segment-evidence-missing",
    "ts-segment-evidence-unfinished",
    "ts-single-program-required",
    "ts-stream-count-mismatch",
    "ts-stream-descriptors-not-supported",
    "ts-stream-type-not-supported",
    "ts-transport-not-supported",
    "ts-undeclared-stream-not-supported",
    "ts-video-duration-clock-required",
    "ts-video-reorder-limit",
    "ts-wall-clock-metadata-not-supported",
    "unrecognized-input",
    "unsafe-local-path",
    "unsupported-path",
    "verification-evidence-scope-mismatch",
    "verification-mismatch",
    "verification-native-evidence-required",
    "verification-output-changed",
    "xml-doctype-forbidden",
];

#[cfg(test)]
mod tests {
    use super::*;
    fn negative_bundle() -> Value {
        let reference = json!({"url":"https://conformance.dashif.org/faq","section":"Validation steps","revision":"retrieved 2026-10-21"});
        let coverage = json!({"document":0,"rule_id":"DASH-FULL-XSD-SCHEMATRON","scope":"outside-scope","severity":"error","status":{"status":"not-checked","reason":"unsupported-feature"},"passed_checks":0,"failed_checks":0,"not_checked_checks":1,"not_checked_reasons":["unsupported-feature"],"reference":reference});
        let native = json!({"schema_version":1,"scope":"manifest-only","engine_revision":env!("CARGO_PKG_VERSION"),"profile":"dash-static-manifest","profile_revision":"2026-10-21.1","errors":0,"warnings":0,"truncated":false,"incomplete":false,"document_count":1,"workers_used":1,"diagnostics":[],"coverage":[coverage]});
        let checks = json!({"stream_metadata":"fail","packet_payloads_equal":false,"packet_timing_equal":true,"decoded_content_equal":false});
        let verification = json!({"schema_version":1,"passed":false,"media_verified":false,"license_playback_verified":false,"checks":checks,"native_manifest":native});
        let output = json!({"id":"video","output_matches_plan":false,"verification":verification,"signaling":{"systems":[],"verified":false},"conformance":null});
        json!({"kind":"bundle-verification","schema_version":1,"plan_id":"a".repeat(64),"source_unchanged":true,"passed":false,"outputs":[output],"command":"verify","status":"failed","error":{"code":"verification-mismatch","hint":"PRIVATE_TOOL_OUTPUT_MARKER","last_activity":"verification"}})
    }
    #[test]
    fn negative_bundle_keeps_each_check_and_not_checked_scope_without_echoing_tool_text() {
        let input = negative_bundle();
        let p = project(
            OperationName::Verify,
            Some(2),
            Some(&"a".repeat(64)),
            &input,
        )
        .unwrap();
        assert!(!p.success);
        assert_eq!(p.kind, "verification");
        assert_eq!(p.value["schema_version"], 1);
        for field in ["source_unchanged", "passed", "outputs"] {
            assert_eq!(p.value[field], input[field]);
        }
        assert!(!p.value.to_string().contains("PRIVATE_TOOL_OUTPUT_MARKER"));
        let mut wrong_version = input.clone();
        wrong_version["schema_version"] = json!(2);
        assert!(project(OperationName::Verify, Some(2), None, &wrong_version).is_err());
        let mut wrong_plan = input.clone();
        wrong_plan["plan_id"] = json!("b".repeat(64));
        assert!(
            project(
                OperationName::Verify,
                Some(2),
                Some(&"a".repeat(64)),
                &wrong_plan
            )
            .is_err()
        );
    }
    #[test]
    fn actual_hls_native_report_keeps_fixed_reference_sections_and_refuses_private_mutations() {
        use drmr_core::{
            conversion::Converter,
            events::NoSink,
            toolrun::{Secrets, ToolRunner},
        };
        use std::sync::Arc;
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("media.m3u8");
        std::fs::write(&path, "#EXTM3U\n#EXT-X-VERSION:6\n#EXT-X-TARGETDURATION:2\n#EXT-X-MAP:URI=\"absent-init.mp4\"\n#EXTINF:2.0,\nabsent-segment.m4s\n#EXT-X-ENDLIST\n").unwrap();
        let converter = Converter::new(
            Arc::new(ToolRunner::new(Arc::new(Secrets::new()), Arc::new(NoSink))),
            "unused-packager",
            "unused-ffmpeg",
        );
        let report = converter.lint_manifest(&path, OutputFormat::Hls).unwrap();
        assert!(report.is_clean_for_scope());
        let input = serde_json::to_value(&report).unwrap();
        let projected = native(&input).unwrap();
        assert_eq!(projected, input);
        let mut unknown = input.clone();
        unknown["coverage"][0]["reference"]["private_uri"] = json!("PRIVATE_CONFIG_MARKER");
        assert!(native(&unknown).is_err());
        let mut foreign_section = input.clone();
        foreign_section["coverage"][0]["reference"]["section"] = json!("PRIVATE_CONFIG_MARKER");
        assert!(native(&foreign_section).is_err());
    }
    #[test]
    fn actual_inspection_failure_stays_a_failed_report_without_private_hint_passthrough() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("damaged.mpd");
        std::fs::write(&path, b"<MPD").unwrap();
        let cause = drmr_core::inspection::inspect(&path).unwrap_err();
        assert_eq!(cause.0, "dash-invalid-xml");
        let payload = json!({"schema_version":1,"command":"verify","status":"failed",
            "error":{"code":cause.0,"hint":"PRIVATE_TOOL_PATH_MARKER","last_activity":"configuration"}});
        let projected = project(
            OperationName::Verify,
            Some(2),
            Some(&"a".repeat(64)),
            &payload,
        )
        .unwrap();
        assert!(!projected.success);
        assert_eq!(projected.kind, "failure");
        assert_eq!(projected.value["error"]["code"], cause.0);
        assert_eq!(projected.value["status"], "failed");
        assert!(
            !projected
                .value
                .to_string()
                .contains("PRIVATE_TOOL_PATH_MARKER")
        );
        let mut unknown = payload.clone();
        unknown["error"]["code"] = json!("PRIVATE_UNKNOWN_MARKER");
        assert!(project(OperationName::Verify, Some(2), None, &unknown).is_err());
    }
    #[test]
    fn unknown_nested_fields_and_foreign_normative_links_never_become_raw_value_fallbacks() {
        let mut input = negative_bundle();
        input["outputs"][0]["verification"]["native_manifest"]["coverage"][0]["reference"]["url"] =
            json!("https://keys.example.invalid/private?secret=MARKER");
        assert!(project(OperationName::Verify, Some(2), None, &input).is_err());
        let mut input = negative_bundle();
        input["outputs"][0]["verification"]["checks"]["raw_output"] = json!("MARKER");
        assert!(project(OperationName::Verify, Some(2), None, &input).is_err());
        let mut input = negative_bundle();
        input["error"]["code"] = json!("unknown-private-error-MARKER");
        assert!(project(OperationName::Verify, Some(2), None, &input).is_err());
    }
}
