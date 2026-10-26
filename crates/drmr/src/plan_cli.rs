//! Saved plans bind local media and intent; runtime secrets are loaded independently.
use crate::convert_cli::{self, ConvertArgs, Progress, VerifyArgs};
use crate::inspect_cli::PlanArgs;
use drmr_core::conversion::{
    ConversionError, Converter, ProcessingMode, VerificationSource,
    dash_addressing::DashAddressing,
    key_file::OperatorKeys,
    plan::{self, ExecutionPlanRequest},
};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;

pub(crate) fn error_hint(code: &str) -> Option<&'static str> {
    match code {
        "execution-plan-invalid" => Some(
            "Use a supported, owner-only saved execution plan. Metadata-only plan reports cannot be executed.",
        ),
        "execution-plan-exists" => {
            Some("Choose a new plan file; existing entries are never replaced.")
        }
        "execution-plan-destination-unsafe" => Some(
            "Keep the new plan file, retained source, and result at separate safe paths under trusted existing parent directories. See docs/security.md.",
        ),
        "execution-plan-source-changed" => Some(
            "The saved source no longer matches the plan. Restore the original bytes or analyze the new source and create a new plan.",
        ),
        "execution-plan-not-supported" => Some(
            "The actual source or output does not satisfy this plan. Review the supported media profile and create a new plan when changing intent.",
        ),
        "plan-requires-single-output" => {
            Some("Use one --format with --to, or select named outputs with --outputs instead.")
        }
        _ => None,
    }
}

struct Session {
    progress: Arc<Progress>,
    cancellation: crate::cancellation::Cancellation,
    keys: OperatorKeys,
    converter: Converter,
}

impl Session {
    fn start(
        command: &str,
        as_json: bool,
        packager: &str,
        maximum: Option<u8>,
        decode: bool,
        keys_file: Option<&PathBuf>,
    ) -> Result<Self, ExitCode> {
        let progress = Arc::new(Progress::new(!as_json));
        progress.mark("configuration");
        let cancellation = crate::cancellation::Cancellation::install().map_err(|_| {
            convert_cli::error(
                command,
                ConversionError("signal-monitor-unavailable"),
                as_json,
                &progress,
                None,
            )
        })?;
        let keys = convert_cli::load(keys_file)
            .map_err(|cause| convert_cli::error(command, cause, as_json, &progress, None))?;
        let converter = convert_cli::converter(
            packager,
            maximum,
            decode,
            progress.clone(),
            cancellation.stop.clone(),
        );
        Ok(Self {
            progress,
            cancellation,
            keys,
            converter,
        })
    }

    fn failed(&self, command: &str, cause: ConversionError, as_json: bool) -> ExitCode {
        convert_cli::error(command, cause, as_json, &self.progress, None)
    }

    fn with_new_intent(
        mut self,
        processing: ProcessingMode,
        addressing: Option<DashAddressing>,
    ) -> Self {
        self.converter =
            crate::output_cli::configure(self.converter, addressing).with_processing(processing);
        self
    }

    fn finish(&self, command: &str, as_json: bool) -> Result<(), ExitCode> {
        if self.cancellation.stop.is_set() {
            return Err(self.failed(command, ConversionError("cancelled"), as_json));
        }
        self.progress.finish();
        Ok(())
    }
}

pub fn save(args: &PlanArgs) -> ExitCode {
    if args.outputs.is_some() {
        return crate::bundle_cli::save(args);
    }
    let mut formats = args.format.clone();
    formats.sort();
    formats.dedup();
    if formats.len() > 1 {
        let progress = Progress::new(!args.json);
        progress.mark("configuration");
        return convert_cli::error(
            "plan",
            ConversionError("plan-requires-single-output"),
            args.json,
            &progress,
            None,
        );
    }
    let session = match Session::start(
        "plan",
        args.json,
        args.packager_bin.as_deref().unwrap_or("packager"),
        args.max_workers,
        args.decode,
        args.keys_file.as_ref(),
    ) {
        Ok(value) => {
            value.with_new_intent(args.processing.unwrap_or_default(), args.dash_addressing)
        }
        Err(code) => return code,
    };
    let input = args.input.as_path();
    let output = args
        .output
        .as_deref()
        .expect("clap requires the saved-plan output");
    let path = args.save.as_deref().expect("saved-plan mode was selected");
    let execute = |command: &Converter| {
        if args.dash_addressing.is_some() {
            let format = formats
                .first()
                .copied()
                .map_or_else(|| command.default_output_format(input), Ok)?;
            crate::output_cli::validate(args.dash_addressing, [format])?;
        }
        session.progress.mark("input-preparation");
        let plan = command.create_execution_plan(ExecutionPlanRequest {
            input: VerificationSource {
                input,
                input_keys: &session.keys.input,
                hls_keys: &session.keys.hls_input,
            },
            output,
            format: formats.first().copied(),
            target: args.to.expect("clap requires the saved-plan target"),
        })?;
        if session.cancellation.stop.is_set() {
            return Err(ConversionError("cancelled"));
        }
        session.progress.mark("publication");
        plan::write_new(path, &plan)?;
        Ok(plan)
    };
    let (result, measurement) = if args.measure {
        let (result, measurement) = session.converter.measure(execute);
        (result, Some(measurement))
    } else {
        (execute(&session.converter), None)
    };
    let plan = match result {
        Ok(value) => value,
        Err(cause) => return session.failed("plan", cause, args.json),
    };
    if let Err(code) = session.finish("plan", args.json) {
        return code;
    }
    let result = if args.json {
        let mut value = serde_json::to_value(&plan).expect("execution plan serializes");
        if let Some(measurement) = measurement {
            value["measurements"] =
                serde_json::to_value(measurement).expect("measurement serializes");
        }
        convert_cli::json(&value)
    } else {
        writeln!(
            std::io::stdout().lock(),
            "Execution plan saved.\nTarget: {} ({})\nSource fingerprints and output intent are fixed.\nSupply runtime keys separately with convert --plan.",
            plan.intent.target.as_str(),
            plan.intent.format.as_str(),
        )
    };
    if result.is_err() {
        session
            .progress
            .diagnostic("plan", "result-output-unavailable");
    }
    ExitCode::from(u8::from(result.is_err()))
}

