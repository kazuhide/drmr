//! Generic media commands with protected operator key files and secret-free summaries.

use clap::Args;
use drmr_core::conversion::{
    ConversionError, ConversionReport, ConversionRequest, Converter, VerificationReport,
    VerificationRequest, VerificationSource,
    key_file::{self, OperatorKeys},
    measurement::MeasurementReport,
};
pub(crate) mod feedback;
use drmr_core::inspection::{self, OutputFormat, TargetProtection};
use drmr_core::toolrun::{Secrets, ToolRunner};
pub(crate) use feedback::Progress;
use std::io::Write;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

#[derive(Args)]
pub struct ConvertArgs {
    /// Local source media or a supported HLS, static DASH, or MP4 URL. The source is never overwritten.
    #[arg(required_unless_present = "plan", conflicts_with = "plan")]
    pub input: Option<PathBuf>,
    /// Execute the exact source and media intent in a saved plan; keys stay separate.
    #[arg(long, conflicts_with_all = ["input", "download_dir", "fetch_policy", "to", "format", "output", "outputs"])]
    pub plan: Option<PathBuf>,
    /// New directory retaining the original input; required for URL input.
    #[arg(long)]
    pub download_dir: Option<PathBuf>,
    /// Private JSON settings for additional origins, authentication and fetch limits.
    #[arg(long)]
    pub fetch_policy: Option<PathBuf>,
    /// Target protection: clear, cenc, cbcs, hls-aes128, or hls-sample-aes.
    #[arg(long, required_unless_present_any = ["plan", "outputs"], conflicts_with = "outputs")]
    pub to: Option<TargetProtection>,
    /// Output format: mp4, dash, hls (fragmented MP4), or hls-ts.
    #[arg(long)]
    pub format: Option<OutputFormat>,
    /// JSON output list; publish all verified results together in one new directory.
    #[arg(long, conflicts_with_all = ["plan", "to", "format"])]
    pub outputs: Option<PathBuf>,
    /// A new output file or directory; existing destinations are refused.
    #[arg(long, required_unless_present = "plan")]
    pub output: Option<PathBuf>,
    /// Protected JSON configuration with distinct input and output keys.
    #[arg(long)]
    pub keys_file: Option<PathBuf>,
    /// Shaka Packager executable, selected by the operator.
    #[arg(long, default_value = "packager")]
    pub packager_bin: String,
    /// Upper bound for simultaneous media work (1-64); default adapts to CPU and memory.
    #[arg(long, value_parser = clap::value_parser!(u8).range(1..=64))]
    pub max_workers: Option<u8>,
    /// Apple mediastreamvalidator executable for requested final HLS manifest checks.
    #[arg(long, default_value = "mediastreamvalidator")]
    pub hls_validator_bin: String,
    /// PHP executable for requested local DASH-IF manifest checks.
    #[arg(long, default_value = "php")]
    pub dash_validator_bin: String,
    /// Print the versioned result as one JSON object.
    #[arg(long)]
    pub json: bool,
    /// Include numeric measurements in JSON; does not change media verification.
    #[arg(long, requires = "json")]
    pub measure: bool,
}

#[derive(Args)]
pub struct VerifyArgs {
    /// Result media to compare with the source.
    #[arg(required_unless_present = "plan", conflicts_with = "plan")]
    pub output: Option<PathBuf>,
    /// Verify saved-plan source fingerprints, output intent, and complete media correspondence.
    #[arg(long, conflicts_with_all = ["output", "source"])]
    pub plan: Option<PathBuf>,
    #[command(flatten)]
    pub manifest: ManifestArgs,
    /// Original source media. Required to verify content correspondence.
    #[arg(long, required_unless_present_any = ["plan", "manifest_only"])]
    pub source: Option<PathBuf>,
    /// Protected JSON configuration: input keys for source, output keys for result.
    #[arg(long)]
    pub keys_file: Option<PathBuf>,
    /// Shaka Packager executable, selected by the operator.
    #[arg(long, default_value = "packager")]
    pub packager_bin: String,
    /// Upper bound for simultaneous media or native manifest work (1-64); default adapts to CPU and memory.
    #[arg(long, value_parser = clap::value_parser!(u8).range(1..=64))]
    pub max_workers: Option<u8>,
    /// Print the versioned verification result as one JSON object.
    #[arg(long)]
    pub json: bool,
    /// Include numeric measurements in JSON; does not change media verification.
    #[arg(long, requires = "json")]
    pub measure: bool,
}

