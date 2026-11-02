//! Explain discovery separately from bounded version and advertised-feature checks.

use drmr_core::tool_diagnostics::{DiagnosticSession, Profile};
use std::{collections::BTreeSet, io::Write as _, path::Path, process::ExitCode};

pub fn run(
    as_json: bool,
    probe: bool,
    decode: bool,
    route: &str,
    outputs: Option<&Path>,
) -> ExitCode {
    let progress = std::sync::Arc::new(crate::progress::Progress::new(!as_json));
    progress.mark("configuration");
    let configuration = match outputs
        .map(drmr_core::conversion::plan::bundle::read_output_config_diagnostic)
        .transpose()
    {
        Ok(value) => value,
        Err(error) => {
            return crate::convert_cli::error("doctor", error, as_json, &progress, Some(2));
        }
    };
    progress.mark("tool-check");
    let cancellation = match cancellation(probe, &progress) {
        Ok(value) => value,
        Err(code) => return code,
    };
    let mut session = DiagnosticSession::new(
        if configuration.is_some() {
            Profile::ALL
        } else {
            Profile::parse(route).expect("validated route")
        },
        probe,
    )
    .with_validation_depth(if decode {
        drmr_core::conversion::ValidationDepth::Decoded
    } else {
        drmr_core::conversion::ValidationDepth::Native
    })
    .with_activity_sink(progress.clone());
    if let Some(cancellation) = &cancellation {
        session = session.with_stop(cancellation.stop.clone());
    }
    let mut report = session
        .run()
        .expect("single diagnostic caller")
        .as_ref()
        .clone();
    let native_ready = report.ready;
    let external = configuration.as_ref().map(|configuration| {
        let (hls, dash) = external_requirements(configuration);
        session.manifest_dependencies(hls, dash)
    });
    if let Some(external) = &external {
        report.ready &= external.ready;
        report.cancelled |= external.cancelled;
        report.profile = "output-config";
    }
    // Do not print before ownership recovery: even a closed stdout can panic.
    session.wait_for_cleanup(|| {
        progress.mark("tool-check");
        // A closed diagnostic stream must not panic and discard the owner.
        if !progress.structured() {
            let _ = progress.write_result(|| writeln!(std::io::stderr().lock(), "doctor: cleanup is unconfirmed; retaining the tool owner and retrying. Ctrl+C requests Stop but does not abandon cleanup."));
        }
    });
    if probe {
        // Preserve the original failed probe and ready=false, but describe the
        // actual cleanup state at publication rather than its historical state.
        report.cleanup_status = "confirmed";
    }
    let value = report_value(&report, configuration.as_ref(), external.as_ref());
    if progress
        .write_result(|| {
            crate::reporting::record(&value)?;
            let mut out = std::io::stdout().lock();
            if as_json {
                serde_json::to_writer(&mut out, &value).map_err(std::io::Error::other)?;
                writeln!(out)
            } else {
                let mut media_tools = report.clone();
                media_tools.ready = native_ready;
                print_report(&media_tools, false, probe, &mut out)?;
                if let Some(external) = &external {
                    print_manifest_dependencies(external, &mut out)?;
                }
                Ok(())
            }
        })
        .is_err()
    {
        progress.diagnostic("doctor", "result-output-unavailable");
        return ExitCode::from(1);
    }
    progress.finish();
    ExitCode::from(u8::from(!report.ready))
}

fn cancellation(
    probe: bool,
    progress: &crate::progress::Progress,
) -> Result<Option<crate::cancellation::Cancellation>, ExitCode> {
    if !probe {
        return Ok(None);
    }
    {
        let Ok(cancellation) = crate::cancellation::Cancellation::install() else {
            progress.fail("signal-monitor-unavailable");
            if progress
                .write_result(|| {
                    if !progress.structured() {
                        writeln!(
                            std::io::stderr().lock(),
                            "doctor: signal-monitor-unavailable"
                        )?;
                    }
                    Ok(())
                })
                .is_err()
            {
                progress.diagnostic("doctor", "result-output-unavailable");
            }
            return Err(ExitCode::from(1));
        };
        Ok(Some(cancellation))
    }
}

fn report_value(
    report: &drmr_core::tool_diagnostics::DiagnosticReport,
    configuration: Option<&drmr_core::conversion::plan::bundle::OutputConfig>,
    external: Option<&drmr_core::tool_diagnostics::ManifestDependencyReport>,
) -> serde_json::Value {
    let mut value = serde_json::to_value(report).expect("diagnostic report serializes");
    if let Some(configuration) = configuration {
        let formats: BTreeSet<_> = configuration
            .outputs
            .iter()
            .map(|output| match output.format {
                drmr_core::inspection::OutputFormat::Hls => "hls-fmp4",
                value => value.as_str(),
            })
            .collect();
        value["scope"] = "conservative-output-dependencies".into();
        value["source_profile_checked"] = false.into();
        value["output_formats"] = serde_json::json!(formats);
        value["external_manifest_dependencies"] =
            serde_json::to_value(external).expect("external diagnostics serialize");
    }
    value
}

