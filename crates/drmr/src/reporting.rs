//! Optional private result files and readable statistics reuse the completed report.
use drmr_core::conversion::{ConversionError, plan};
use serde_json::Value;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};

static TARGET: OnceLock<PathBuf> = OnceLock::new();
static WRITTEN: AtomicBool = AtomicBool::new(false);
static DISABLED: AtomicBool = AtomicBool::new(false);
static READY: AtomicBool = AtomicBool::new(false);

pub(crate) fn configure(path: Option<&Path>) -> Result<(), ConversionError> {
    if let Some(path) = path {
        let path = plan::report_destination(path)?;
        TARGET
            .set(path)
            .map_err(|_| ConversionError("report-destination-unsafe"))?;
    }
    Ok(())
}

pub(crate) fn enabled() -> bool {
    TARGET.get().is_some() && !DISABLED.load(Ordering::Acquire)
}

/// Error reports are allowed only after all selected source/result paths are known.
pub(crate) fn ready() {
    READY.store(true, Ordering::Release);
}

pub(crate) fn error_hint(code: &str) -> Option<&'static str> {
    match code {
        "report-destination-overlap" => Some(
            "Keep --report-file outside source media and result trees, separate from plans and private settings.",
        ),
        "report-destination-exists"
        | "report-destination-unsafe"
        | "report-destination-write-failed"
        | "report-destination-limit" => Some(
            "Choose a new --report-file under an existing trusted directory; existing files are never replaced.",
        ),
        _ => None,
    }
}

fn absolute(path: &Path) -> Result<PathBuf, ConversionError> {
    if path.exists() {
        return path
            .canonicalize()
            .map_err(|_| ConversionError("report-destination-unsafe"));
    }
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    Ok(parent
        .canonicalize()
        .map_err(|_| ConversionError("report-destination-unsafe"))?
        .join(
            path.file_name()
                .ok_or(ConversionError("report-destination-unsafe"))?,
        ))
}

/// Keep the report outside a media result, retained source, or protected file.
pub(crate) fn protect(path: &Path, tree: bool) -> Result<(), ConversionError> {
    let Some(target) = TARGET.get() else {
        return Ok(());
    };
    let path = absolute(path).inspect_err(|_| DISABLED.store(true, Ordering::Release))?;
    if target == &path || (tree && target.starts_with(&path)) || path.starts_with(target) {
        DISABLED.store(true, Ordering::Release);
        return Err(ConversionError("report-destination-overlap"));
    }
    Ok(())
}

pub(crate) fn protect_source(path: &Path) -> Result<(), ConversionError> {
    if !enabled() {
        return Ok(());
    }
    protect(path, true)?;
    if !path.is_dir() && plan::report_source_requires_parent_guard(path) {
        let parent = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        protect(parent, true)?;
    }
    Ok(())
}

pub(crate) fn protect_saved<'a>(
    source_root: &Path,
    source_files: impl IntoIterator<Item = &'a Path>,
    single_file_source: bool,
    output: &Path,
) -> Result<(), ConversionError> {
    if !enabled() {
        return Ok(());
    }
    protect(output, true)?;
    if single_file_source {
        for file in source_files {
            protect(&source_root.join(file), false)?;
        }
    } else {
        protect(source_root, true)?;
    }
    Ok(())
}

pub(crate) fn record(value: &Value) -> std::io::Result<()> {
    if DISABLED.load(Ordering::Acquire) || !READY.load(Ordering::Acquire) {
        return Ok(());
    }
    if let Some(path) = TARGET.get()
        && !WRITTEN.load(Ordering::Acquire)
    {
        plan::write_report_new(path, value)
            .map_err(|_| std::io::Error::other("report-output-unavailable"))?;
        WRITTEN.store(true, Ordering::Release);
    }
    Ok(())
}

pub(crate) fn render(
    value: &Value,
    as_json: bool,
    human: impl FnOnce() -> std::io::Result<()>,
) -> std::io::Result<()> {
    record(value)?;
    if as_json {
        crate::convert_cli::json(value)
    } else {
        human()?;
        if let Some(measurements) = value.get("measurements") {
            stats(measurements, &mut std::io::stdout().lock())?;
        }
        Ok(())
    }
}

pub(crate) fn measurements(
    value: &mut Value,
    measurements: Option<&drmr_core::conversion::measurement::MeasurementReport>,
) {
    if let Some(measurements) = measurements {
        value["measurements"] = serde_json::to_value(measurements).expect("measurements serialize");
    }
}

/// Phase sums can overlap; their sum must never be presented as total wall time.
pub(crate) fn stats(value: &Value, out: &mut impl Write) -> std::io::Result<()> {
    writeln!(out, "\nStatistics")?;
    if let Some(phases) = value["phases"].as_array() {
        for phase in phases {
            let scopes = phase["scopes"].as_u64().unwrap_or(0);
            if scopes == 0 {
                continue;
            }
            let label = match phase["phase"].as_str() {
                Some("input-preparation") => "Source preparation",
                Some("inspection") => "Metadata",
                Some("normalization") => "Media preparation",
                Some("packaging") => "Conversion",
                Some("verification") => "Verification",
                Some("publication") => "Saving result",
                _ => "Other media work",
            };
            if let Some(seconds) = phase["scope_elapsed_sum_seconds"].as_f64() {
                writeln!(out, "  {label:22} {seconds:8.2}s  ({scopes} measurements)")?;
            }
        }
    }
    writeln!(
        out,
        "  Scope times overlap during parallel work; they are not total elapsed time."
    )?;
    if let Some(bytes) = value["workspace"]["checkpoint_regular_file_bytes_max"].as_u64() {
        writeln!(
            out,
            "  Largest measured workspace checkpoint: {}.{:02} MiB",
            bytes / 1_048_576,
            (bytes % 1_048_576) * 100 / 1_048_576
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn statistics_distinguish_overlapping_scopes_and_omit_unmeasured_phases() {
        let value = serde_json::json!({
            "phases": [
                {"phase":"verification", "scopes":2, "scope_elapsed_sum_seconds":3.5},
                {"phase":"packaging", "scopes":0, "scope_elapsed_sum_seconds":0.0}
            ],
            "workspace":{"checkpoint_regular_file_bytes_max":1_048_576}
        });
        let mut out = Vec::new();
        stats(&value, &mut out).unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(text.contains("Verification") && text.contains("3.50s"));
        assert!(text.contains("2 measurements") && text.contains("overlap"));
        assert!(text.contains("1.00 MiB"));
        assert!(!text.contains("Conversion"));
    }
}