#[derive(Args)]
pub struct ManifestArgs {
    /// Check final HLS/DASH manifests with native rules, without reading media or keys.
    #[arg(long, conflicts_with_all = ["plan", "source", "keys_file"])]
    pub manifest_only: bool,
    /// Add the selected external validator to native manifest-only checks.
    #[arg(long, requires = "manifest_only", conflicts_with_all = ["source", "keys_file", "plan"])]
    pub external_validator: bool,
    /// Native manifest profile (default: base); apple-vod adds HLS authoring-guide rules.
    #[arg(long, value_enum, requires = "manifest_only", conflicts_with_all = ["source", "keys_file", "plan"])]
    pub manifest_profile: Option<drmr_core::conversion::manifest_lint::ManifestProfile>,
    /// External HLS executable; defaults to mediastreamvalidator when requested.
    #[arg(long)]
    pub hls_validator_bin: Option<String>,
    /// External DASH PHP executable; defaults to php when requested.
    #[arg(long)]
    pub dash_validator_bin: Option<String>,
}

pub(crate) fn load(path: Option<&PathBuf>) -> Result<OperatorKeys, ConversionError> {
    path.map_or_else(|| Ok(OperatorKeys::default()), |path| key_file::read(path))
}

pub(crate) fn load_source(path: Option<&PathBuf>) -> Result<OperatorKeys, ConversionError> {
    path.map_or_else(
        || Ok(OperatorKeys::default()),
        |path| key_file::read_source(path),
    )
}

pub(crate) fn converter(
    packager: &str,
    maximum: Option<u8>,
    progress: Arc<Progress>,
    stop: drmr_core::concurrency::Stop,
) -> Converter {
    let runner = Arc::new(ToolRunner::new(Arc::new(Secrets::new()), progress.clone()));
    Converter::new(runner, packager, "ffmpeg")
        .with_activity_sink(progress)
        .with_stop(stop)
        .with_max_workers(maximum.map(usize::from))
        .expect("clap validates the worker upper bound")
}

fn manifest_error_hint(code: &str) -> Option<&'static str> {
    match code {
        "external-validator-required" => Some(
            "Use --external-validator with --manifest-only to request an external engine; executable paths only configure that request.",
        ),
        "manifest-lint-profile-unsupported" => Some(
            "Use --manifest-profile base for DASH. The apple-vod profile applies only to HLS manifest-only checks.",
        ),
        "native-manifest-lint-failed" => Some(
            "Review native_manifest diagnostics and incomplete coverage. Native checks establish only their named manifest scope; media and external checks remain separate.",
        ),
        _ => None,
    }
}

