//! Explain discovery separately from bounded version and advertised-feature checks.

use drmr_core::tool_diagnostics::{DiagnosticSession, Profile};
use std::{io::Write as _, process::ExitCode};

pub fn run(as_json: bool, probe: bool, decode: bool, route: &str) -> ExitCode {
    let progress = std::sync::Arc::new(crate::progress::Progress::new(!as_json));
    progress.mark("tool-check");
    let cancellation = if probe {
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
            return ExitCode::from(1);
        };
        Some(cancellation)
    } else {
        None
    };
    let mut session =
        DiagnosticSession::new(Profile::parse(route).expect("validated route"), probe)
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
    if progress
        .write_result(|| print_report(&report, as_json, probe, &mut std::io::stdout().lock()))
        .is_err()
    {
        progress.diagnostic("doctor", "result-output-unavailable");
        return ExitCode::from(1);
    }
    progress.finish();
    ExitCode::from(u8::from(!report.ready))
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