pub fn convert(args: &ConvertArgs, path: &Path) -> ExitCode {
    match plan::read_saved(path) {
        Ok(plan::SavedPlan::Bundle(plan)) => return crate::bundle_cli::convert(args, &plan),
        Ok(plan::SavedPlan::Single(_)) | Err(_) => {}
    }
    let session = match Session::start(
        "convert",
        args.json,
        &args.packager_bin,
        args.max_workers,
        args.decode,
        args.keys_file.as_ref(),
    ) {
        Ok(value) => value,
        Err(code) => return code,
    };
    let plan = match plan::read(path) {
        Ok(value) => value,
        Err(cause) => return session.failed("convert", cause, args.json),
    };
    let execute = |command: &Converter| command.execute_plan(&plan, &session.keys);
    let (result, measurement) = if args.measure {
        let (result, measurement) = session.converter.measure(execute);
        (result, Some(measurement))
    } else {
        (execute(&session.converter), None)
    };
    let report = match result {
        Ok(value) => value,
        Err(cause) => return session.failed("convert", cause, args.json),
    };
    if let Err(code) = session.finish("convert", args.json) {
        return code;
    }
    let result = if args.json {
        let mut value = serde_json::to_value(&report).expect("conversion report serializes");
        value["plan_id"] = plan.plan_id.clone().into();
        if let Some(measurement) = measurement {
            value["measurements"] =
                serde_json::to_value(measurement).expect("measurement serializes");
        }
        convert_cli::json(&value)
    } else {
        convert_cli::conversion_text(&report)
    };
    if result.is_err() {
        session
            .progress
            .diagnostic("convert", "result-output-unavailable");
    }
    ExitCode::from(u8::from(result.is_err()))
}

pub fn verify(args: &VerifyArgs, path: &Path) -> ExitCode {
    match plan::read_saved(path) {
        Ok(plan::SavedPlan::Bundle(plan)) => return crate::bundle_cli::verify(args, &plan),
        Ok(plan::SavedPlan::Single(_)) | Err(_) => {}
    }
    let session = match Session::start(
        "verify",
        args.json,
        &args.packager_bin,
        args.max_workers,
        args.decode,
        args.keys_file.as_ref(),
    ) {
        Ok(value) => value,
        Err(code) => return code,
    };
    let plan = match plan::read(path) {
        Ok(value) => value,
        Err(cause) => return session.failed("verify", cause, args.json),
    };
    let execute = |command: &Converter| command.verify_plan(&plan, &session.keys);
    let (result, measurement) = if args.measure {
        let (result, measurement) = session.converter.measure(execute);
        (result, Some(measurement))
    } else {
        (execute(&session.converter), None)
    };
    let report = match result {
        Ok(value) => value,
        Err(cause) => return session.failed("verify", cause, args.json),
    };
    if let Err(code) = session.finish("verify", args.json) {
        return code;
    }
    let passed =
        report.source_unchanged && report.output_matches_plan && report.verification.passed;
    if !passed {
        session
            .progress
            .diagnostic("verify", "verification-mismatch");
    }
    let result = if args.json {
        let mut value = serde_json::to_value(report).expect("plan verification serializes");
        if !passed {
            let failure = convert_cli::error_value(
                "verify",
                ConversionError("verification-mismatch"),
                &session.progress,
            );
            for key in ["command", "status", "error"] {
                value[key] = failure[key].clone();
            }
        }
        if let Some(measurement) = measurement {
            value["measurements"] =
                serde_json::to_value(measurement).expect("measurement serializes");
        }
        convert_cli::json(&value)
    } else {
        writeln!(
            std::io::stdout().lock(),
            "Plan verification: {} (source fingerprints, output intent, and media correspondence).",
            if passed { "passed" } else { "failed" }
        )
    };
    if result.is_err() {
        session
            .progress
            .diagnostic("verify", "result-output-unavailable");
    }
    ExitCode::from(u8::from(result.is_err() || !passed))
}
