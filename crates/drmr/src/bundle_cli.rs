//! One retained source and one atomic result directory for explicitly named outputs.
use crate::convert_cli::{self, ConvertArgs, Progress, VerifyArgs};
use crate::download_cli::Input;
use crate::inspect_cli::PlanArgs;
use drmr_core::conversion::{
    ConversionError, Converter, VerificationSource,
    conformance::ManifestValidationTools,
    key_file::{self, BundleOperatorKeys},
    plan::{
        self,
        bundle::{BundlePlan, BundlePlanRequest, read_output_spec},
    },
};
use serde_json::Value;
use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;

pub(crate) fn error_hint(code: &str) -> Option<&'static str> {
    match code {
        "output-spec-invalid" => Some(
            "Use a schema-version-1 output list with unique safe IDs and supported format, protection, DRM systems and manifest-check policy.",
        ),
        "execution-plan-output-keys-mismatch" => Some(
            "Match schema-version-2 output key entries to the encrypted output IDs; omit clear outputs and unused entries.",
        ),
        "execution-plan-drm-mismatch" => Some(
            "Supply the DRM systems selected by the saved plan in each output's runtime key configuration.",
        ),
        "manifest-validator-not-configured" | "manifest-validator-unavailable" => Some(
            "Install the requested manifest validator and configure its executable. DASH also needs DRMR_DASH_IF_ROOT; see docs/manifest-validation.md.",
        ),
        "manifest-validator-timeout" | "manifest-validator-output-limit" => Some(
            "The validator did not return complete evidence within its bounds. No manifest pass is established.",
        ),
        "manifest-validator-cleanup-unconfirmed" => Some(
            "Validator cleanup could not be confirmed. Resolve the owned process failure before retrying.",
        ),
        _ => None,
    }
}

struct Session {
    progress: Arc<Progress>,
    cancellation: crate::cancellation::Cancellation,
    converter: Converter,
    keys: BundleOperatorKeys,
}

impl Session {
    fn start(
        command: &str,
        json: bool,
        packager: &str,
        maximum: Option<u8>,
        keys: Option<&PathBuf>,
    ) -> Result<Self, ExitCode> {
        let progress = Arc::new(Progress::new(!json));
        Self::start_observed(command, json, packager, maximum, keys, progress)
    }

    fn start_observed(
        command: &str,
        json: bool,
        packager: &str,
        maximum: Option<u8>,
        keys: Option<&PathBuf>,
        progress: Arc<Progress>,
    ) -> Result<Self, ExitCode> {
        progress.mark("configuration");
        let cancellation = crate::cancellation::Cancellation::install().map_err(|_| {
            convert_cli::error(
                command,
                ConversionError("signal-monitor-unavailable"),
                json,
                &progress,
                None,
            )
        })?;
        let keys = keys
            .map_or_else(
                || Ok(BundleOperatorKeys::default()),
                |path| key_file::read_bundle(path),
            )
            .map_err(|cause| convert_cli::error(command, cause, json, &progress, None))?;
        let converter = convert_cli::converter(
            packager,
            maximum,
            progress.clone(),
            cancellation.stop.clone(),
        );
        Ok(Self {
            progress,
            cancellation,
            converter,
            keys,
        })
    }

    fn failed(&self, command: &str, error: ConversionError, json: bool) -> ExitCode {
        convert_cli::error(command, error, json, &self.progress, None)
    }

