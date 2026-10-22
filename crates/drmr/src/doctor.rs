//! Explain discovery separately from bounded version and advertised-feature checks.

use drmr_core::tool_diagnostics::{DiagnosticSession, Profile};
use std::{io::Write as _, process::ExitCode};

pub fn run(as_json: bool, probe: bool, route: &str) -> ExitCode {
    let cancellation = if probe {
        let Ok(cancellation) = crate::cancellation::Cancellation::install() else {
            eprintln!("doctor: signal-monitor-unavailable");
            return ExitCode::from(1);
        };
        Some(cancellation)
    } else {
        None
    };
    let mut session =
        DiagnosticSession::new(Profile::parse(route).expect("validated route"), probe);
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
        // A closed diagnostic stream must not panic and discard the owner.
        let _ = writeln!(std::io::stderr().lock(), "doctor: cleanup is unconfirmed; retaining the tool owner and retrying. Ctrl+C requests Stop but does not abandon cleanup.");
    });
    if probe {
        // Preserve the original failed probe and ready=false, but describe the
        // actual cleanup state at publication rather than its historical state.
        report.cleanup_status = "confirmed";
    }
    if as_json {
        println!(
            "{}",
            serde_json::to_string(&report).expect("diagnostic report")
        );
    } else {
        println!(
            "External media tools (current launch PATH; route {}):",
            report.profile
        );
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
            println!(
                "  {status:7} {:8} {} ({})",
                tool.name,
                tool.purpose,
                if tool.required {
                    "required"
                } else {
                    "optional for this route"
                }
            );
            if let Some(path) = &tool.path {
                println!("    path: {}", serde_json::to_string(path).expect("path"));
            }
            if probe && tool.available {
                println!(
                    "    version: {} [{}]; minimum: {}",
                    tool.version.as_deref().unwrap_or("unknown"),
                    tool.version_status,
                    tool.minimum_version
                );
                println!("    advertised features: {}", tool.feature_status);
                if let Some(code) = tool.failure_code {
                    println!("    diagnostic: {code}");
                }
            }
        }
        if probe {
            println!(
                "\nProbes check version banners and advertised options. They do not process media."
            );
        } else {
            println!(
                "\nDiscovery does not run tools or verify versions. Use 'drmr doctor --probe' to check versions and advertised options."
            );
        }
        if probe {
            println!("Cleanup: {}", report.cleanup_status);
        }
        if !report.ready {
            println!(
                "Install compatible FFmpeg or Shaka Packager tools required for this route, and add their directories to the launcher's PATH."
            );
            println!(
                "Run 'drmr doctor --probe' again; media compatibility still requires conversion checks."
            );
        }
    }
    ExitCode::from(u8::from(!report.ready))
}
