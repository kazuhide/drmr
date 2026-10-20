//! External checks of final manifest bytes. No media, keys, or licenses are fetched.
//!
//! These checks supplement media verification. They do not establish encrypted-media
//! conformance, license delivery, successful playback, or deployment behavior.

mod apple;
mod dash;
mod hls;
mod workspace;

use super::{ConversionError, Converter, files};
use crate::{
    inspection::OutputFormat,
    toolrun::{CleanupReceipt, RunOptions},
};
use serde::Serialize;
use sha2::{Digest as _, Sha256};
use std::{fmt, path::Path};

/// Explicit optional executable paths, independent of source and saved-plan metadata.
/// A selected format requires its corresponding executable; missing tools never pass.
#[derive(Clone, Default)]
pub struct ManifestValidationTools {
    pub mediastreamvalidator: Option<String>,
    /// PHP interpreter for the embedded DASH-IF manifest-only adapter.
    pub dash_manifest_validator: Option<String>,
}

impl fmt::Debug for ManifestValidationTools {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ManifestValidationTools")
            .field("mediastreamvalidator", &self.mediastreamvalidator.is_some())
            .field(
                "dash_manifest_validator",
                &self.dash_manifest_validator.is_some(),
            )
            .finish()
    }
}

/// One external engine, with neither executable paths nor untrusted diagnostic text.
#[derive(Clone, Debug, Serialize)]
pub struct ManifestToolCheck {
    pub tool: &'static str,
    pub passed: bool,
    pub checked_manifest_count: usize,
    pub warning_count: usize,
    pub observed_version: Option<String>,
}

/// The minimum protocol level follows used HLS features, rather than an exact version.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct HlsVersionCheck {
    pub manifest_ordinal: usize,
    pub declared: Option<u32>,
    pub minimum: u32,
}

/// A successful manifest-only check of all playlists or the final DASH MPD.
/// Every operational, parser, scope, or conformance failure returns an error instead.
#[derive(Clone, Debug, Serialize)]
pub struct ManifestValidationReport {
    pub schema_version: u32,
    pub scope: &'static str,
    pub passed: bool,
    pub manifest_count: usize,
    pub checks: Vec<ManifestToolCheck>,
    pub hls_versions: Vec<HlsVersionCheck>,
}

impl Converter {
    /// Validate the final generated manifests without reading media or requesting keys.
    /// Byte-identical private manifest copies prevent a tool from following a replaced
    /// output path. Each checked copy is revalidated after its tool call; every
    /// copy and original manifest is reread before success is returned.
    pub fn validate_manifest(
        &self,
        entrypoint: &Path,
        format: OutputFormat,
        tools: &ManifestValidationTools,
    ) -> Result<ManifestValidationReport, ConversionError> {
        self.backend.check_cancelled()?;
        match format {
            OutputFormat::Hls | OutputFormat::HlsTs => self.validate_hls_manifest(
                entrypoint,
                selected(tools.mediastreamvalidator.as_deref())?,
            ),
            OutputFormat::Dash => self.validate_dash_manifest(
                entrypoint,
                selected(tools.dash_manifest_validator.as_deref())?,
            ),
            OutputFormat::Mp4 => Err(ConversionError("manifest-validation-format-unsupported")),
        }
    }

    fn validate_hls_manifest(
        &self,
        entrypoint: &Path,
        executable: &str,
    ) -> Result<ManifestValidationReport, ConversionError> {
        let manifests = hls::capture(entrypoint)?;
        let mut workspace = workspace::Workspace::new()?;
        for manifest in &manifests {
            workspace.write(&manifest.relative, &manifest.bytes)?;
        }
        workspace.check()?;
        let mut observed = None;
        for manifest in &manifests {
            workspace.check_file(&manifest.relative)?;
            let path = workspace.path().join(&manifest.relative);
            let argv = vec![
                executable.to_owned(),
                "-p".into(),
                "-t".into(),
                self.backend.timeout.as_secs().max(1).to_string(),
                "-O".into(),
                "/dev/stdout".into(),
                files::tool_path(&path)?,
            ];
            let bytes = self.manifest_tool(&argv, "mediastreamvalidator")?;
            workspace.check_file(&manifest.relative)?;
            let version = apple::validate(&bytes, manifest, &files::tool_path(&path)?)?;
            if observed.as_ref().is_some_and(|old| old != &version) {
                return Err(ConversionError("manifest-validator-version-changed"));
            }
            observed = Some(version);
        }
        for manifest in &manifests {
            if files::read_manifest(&manifest.original)? != manifest.bytes {
                return Err(ConversionError("manifest-changed-during-validation"));
            }
        }
        workspace.check()?;
        self.backend.check_cancelled()?;
        let count = manifests.len();
        Ok(ManifestValidationReport {
            schema_version: 1,
            scope: "manifest-only",
            passed: true,
            manifest_count: count,
            checks: vec![ManifestToolCheck {
                tool: "apple-mediastreamvalidator",
                passed: true,
                checked_manifest_count: count,
                warning_count: 0,
                observed_version: observed.flatten(),
            }],
            hls_versions: manifests
                .iter()
                .enumerate()
                .map(|(ordinal, manifest)| HlsVersionCheck {
                    manifest_ordinal: ordinal,
                    declared: manifest.version,
                    minimum: manifest.minimum,
                })
                .collect(),
        })
    }