    fn run(
        &self,
        command: &str,
        json: bool,
        measure: bool,
        execute: impl FnOnce(&Converter) -> Result<Value, ConversionError>,
    ) -> ExitCode {
        let (result, measurement) = if measure {
            let (result, measurement) = self.converter.measure(execute);
            (result, Some(measurement))
        } else {
            (execute(&self.converter), None)
        };
        let mut report = match result {
            Ok(report) => report,
            Err(error) => return self.failed(command, error, json),
        };
        if self.cancellation.stop.is_set() {
            return self.failed(command, ConversionError("cancelled"), json);
        }
        self.progress.finish();
        let passed = command != "verify" || report["passed"] == true;
        if !passed {
            self.progress.diagnostic(command, "verification-mismatch");
        }
        if !passed && report["error"].is_null() {
            let error = convert_cli::error_value(
                command,
                ConversionError("verification-mismatch"),
                &self.progress,
            );
            for field in ["command", "status", "error"] {
                report[field] = error[field].clone();
            }
        }
        if let Some(measurement) = measurement {
            report["measurements"] =
                serde_json::to_value(measurement).expect("measurements serialize");
        }
        let result = if json {
            convert_cli::json(&report)
        } else {
            text(command, &report, passed)
        };
        if result.is_err() {
            self.progress
                .diagnostic(command, "result-output-unavailable");
        }
        ExitCode::from(u8::from(!passed || result.is_err()))
    }
}

fn tools(hls: &str, dash: &str) -> ManifestValidationTools {
    ManifestValidationTools {
        mediastreamvalidator: Some(hls.into()),
        dash_manifest_validator: Some(dash.into()),
    }
}

fn verify_tools(args: &VerifyArgs) -> ManifestValidationTools {
    tools(
        args.manifest
            .hls_validator_bin
            .as_deref()
            .unwrap_or("mediastreamvalidator"),
        args.manifest.dash_validator_bin.as_deref().unwrap_or("php"),
    )
}

fn incomplete_native_rules(out: &mut impl std::io::Write, native: &Value) -> std::io::Result<()> {
    const MAXIMUM: usize = 32;
    let Some(coverage) = native["coverage"].as_array() else {
        return Ok(());
    };
    let mut count = 0_usize;
    for item in coverage
        .iter()
        .filter(|item| item["scope"] == "in-scope" && item["status"]["status"] == "not-checked")
    {
        if count < MAXIMUM {
            writeln!(
                out,
                "  {}: not checked ({}; document {}).",
                item["rule_id"].as_str().unwrap_or("manifest-rule"),
                item["status"]["reason"]
                    .as_str()
                    .unwrap_or("unknown-reason"),
                item["document"].as_u64().unwrap_or_default()
            )?;
        }
        count += 1;
    }
    if count > MAXIMUM {
        writeln!(
            out,
            "  {} additional incomplete native rule summaries are available in --json output.",
            count - MAXIMUM
        )?;
    }
    Ok(())
}

fn text(command: &str, value: &Value, passed: bool) -> std::io::Result<()> {
    let mut out = std::io::stdout().lock();
    if value["scope"] == "manifest-only" {
        writeln!(
            out,
            "Manifest validation: {} (manifest only; media correspondence was not checked).",
            if passed { "passed" } else { "failed" }
        )?;
        if let Some(diagnostics) = value["native_manifest"]["diagnostics"].as_array() {
            for issue in diagnostics {
                writeln!(
                    out,
                    "  {}: {} (document {}, line {}).",
                    issue["rule_id"].as_str().unwrap_or("manifest-rule"),
                    issue["message_code"]
                        .as_str()
                        .unwrap_or("manifest-check-failed"),
                    issue["location"]["document"],
                    issue["location"]["line"]
                )?;
            }
        }
        incomplete_native_rules(&mut out, &value["native_manifest"])?;
        if let Some(code) = value["external_error"].as_str() {
            writeln!(out, "External manifest check: {code}.")?;
        }
        return Ok(());
    }
    if command == "plan" {
        writeln!(
            out,
            "Execution plan saved with {} outputs.\nSource fingerprints, output profiles, DRM systems and manifest-check policies are fixed.\nSupply runtime keys separately with convert --plan.",
            value["outputs"].as_array().map_or(0, Vec::len)
        )?;
    } else {
        writeln!(
            out,
            "Multiple-output {command}: {}.",
            if passed { "passed" } else { "failed" }
        )?;
    }
    if let Some(outputs) = value["outputs"].as_array() {
        for output in outputs {
            let id = output["id"].as_str().unwrap_or("output");
            if command == "plan" {
                writeln!(
                    out,
                    "  {id}: {} / {}",
                    output["format"].as_str().unwrap_or("unknown"),
                    output["target"].as_str().unwrap_or("unknown")
                )?;
            } else {
                let checked = if output["conformance"].is_null() {
                    "not requested"
                } else {
                    "passed (manifest only)"
                };
                let drm = if output["signaling"]["verified"] == true {
                    "passed"
                } else {
                    "not requested"
                };
                writeln!(
                    out,
                    "  {id}: DRM declarations {drm}; external manifest checks {checked}."
                )?;
            }
        }
    }
    Ok(())
}

