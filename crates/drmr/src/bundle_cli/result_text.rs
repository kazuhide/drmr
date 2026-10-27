//! Summarize recorded checks without treating absent or partial evidence as a pass.
use serde_json::Value;
use std::io::{self, Write};

pub(super) fn output(out: &mut impl Write, command: &str, output: &Value) -> io::Result<()> {
    let id = output["id"].as_str().unwrap_or("output");
    let proof = if command == "convert" {
        &output["conversion"]["verification"]
    } else {
        &output["verification"]
    };
    write!(out, "  {id}: Media comparison {}", comparison(proof))?;
    if let Some(report) = proof
        .get("native_manifest")
        .filter(|report| !report.is_null())
    {
        write!(out, "; native manifest checks {}", manifest(report))?;
    }
    write!(out, "; DRM declarations {}", drm(&output["signaling"]))?;
    let decode = &proof["decode_evidence"];
    if proof["required_depth"] == "decoded" || decode["status"] == "content-compared" {
        let status = if comparison(proof) == "passed"
            && decode["status"] == "content-compared"
            && decode["equal"] == true
        {
            "passed"
        } else if decode["equal"] == false || proof["passed"] == false {
            "failed"
        } else {
            "not established"
        };
        write!(out, "; decoded media comparison {status}")?;
    }
    if let Some(report) = output.get("conformance").filter(|report| !report.is_null()) {
        write!(
            out,
            "; external manifest checks {} (manifest only)",
            external(report)
        )?;
    }
    writeln!(out, ".")
}

fn comparison(report: &Value) -> &'static str {
    if report["scope"] != "comparison" {
        return "not established";
    }
    let checks = &report["checks"];
    if report["passed"] == false
        || !report["failure"].is_null()
        || checks["stream_metadata"] == "fail"
        || checks["packet_payloads_equal"] == false
        || checks["packet_timing_equal"] == false
    {
        "failed"
    } else if report["passed"] == true
        && matches!(
            report["required_depth"].as_str(),
            Some("native" | "decoded")
        )
        && checks["stream_metadata"] == "pass"
        && checks["packet_payloads_equal"] == true
        && checks["packet_timing_equal"] == true
    {
        "passed"
    } else {
        "not established"
    }
}

fn manifest(report: &Value) -> &'static str {
    if report["scope"] != "manifest-only" {
        "not established"
    } else if report["errors"].as_u64().is_some_and(|errors| errors > 0) {
        "failed"
    } else if report["truncated"] == true || report["incomplete"] == true {
        "incomplete"
    } else if report["errors"] == 0
        && report["truncated"] == false
        && report["incomplete"] == false
        && report["document_count"]
            .as_u64()
            .is_some_and(|count| count > 0)
    {
        "passed"
    } else {
        "not established"
    }
}

fn drm(report: &Value) -> &'static str {
    match report["systems"].as_array() {
        Some(systems) if systems.is_empty() => "not requested",
        Some(_) if report["verified"] == true => "passed",
        Some(_) if report["verified"] == false => "failed",
        _ => "not established",
    }
}

fn external(report: &Value) -> &'static str {
    if report["passed"] == false {
        return "failed";
    }
    let Some(checks) = report["checks"].as_array() else {
        return "not established";
    };
    if checks.iter().any(|check| check["passed"] == false) {
        return "failed";
    }
    let count = report["manifest_count"].as_u64().filter(|count| *count > 0);
    if report["scope"] == "manifest-only"
        && report["passed"] == true
        && count.is_some()
        && !checks.is_empty()
        && checks.iter().all(|check| {
            check["passed"] == true && check["checked_manifest_count"].as_u64() == count
        })
    {
        "passed"
    } else {
        "not established"
    }
}