    fn validate_dash_manifest(
        &self,
        entrypoint: &Path,
        executable: &str,
    ) -> Result<ManifestValidationReport, ConversionError> {
        let original = files::read_manifest(entrypoint)?;
        dash::admit(&original)?;
        let mut workspace = workspace::Workspace::new()?;
        let copy = workspace.write(Path::new("manifest.mpd"), &original)?;
        let adapter = workspace.write(
            Path::new("dash_manifest_validator.php"),
            include_bytes!("conformance/dash_adapter.php"),
        )?;
        workspace.check()?;
        let bytes = self.manifest_tool(
            &[
                executable.to_owned(),
                "-d".into(),
                "display_errors=0".into(),
                "-d".into(),
                "log_errors=0".into(),
                files::tool_path(&adapter)?,
                files::tool_path(&copy)?,
            ],
            "dash-manifest-validator",
        )?;
        workspace.check()?;
        let version = dash::validate(&bytes, &digest(&original))?;
        if files::read_manifest(entrypoint)? != original {
            return Err(ConversionError("manifest-changed-during-validation"));
        }
        workspace.check()?;
        self.backend.check_cancelled()?;
        Ok(ManifestValidationReport {
            schema_version: 1,
            scope: "manifest-only",
            passed: true,
            manifest_count: 1,
            checks: vec![ManifestToolCheck {
                tool: "dash-if-manifest",
                passed: true,
                checked_manifest_count: 1,
                warning_count: 0,
                observed_version: version,
            }],
            hls_versions: Vec::new(),
        })
    }

    fn manifest_tool(&self, argv: &[String], tool: &str) -> Result<Vec<u8>, ConversionError> {
        let shown = vec![tool.to_owned(), "[arguments redacted]".into()];
        let (runner, _owner) = self.backend.runner.ensuring_cleanup_owner();
        let mut invocation = ManifestInvocation {
            converter: self,
            cleanup: None,
        };
        let result = runner.run_tool_with_cleanup_receipt(
            argv,
            tool,
            &RunOptions {
                argv_redacted: Some(&shown),
                stop: Some(&self.backend.stop),
                timeout: Some(self.backend.timeout),
                max_output_chars: 0,
                log_stdout: false,
                ..RunOptions::default()
            },
            &mut invocation.cleanup,
        );
        invocation.wait_for_cleanup(true);
        let result = result.map_err(|error| match error.kind {
            "cancelled" => ConversionError("cancelled"),
            "timeout" => ConversionError("manifest-validator-timeout"),
            "output-limit" => ConversionError("manifest-validator-output-limit"),
            "cleanup-unconfirmed" => ConversionError("manifest-validator-cleanup-unconfirmed"),
            _ => ConversionError("manifest-validator-unavailable"),
        })?;
        self.backend.check_cancelled()?;
        if result.returncode != Some(0) {
            return Err(ConversionError("manifest-validator-failed"));
        }
        Ok(result.stdout)
    }
}

struct ManifestInvocation<'a> {
    converter: &'a Converter,
    cleanup: Option<CleanupReceipt>,
}

impl ManifestInvocation<'_> {
    fn wait_for_cleanup(&self, notify: bool) {
        let Some(cleanup) = &self.cleanup else {
            // A denied shared-owner claim started no tool using these copies.
            return;
        };
        let mut notified = false;
        while cleanup.has_pending() {
            let _ = cleanup.retry_finish();
            if !cleanup.has_pending() {
                break;
            }
            if notify && !std::thread::panicking() && !notified {
                if let Some(activity) = &self.converter.backend.activity {
                    activity.emit(
                        serde_json::json!({"stage":"manifest-validation", "event":"cleanup-pending"})
                            .as_object()
                            .unwrap(),
                    );
                }
                notified = true;
            }
            std::thread::sleep(std::time::Duration::from_secs(1));
        }
    }
}

impl Drop for ManifestInvocation<'_> {
    fn drop(&mut self) {
        // A failure sink can unwind after retaining a live native tree. Keep
        // its files alive through cleanup, without invoking more user callbacks.
        self.wait_for_cleanup(false);
    }
}

fn selected(tool: Option<&str>) -> Result<&str, ConversionError> {
    tool.filter(|value| !value.is_empty() && !value.chars().any(char::is_control))
        .ok_or(ConversionError("manifest-validator-not-configured"))
}

fn digest(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn version_token(value: &str) -> Option<String> {
    let token = value.split_whitespace().next()?;
    (!token.is_empty()
        && token.len() <= 64
        && token.chars().all(|c| c.is_ascii_digit() || c == '.')
        && token.chars().any(|c| c.is_ascii_digit()))
    .then(|| token.to_owned())
}

#[cfg(all(test, unix))]
mod process_tests;
#[cfg(test)]
mod tests;