pub fn verify_manifest(args: &VerifyArgs) -> ExitCode {
    let session = match Session::start(
        "verify",
        args.json,
        &args.packager_bin,
        args.max_workers,
        None,
    ) {
        Ok(session) => session,
        Err(code) => return code,
    };
    if !args.manifest.external_validator
        && (args.manifest.hls_validator_bin.is_some() || args.manifest.dash_validator_bin.is_some())
    {
        return session.failed(
            "verify",
            ConversionError("external-validator-required"),
            args.json,
        );
    }
    let validators = verify_tools(args);
    session.run("verify", args.json, args.measure, |converter| {
        let path = args
            .output
            .as_deref()
            .expect("clap requires the final manifest");
        let format = manifest_format(path)?;
        let result = converter.check_manifest_with_profile(
            path,
            format,
            args.manifest.manifest_profile.unwrap_or_default(),
            args.manifest.external_validator.then_some(&validators),
        )?;
        let failure = result.failure();
        let mut value = serde_json::to_value(result).expect("manifest validation serializes");
        value["kind"] = "manifest-validation".into();
        value["command"] = "verify".into();
        value["media_verified"] = false.into();
        value["license_playback_verified"] = false.into();
        if let Some(failure) = failure {
            let error = convert_cli::error_value("verify", failure, &session.progress);
            for field in ["status", "error"] {
                value[field] = error[field].clone();
            }
        }
        Ok(value)
    })
}

fn manifest_format(path: &Path) -> Result<drmr_core::inspection::OutputFormat, ConversionError> {
    use drmr_core::inspection::OutputFormat;
    let invalid = || ConversionError("invalid-input-media");
    if path.to_string_lossy().contains("://")
        || !std::fs::metadata(path).map_err(|_| invalid())?.is_file()
    {
        return Err(invalid());
    }
    // Route by the same bounded prefix as inspection, without its media/graph parsing.
    // The selected conformance parser owns complete manifest admission and limits.
    let mut prefix = [0; 64];
    let count = std::fs::File::open(path)
        .and_then(|mut file| file.read(&mut prefix))
        .map_err(|_| invalid())?;
    let prefix = &prefix[..count];
    let text = prefix.strip_prefix(b"\xef\xbb\xbf").unwrap_or(prefix);
    let text = text.trim_ascii_start();
    if text.starts_with(b"#EXTM3U") {
        Ok(OutputFormat::Hls)
    } else if text.starts_with(b"<") {
        Ok(OutputFormat::Dash)
    } else if prefix.get(4..8).is_some_and(|kind| {
        matches!(
            kind,
            b"ftyp" | b"moov" | b"styp" | b"free" | b"wide" | b"mdat" | b"sidx"
        )
    }) {
        Ok(OutputFormat::Mp4)
    } else {
        Err(invalid())
    }
}

pub fn save(args: &PlanArgs) -> ExitCode {
    let session = match Session::start(
        "plan",
        args.json,
        args.packager_bin.as_deref().unwrap_or("packager"),
        args.max_workers,
        args.keys_file.as_ref(),
    ) {
        Ok(session) => session,
        Err(code) => return code,
    };
    let outputs = match read_output_spec(
        args.outputs
            .as_deref()
            .expect("multiple-output mode was selected"),
    ) {
        Ok(outputs) => outputs,
        Err(error) => return session.failed("plan", error, args.json),
    };
    session.run("plan", args.json, args.measure, |converter| {
        session.progress.mark("input-preparation");
        let plan = converter.create_bundle_plan(BundlePlanRequest {
            input: VerificationSource {
                input: args.input.as_path(),
                input_keys: &session.keys.input,
                hls_keys: &session.keys.hls_input,
            },
            output_root: args.output.as_deref().expect("clap requires the output"),
            outputs: &outputs,
        })?;
        session.progress.mark("publication");
        plan::write_bundle(
            args.save.as_deref().expect("clap requires the plan path"),
            &plan,
        )?;
        Ok(serde_json::to_value(plan).expect("bundle plan serializes"))
    })
}