fn error_hint(code: &str) -> Option<&'static str> {
    match code {
        "key-file-invalid" => Some(
            "Use an owner-only regular JSON file: schema version 1 for one output, or version 2 for named outputs.",
        ),
        "cancelled" => Some(
            "Check the destination and job status before retrying; cancellation is not a media verification result.",
        ),
        "verification-mismatch" => {
            Some("Review every source/result check. Do not use the result as verified media.")
        }
        "media-tool-unavailable-or-timeout" | "media-tool-timeout" => Some(
            "Check external tool discovery and source compatibility, and review the configured media timeout.",
        ),
        "media-cleanup-unconfirmed" => Some(
            "Cleanup is unconfirmed. Resolve the owned process failure before retrying; no verified result is established.",
        ),
        "output-signaling-profile-not-supported" => Some(
            "Use the documented input, output and DRM-signaling combination. Unsupported existing or unknown metadata is refused. See docs/conversion-support.md.",
        ),
        "download-directory-required" => Some(
            "URL input requires --download-dir pointing to a new directory under an existing parent.",
        ),
        "download-options-require-url" => {
            Some("Use --download-dir and --fetch-policy only with URL input.")
        }
        "download-output-overlap" => {
            Some("Choose separate download and output paths; neither may contain the other.")
        }
        "download-directory-exists" => Some(
            "Choose a new --download-dir, or convert the retained input.m3u8, input.mpd, or input.mp4 as local input.",
        ),
        "download-destination-unsafe" => Some(
            "Choose new download and output paths whose immediate parent directories already exist.",
        ),
        "fetch-policy-invalid" => {
            Some("Use an owner-only JSON object with supported, correctly typed fetch settings.")
        }
        "download-source-unavailable" => Some(
            "Check network access, allowed origins, authentication, and fetch limits in --fetch-policy.",
        ),
        "download-source-not-supported" => Some(
            "URL input requires supported completed HLS, single-Period static DASH using SegmentTemplate with a finite SegmentTimeline, or self-contained MP4. Supply input keys separately; key and license URIs are never fetched. See docs/conversion-support.md.",
        ),
        "download-source-changed" => Some(
            "The retained source changed or became unavailable. Inspect it and choose a new download directory before retrying the URL.",
        ),
        "download-graph-limit" => Some(
            "The source references exceed the bounded URL acquisition profile. See docs/media-processing.md.",
        ),
        "download-cleanup-unconfirmed" => Some(
            "Partial source cleanup could not be confirmed. Inspect the chosen download directory before retrying.",
        ),
        "resource-budget" => Some(
            "Free memory or run fewer jobs; the worker maximum does not override memory admission.",
        ),
        "media-output-limit" => {
            Some("The tool output exceeded the capture limit. Partial evidence is never accepted.")
        }
        "input-key-required" => Some(
            "Provide every source key in input.keys; use input.hls_keys for HLS AES-128 or SAMPLE-AES.",
        ),
        "output-key-required" => Some("Provide output.kid and output.key in --keys-file."),
        "output-key-role-mismatch" => Some(
            "For cenc/cbcs, provide output.kid and output.key. For HLS AES-128 or SAMPLE-AES, omit the KID and provide output.key plus output.hls_key_uri; SAMPLE-AES also needs output.iv. For clear output, omit output.",
        ),
        "hls-key-uri-required" => {
            Some("Provide output.hls_key_uri for CBCS HLS playlist signaling.")
        }
        "cenc-hls-output-not-supported" => Some("Use --format dash or --format mp4 for cenc."),
        "output-key-not-used" => {
            Some("For clear output, omit the output section from --keys-file.")
        }
        "target-requires-hls-output" => {
            Some("Use --format hls or --format hls-ts for HLS AES-128.")
        }
        "single-track-mp4-required" | "mp4-output-requires-one-track" => {
            Some("Use DASH or HLS output for separate video and audio tracks.")
        }
        "finite-audio-tail-not-supported" => Some(
            "This source has a finite audio trim that the backend cannot preserve. See docs/media-processing.md.",
        ),
        "output-exists-or-unsafe" | "output-exists-or-publish-failed" => {
            Some("Choose a new output path under an existing directory.")
        }
        "temporary-workspace-parent-unsafe" => Some(
            "Choose an output or temporary parent directory owned by your user that other users cannot modify. See docs/security.md.",
        ),
        "packet-timing-unavailable" => Some(
            "Use an FFmpeg build that supplies complete packet timestamps and durations. See docs/media-processing.md.",
        ),
        _ => manifest_error_hint(code)
            .or_else(|| crate::bundle_cli::error_hint(code))
            .or_else(|| crate::plan_cli::error_hint(code)),
    }
}

pub(crate) fn error_value(
    command: &str,
    error: ConversionError,
    progress: &Progress,
) -> serde_json::Value {
    serde_json::json!({
        "schema_version": 1, "command": command,
        "status": if error.0 == "cancelled" { "cancelled" } else { "failed" },
        "error": {"code": error.0, "hint": error_hint(error.0).unwrap_or("Review the error code and source profile before retrying with a new output destination."), "last_activity": progress.last_activity()}
    })
}
pub(crate) fn error(
    command: &str,
    error: ConversionError,
    as_json: bool,
    progress: &Progress,
    exit_override: Option<u8>,
) -> ExitCode {
    progress.finish();
    eprintln!("{command}: {error}");
    if as_json {
        if json(&error_value(command, error, progress)).is_err() {
            eprintln!("{command}: result-output-unavailable");
            return ExitCode::from(1);
        }
    } else if let Some(hint) = error_hint(error.0) {
        eprintln!("{hint}");
    }
    if error.0 == "cancelled" {
        return ExitCode::from(130);
    }
    ExitCode::from(exit_override.unwrap_or({
        if matches!(
            error.0,
            "key-file-invalid"
                | "external-validator-required"
                | "manifest-lint-profile-unsupported"
                | "execution-plan-invalid"
                | "execution-plan-exists"
                | "execution-plan-destination-unsafe"
                | "temporary-workspace-parent-unsafe"
                | "plan-requires-single-output"
                | "download-directory-required"
                | "download-options-require-url"
                | "download-output-overlap"
                | "download-directory-exists"
                | "download-destination-unsafe"
                | "download-url-invalid"
                | "download-url-scheme-not-supported"
                | "fetch-policy-invalid"
                | "input-key-required"
                | "output-key-required"
                | "output-key-not-used"
                | "output-key-role-mismatch"
                | "hls-key-uri-required"
                | "cenc-hls-output-not-supported"
                | "multi-track-mp4-output-not-supported"
                | "target-requires-hls-output"
                | "input-format-not-supported"
                | "single-track-mp4-required"
                | "input-protection-not-supported"
                | "output-signaling-profile-not-supported"
                | "output-spec-invalid"
                | "execution-plan-output-keys-mismatch"
                | "execution-plan-drm-mismatch"
        ) {
            2
        } else {
            1
        }
    }))
}
pub(crate) struct ConfigurationError {
    pub(crate) cause: ConversionError,
    pub(crate) exit: Option<u8>,
}
impl From<ConversionError> for ConfigurationError {
    fn from(cause: ConversionError) -> Self {
        Self { cause, exit: None }
    }
}