fn print_manifest_dependencies(
    external: &drmr_core::tool_diagnostics::ManifestDependencyReport,
    out: &mut impl std::io::Write,
) -> std::io::Result<()> {
    writeln!(
        out,
        "Input media was not inspected; packaging requirements are conservative."
    )?;
    for tool in &external.tools {
        writeln!(
            out,
            "  {} {}: {}; features {}",
            if tool.available { "FOUND" } else { "MISSING" },
            tool.name,
            tool.purpose,
            tool.feature_status
        )?;
        if let Some(code) = tool.failure_code {
            writeln!(out, "    diagnostic: {code}")?;
        }
    }
    if !external.ready {
        writeln!(
            out,
            "Install the external manifest-check tools and assets required by the output configuration."
        )?;
    }
    if let Some(available) = external.dash_assets_available {
        writeln!(
            out,
            "  {} Installed DASH-IF schema and Schematron assets",
            if available { "FOUND" } else { "MISSING" }
        )?;
    }
    Ok(())
}

fn external_requirements(
    configuration: &drmr_core::conversion::plan::bundle::OutputConfig,
) -> (bool, bool) {
    use drmr_core::{conversion::plan::bundle::ManifestValidation, inspection::OutputFormat};
    let requested = |format| {
        configuration.outputs.iter().any(|output| {
            output.format == format && output.conformance == ManifestValidation::Required
        })
    };
    (
        requested(OutputFormat::Hls) || requested(OutputFormat::HlsTs),
        requested(OutputFormat::Dash),
    )
}

fn print_report(
    report: &drmr_core::tool_diagnostics::DiagnosticReport,
    as_json: bool,
    probe: bool,
    out: &mut impl std::io::Write,
) -> std::io::Result<()> {
    if as_json {
        serde_json::to_writer(&mut *out, report).map_err(std::io::Error::other)?;
        return writeln!(out);
    }
    writeln!(
        out,
        "External media tools (current launch PATH; route {}):",
        report.profile
    )?;
    for tool in &report.tools {
        let status = if !tool.available {
            "MISSING"
        } else if probe
            && (tool.failure_code.is_some()
                || tool.version_status == "below-minimum"
                || tool.feature_status == "missing")
        {
            "CHECK"
        } else {
            "FOUND"
        };
        writeln!(
            out,
            "  {status:7} {:8} {} ({})",
            tool.name,
            tool.purpose,
            if tool.required {
                "required"
            } else {
                "optional for this route"
            }
        )?;
        if let Some(path) = &tool.path {
            writeln!(
                out,
                "    path: {}",
                serde_json::to_string(path).expect("path")
            )?;
        }
        if probe && tool.available {
            writeln!(
                out,
                "    version: {} [{}]; minimum: {}",
                tool.version.as_deref().unwrap_or("unknown"),
                tool.version_status,
                tool.minimum_version
            )?;
            writeln!(out, "    advertised features: {}", tool.feature_status)?;
            if let Some(code) = tool.failure_code {
                writeln!(out, "    diagnostic: {code}")?;
            }
        }
    }
    if probe {
        writeln!(
            out,
            "\nProbes check version banners and advertised options. They do not process media."
        )?;
        writeln!(out, "Cleanup: {}", report.cleanup_status)?;
    } else {
        writeln!(
            out,
            "\nDiscovery does not run tools or verify versions. Use 'drmr doctor --probe' to check versions and advertised options."
        )?;
    }
    if !report.ready {
        writeln!(
            out,
            "Install compatible FFmpeg or Shaka Packager tools required for this route, and add their directories to the launcher's PATH."
        )?;
        writeln!(
            out,
            "Run 'drmr doctor --probe' again; media compatibility still requires conversion checks."
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn only_requested_manifest_checks_add_their_actual_engine_dependencies() {
        for (format, requested, expected) in [
            ("hls-fmp4", false, (false, false)),
            ("hls-fmp4", true, (true, false)),
            ("hls-ts", true, (true, false)),
            ("dash", true, (false, true)),
        ] {
            let value = serde_json::json!({"schema_version": 4, "outputs": [{
                "id": "result", "output_format": format, "encryption": "clear", "drms": [],
                "external_manifest_check": requested
            }]});
            let configuration =
                drmr_core::conversion::plan::bundle::parse_output_config_diagnostic(
                    &serde_json::to_vec(&value).unwrap(),
                )
                .unwrap();
            assert_eq!(super::external_requirements(&configuration), expected);
        }
    }
}