pub fn convert(args: &ConvertArgs, plan: &BundlePlan) -> ExitCode {
    let session = match Session::start(
        "convert",
        args.json,
        &args.packager_bin,
        args.max_workers,
        args.keys_file.as_ref(),
    ) {
        Ok(session) => session,
        Err(code) => return code,
    };
    let validators = tools(&args.hls_validator_bin, &args.dash_validator_bin);
    session.run("convert", args.json, args.measure, |converter| {
        let report = converter.execute_bundle_plan(plan, &session.keys, &validators)?;
        Ok(serde_json::to_value(report).expect("bundle conversion serializes"))
    })
}

pub fn verify(args: &VerifyArgs, plan: &BundlePlan) -> ExitCode {
    let session = match Session::start(
        "verify",
        args.json,
        &args.packager_bin,
        args.max_workers,
        args.keys_file.as_ref(),
    ) {
        Ok(session) => session,
        Err(code) => return code,
    };
    let validators = verify_tools(args);
    session.run("verify", args.json, args.measure, |converter| {
        let report = converter.verify_bundle_plan(plan, &session.keys, &validators)?;
        Ok(serde_json::to_value(report).expect("bundle verification serializes"))
    })
}

pub fn convert_direct(args: &ConvertArgs) -> ExitCode {
    let progress = Arc::new(Progress::new(!args.json));
    progress.mark("configuration");
    let input = match Input::parse(
        args.input
            .as_deref()
            .expect("clap requires the input")
            .as_os_str(),
    ) {
        Ok(input) => input,
        Err(error) => return convert_cli::error("convert", error, args.json, &progress, None),
    };
    let policy = match convert_cli::configured_download(args, &input) {
        Ok(policy) => policy,
        Err(error) => {
            return convert_cli::error("convert", error.cause, args.json, &progress, error.exit);
        }
    };
    let session = match Session::start_observed(
        "convert",
        args.json,
        &args.packager_bin,
        args.max_workers,
        args.keys_file.as_ref(),
        progress,
    ) {
        Ok(session) => session,
        Err(code) => return code,
    };
    let outputs = match read_output_spec(
        args.outputs
            .as_deref()
            .expect("multiple-output mode was selected"),
    ) {
        Ok(outputs) => outputs,
        Err(error) => return session.failed("convert", error, args.json),
    };
    let validators = tools(&args.hls_validator_bin, &args.dash_validator_bin);
    let output = args.output.as_deref().expect("clap requires the output");
    session.run("convert", args.json, args.measure, |converter| {
        let report = match &input {
            Input::Local(input) => {
                session.progress.mark("input-preparation");
                let plan = converter.create_bundle_plan(BundlePlanRequest {
                    input: VerificationSource {
                        input,
                        input_keys: &session.keys.input,
                        hls_keys: &session.keys.hls_input,
                    },
                    output_root: output,
                    outputs: &outputs,
                })?;
                converter.execute_bundle_plan(&plan, &session.keys, &validators)?
            }
            Input::Url(url) => {
                session.progress.mark("acquisition");
                let downloaded = converter.download_source(
                    url,
                    args.download_dir
                        .as_deref()
                        .expect("download directory checked"),
                    output,
                    policy.as_ref().expect("fetch policy checked"),
                )?;
                if !session.progress.structured() {
                    eprintln!("Downloaded source retained in the requested download directory.");
                }
                converter.convert_bundle_downloaded(
                    &downloaded,
                    &outputs,
                    output,
                    &session.keys,
                    &validators,
                )?
            }
        };
        Ok(serde_json::to_value(report).expect("bundle conversion serializes"))
    })
}