pub(crate) fn json(value: &serde_json::Value) -> std::io::Result<()> {
    if let Some(result) = crate::common_job::result(value) {
        return result;
    }
    let mut out = std::io::stdout().lock();
    serde_json::to_writer_pretty(&mut out, value)?;
    writeln!(out)
}

pub(crate) fn configured_download(
    args: &ConvertArgs,
    input: &crate::download_cli::Input,
) -> Result<Option<drmr_core::source::FetchPolicy>, ConfigurationError> {
    match input {
        crate::download_cli::Input::Local(_) => {
            if args.download_dir.is_some() || args.fetch_policy.is_some() {
                Err(ConversionError("download-options-require-url").into())
            } else {
                Ok(None)
            }
        }
        crate::download_cli::Input::Url(url) => {
            let directory = args
                .download_dir
                .as_ref()
                .ok_or(ConversionError("download-directory-required"))?;
            crate::download_cli::preflight(
                directory,
                args.output
                    .as_deref()
                    .expect("clap requires a direct output"),
            )
            .map_err(|cause| ConfigurationError {
                cause,
                exit: Some(2),
            })?;
            drmr_core::conversion::download_policy::read(url, args.fetch_policy.as_deref())
                .map(Some)
                .map_err(Into::into)
        }
    }
}

fn execute_conversion(
    command: &Converter,
    args: &ConvertArgs,
    input: &crate::download_cli::Input,
    fetch_policy: Option<&drmr_core::source::FetchPolicy>,
    local_format: Option<OutputFormat>,
    keys: &OperatorKeys,
    progress: &Progress,
) -> Result<(ConversionReport, Option<serde_json::Value>), ConversionError> {
    let downloaded = match input {
        crate::download_cli::Input::Url(url) => {
            progress.mark("acquisition");
            let source = command.download_source(
                url,
                args.download_dir
                    .as_deref()
                    .expect("URL download directory was checked"),
                args.output
                    .as_deref()
                    .expect("clap requires a direct output"),
                fetch_policy.expect("URL fetch policy was checked"),
            )?;
            eprintln!("Downloaded source retained in the requested download directory.");
            Some(source)
        }
        crate::download_cli::Input::Local(_) => None,
    };
    let source = downloaded.as_ref().map_or_else(
        || match input {
            crate::download_cli::Input::Local(path) => path.as_path(),
            crate::download_cli::Input::Url(_) => unreachable!("URL acquisition returned a source"),
        },
        |source| source.entrypoint(),
    );
    let format = if let Some(format) = local_format {
        format
    } else {
        progress.mark("inspection");
        inspection::inspect(source)
            .map_err(|_| ConversionError("invalid-input-media"))?
            .format
            .into()
    };
    progress.mark("input-preparation");
    let request = ConversionRequest {
        input: source,
        output: args
            .output
            .as_deref()
            .expect("clap requires a direct output"),
        format,
        target: args.to.expect("clap requires a direct target"),
        input_keys: &keys.input,
        hls_input_keys: &keys.hls_input,
        output_key: keys.output.as_ref(),
        hls_key_uri: keys.output.as_ref().and_then(|_| keys.hls_output_uri()),
        hls_output_key: keys.hls_output.as_ref(),
    };
    let report = match &downloaded {
        Some(source) => command.convert_downloaded(source, request),
        None => command.convert(request),
    }?;
    let download = downloaded.as_ref().map(|source| {
        serde_json::json!({
            "directory": source.directory().to_string_lossy(),
            "entrypoint": source.entrypoint().to_string_lossy(),
            "entrypoint_relative": source.entrypoint_relative().to_string_lossy(),
            "retained": true
        })
    });
    Ok((report, download))
}

fn configured_format(
    args: &ConvertArgs,
    input: &crate::download_cli::Input,
    progress: &Progress,
) -> Result<Option<OutputFormat>, ConversionError> {
    match (input, args.format) {
        (_, Some(format)) => Ok(Some(format)),
        (crate::download_cli::Input::Local(path), None) => {
            progress.mark("inspection");
            inspection::inspect(path)
                .map(|report| Some(report.format.into()))
                .map_err(|_| ConversionError("invalid-input-media"))
        }
        (crate::download_cli::Input::Url(_), None) => Ok(None),
    }
}

pub(crate) fn conversion_text(report: &ConversionReport) -> std::io::Result<()> {
    let mut out = std::io::stdout().lock();
    writeln!(
        out,
        "Converted: {} -> {}",
        report.input.protection.as_str(),
        report.output.protection.as_str()
    )
    .and_then(|()| {
        if let Some(processing) = &report.processing {
            writeln!(out, "Processing route: {}", processing.route.as_str())?;
            writeln!(
                out,
                "Input preparation: {}",
                processing.input_preparation.description()
            )?;
            writeln!(out, "Reason: {}", processing.explanation)?;
            let tools = processing
                .required_tools
                .iter()
                .map(|tool| tool.as_str())
                .collect::<Vec<_>>();
            writeln!(out, "Required tools: {}", tools.join(", "))?;
        }
        Ok(())
    })
    .and_then(|()| {
        writeln!(
            out,
            "Verified stream metadata, packet payloads, decoded content, and packet timing."
        )
    })
}

pub fn convert(args: &ConvertArgs) -> ExitCode {
    if let Some(path) = &args.plan {
        return crate::plan_cli::convert(args, path);
    }
    if args.outputs.is_some() {
        return crate::bundle_cli::convert_direct(args);
    }
    let progress = Arc::new(Progress::new(!args.json));
    progress.mark("configuration");
    let input = match crate::download_cli::Input::parse(
        args.input
            .as_deref()
            .expect("clap requires a direct input")
            .as_os_str(),
    ) {
        Ok(input) => input,
        Err(cause) => return error("convert", cause, args.json, &progress, None),
    };
    let fetch_policy = match configured_download(args, &input) {
        Ok(policy) => policy,
        Err(cause) => return error("convert", cause.cause, args.json, &progress, cause.exit),
    };
    let Ok(cancellation) = crate::cancellation::Cancellation::install() else {
        return error(
            "convert",
            ConversionError("signal-monitor-unavailable"),
            args.json,
            &progress,
            None,
        );
    };
    let keys = match load(args.keys_file.as_ref()) {
        Ok(keys) => keys,
        Err(cause) => return error("convert", cause, args.json, &progress, None),
    };
    let local_format = match configured_format(args, &input, &progress) {
        Ok(value) => value,
        Err(cause) => return error("convert", cause, args.json, &progress, None),
    };
    let converter = converter(
        &args.packager_bin,
        args.max_workers,
        progress.clone(),
        cancellation.stop.clone(),
    )
    .with_output_signaling(keys.signaling.clone());
    let execute = |command: &Converter| {
        execute_conversion(
            command,
            args,
            &input,
            fetch_policy.as_ref(),
            local_format,
            &keys,
            &progress,
        )
    };
    let (result, measurement) = if args.measure {
        let (result, measurement) = converter.measure(execute);
        (result, Some(measurement))
    } else {
        (execute(&converter), None)
    };
    let (report, download) = match result {
        Ok(report) => report,
        Err(cause) => return error("convert", cause, args.json, &progress, None),
    };
    if cancellation.stop.is_set() {
        return error(
            "convert",
            ConversionError("cancelled"),
            args.json,
            &progress,
            None,
        );
    }
    progress.finish();
    let result = if args.json {
        let mut value = serde_json::to_value(&report).expect("conversion report serializes");
        if let Some(download) = download {
            value["download"] = download;
        }
        if let Some(measurement) = measurement {
            value["measurements"] =
                serde_json::to_value(measurement).expect("measurement serializes");
        }
        json(&value)
    } else {
        conversion_text(&report)
    };
    if result.is_err() {
        eprintln!("convert: result-output-unavailable");
    }
    ExitCode::from(u8::from(result.is_err()))
}

pub fn verify(args: &VerifyArgs) -> ExitCode {
    if args.manifest.manifest_only {
        return crate::bundle_cli::verify_manifest(args);
    }
    if let Some(path) = &args.plan {
        return crate::plan_cli::verify(args, path);
    }
    let progress = Arc::new(Progress::new(!args.json));
    progress.mark("configuration");
    let Ok(cancellation) = crate::cancellation::Cancellation::install() else {
        return error(
            "verify",
            ConversionError("signal-monitor-unavailable"),
            args.json,
            &progress,
            None,
        );
    };
    let keys = match load(args.keys_file.as_ref()) {
        Ok(keys) => keys,
        Err(cause) => return error("verify", cause, args.json, &progress, None),
    };
    progress.mark("input-preparation");
    let converter = converter(
        &args.packager_bin,
        args.max_workers,
        progress.clone(),
        cancellation.stop.clone(),
    );
    let execute =
        |command: &Converter| verify_current_output(command, args, &keys, &cancellation.stop);
    let (result, measurement) = if args.measure {
        let (result, measurement) = converter.measure(execute);
        (result, Some(measurement))
    } else {
        (execute(&converter), None)
    };
    let report = match result {
        Ok(report) => report,
        Err(cause) => return error("verify", cause, args.json, &progress, None),
    };
    if cancellation.stop.is_set() {
        return error(
            "verify",
            ConversionError("cancelled"),
            args.json,
            &progress,
            None,
        );
    }
    progress.finish();
    verification_result(&report, args.json, measurement, &progress)
}

fn verify_current_output(
    command: &Converter,
    args: &VerifyArgs,
    keys: &OperatorKeys,
    stop: &drmr_core::concurrency::Stop,
) -> Result<VerificationReport, ConversionError> {
    let output_keys = keys.output_verification_keys()?;
    let output_hls_keys = keys.output_hls_verification_keys()?;
    let output = args
        .output
        .as_deref()
        .expect("clap requires a direct output");
    let frozen = keys
        .signaling
        .as_ref()
        .map(|_| drmr_core::conversion::FrozenSource::capture(output, stop))
        .transpose()?;
    let checked_output = frozen
        .as_ref()
        .map_or(output, drmr_core::conversion::FrozenSource::entrypoint);
    let report = command.verify(VerificationRequest {
        source: VerificationSource {
            input: args
                .source
                .as_deref()
                .expect("clap requires a direct source"),
            input_keys: &keys.input,
            hls_keys: &keys.hls_input,
        },
        output: VerificationSource {
            input: checked_output,
            input_keys: &output_keys,
            hls_keys: &output_hls_keys,
        },
    })?;
    if let Some(frozen) = &frozen {
        let format = if keys.hls_output.is_some() {
            OutputFormat::HlsTs
        } else {
            inspection::inspect(checked_output)
                .map_err(|_| ConversionError("invalid-input-media"))?
                .format
                .into()
        };
        let target = if format == OutputFormat::HlsTs {
            TargetProtection::HlsSampleAes
        } else {
            TargetProtection::Cbcs
        };
        command.verify_output_signaling(checked_output, format, target, keys)?;
        frozen.check_original().map_err(|cause| {
            if cause.0 == "cancelled" {
                cause
            } else {
                ConversionError("verification-output-changed")
            }
        })?;
    }
    Ok(report)
}

fn verification_result(
    report: &VerificationReport,
    as_json: bool,
    measurement: Option<MeasurementReport>,
    progress: &Progress,
) -> ExitCode {
    if !report.passed {
        eprintln!("verify: verification-mismatch");
    }
    let result = if as_json {
        let mut value = serde_json::to_value(report).expect("verification report serializes");
        if !report.passed {
            let failure = error_value("verify", ConversionError("verification-mismatch"), progress);
            for key in ["command", "status", "error"] {
                value[key] = failure[key].clone();
            }
        }
        if let Some(measurement) = measurement {
            value["measurements"] =
                serde_json::to_value(measurement).expect("measurement serializes");
        }
        json(&value)
    } else {
        writeln!(
            std::io::stdout().lock(),
            "Verification: {}",
            if report.passed { "passed" } else { "failed" }
        )
    };
    if result.is_err() {
        eprintln!("verify: result-output-unavailable");
    }
    ExitCode::from(u8::from(result.is_err() || !report.passed))
}
