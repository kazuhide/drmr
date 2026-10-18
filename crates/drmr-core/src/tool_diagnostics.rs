//! Bounded, read-only dependency checks shared by the CLI and native application.
//!
//! Discovery never executes a tool. Explicit probing checks advertised options, not
//! media compatibility, license delivery, or the correctness of a particular build.

use crate::events::NoSink;
use crate::toolrun::{RunOptions, Secrets, ToolRunner};
use regex::Regex;
use serde::Serialize;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

const TOOLS: [(&str, &str, &str); 4] = [
    ("ffmpeg", "media processing (FFmpeg)", "9.0.1"),
    ("ffprobe", "media inspection (FFmpeg)", "9.0.1"),
    (
        "mp4dump",
        "MP4 structure inspection (Bento4)",
        "1.6.0 (build 641 not exposed by banner)",
    ),
    (
        "packager",
        "media encryption and packaging (Shaka Packager)",
        "3.8.0",
    ),
];

/// A selected processing route, or the complete dependency set of the older workflow.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Profile(&'static str);

impl Profile {
    pub const ALL: Self = Self("all");

    pub fn parse(value: &str) -> Option<Self> {
        [
            "all",
            "prepared-dash",
            "progressive-mp4",
            "fragmented-mp4",
            "dash-presentation",
            "fmp4-hls-presentation",
            "mpeg-ts-hls",
            "mpeg-ts-sample-aes-hls",
            "mpeg-ts-hls-ts",
        ]
        .into_iter()
        .find(|name| *name == value)
        .map(Self)
    }

    fn required(self, tool: &str) -> bool {
        match tool {
            "mp4dump" => matches!(self.0, "all" | "prepared-dash"),
            "packager" => self.0 != "progressive-mp4",
            _ => true,
        }
    }
}

/// One fixed, advertised command-line option. Presence is not a media execution test.
#[derive(Debug, Serialize)]
pub struct FeatureCheck {
    pub name: &'static str,
    pub available: bool,
}

#[derive(Debug, Serialize)]
pub struct ToolDiagnostic {
    pub name: &'static str,
    pub purpose: &'static str,
    pub available: bool,
    pub required: bool,
    pub path: Option<PathBuf>,
    pub path_status: &'static str,
    pub minimum_version: &'static str,
    pub version: Option<String>,
    pub version_status: &'static str,
    pub feature_status: &'static str,
    pub features: Vec<FeatureCheck>,
    pub failure_code: Option<&'static str>,
}

#[derive(Debug, Serialize)]
pub struct DiagnosticReport {
    pub schema_version: u8,
    pub mode: &'static str,
    pub profile: &'static str,
    pub ready: bool,
    pub media_compatibility_checked: bool,
    pub tools: Vec<ToolDiagnostic>,
}

impl DiagnosticReport {
    /// A native-dialog summary. Paths are quoted to escape control characters.
    pub fn summary(&self) -> String {
        let mut text = String::from("Tools in this application's launch PATH:\n\n");
        for tool in &self.tools {
            writeln!(
                text,
                "{}: {}{}",
                tool.name,
                if tool.available { "found" } else { "missing" },
                if tool.required {
                    " (required)"
                } else {
                    " (optional)"
                }
            )
            .expect("String write");
            if let Some(path) = &tool.path {
                writeln!(
                    text,
                    "  {}",
                    serde_json::to_string(path).unwrap_or_else(|_| "[unrepresentable path]".into())
                )
                .expect("String write");
            }
            writeln!(
                text,
                "  version: {} [{}]; options: {}",
                tool.version.as_deref().unwrap_or("unknown"),
                tool.version_status,
                tool.feature_status
            )
            .expect("String write");
            if let Some(code) = tool.failure_code {
                writeln!(text, "  {code}").expect("String write");
            }
        }
        text.push_str("\nVersion/help checks do not prove media compatibility. Bento4's build number is not exposed; confirm build 641 or newer with its distributor.\n\nIf required checks fail, install compatible tools and reopen drmr from an environment whose PATH contains them. Shell startup files and extra installation folders are not searched.");
        text
    }
}

/// Use exactly the current launcher's PATH; do not read shell startup files or guess folders.
/// Each probe has a three-second execution/admission deadline and a 1 MiB limit per stream.
/// Managed cleanup retains process ownership if termination cannot be confirmed.
pub fn diagnose(profile: Profile, probe: bool) -> DiagnosticReport {
    let path = std::env::var_os("PATH");
    let directories: Vec<_> = path
        .as_deref()
        .map(std::env::split_paths)
        .into_iter()
        .flatten()
        .collect();
    let runner = ToolRunner::new(Arc::new(Secrets::new()), Arc::new(NoSink));
    let tools: Vec<_> = TOOLS
        .iter()
        .map(|&(name, purpose, minimum_version)| {
            let executable = if cfg!(windows) {
                format!("{name}.exe")
            } else {
                name.into()
            };
            let resolved = directories
                .iter()
                .map(|p| p.join(&executable))
                .find(|p| is_executable(p))
                .and_then(|p| p.canonicalize().ok());
            let available = resolved.is_some();
            let representable = resolved.as_ref().is_none_or(|p| p.to_str().is_some());
            let mut tool = ToolDiagnostic {
                name,
                purpose,
                minimum_version,
                available,
                required: profile.required(name),
                path: resolved.filter(|_| representable),
                path_status: if !representable {
                    "not-utf8"
                } else if available {
                    "resolved"
                } else {
                    "missing"
                },
                version: None,
                version_status: "not-checked",
                feature_status: "not-checked",
                features: Vec::new(),
                failure_code: if representable {
                    None
                } else {
                    Some("probe-path-not-utf8")
                },
            };
            if probe
                && tool.available
                && tool.failure_code.is_none()
                && let Err(code) = probe_tool(&runner, &mut tool)
            {
                tool.failure_code = Some(code);
            }
            tool
        })
        .collect();
    let ready = tools.iter().filter(|t| t.required).all(|t| {
        t.available
            && (!probe
                || (matches!(t.version_status, "supported" | "build-unverified")
                    && t.feature_status == "available"
                    && t.failure_code.is_none()))
    });
    DiagnosticReport {
        schema_version: 1,
        mode: if probe { "probe" } else { "discovery" },
        profile: profile.0,
        ready,
        media_compatibility_checked: false,
        tools,
    }
}

fn output(
    runner: &ToolRunner,
    tool: &ToolDiagnostic,
    arguments: &[&str],
    usage: bool,
) -> Result<String, &'static str> {
    let mut argv = vec![
        tool.path
            .as_ref()
            .expect("discovered tool")
            .to_str()
            .ok_or("probe-path-not-utf8")?
            .to_owned(),
    ];
    argv.extend(arguments.iter().map(|a| (*a).to_owned()));
    let shown = [tool.name.to_owned(), "[diagnostic arguments]".to_owned()];
    let result = runner
        .run_tool(
            &argv,
            tool.name,
            &RunOptions {
                timeout: Some(Duration::from_secs(3)),
                max_stdout_bytes: 1024 * 1024,
                max_stderr_bytes: 1024 * 1024,
                max_output_chars: 0,
                log_stdout: false,
                argv_redacted: Some(&shown),
                ..RunOptions::default()
            },
        )
        .map_err(|e| match e.kind {
            "timeout" => "probe-timeout",
            "output-limit" => "probe-output-limit",
            "cleanup-unconfirmed" => "probe-cleanup-unconfirmed",
            _ => "probe-execution-failed",
        })?;
    if result.returncode != Some(0) && !(usage && result.returncode == Some(1)) {
        return Err("probe-exit-failed");
    }
    Ok(format!(
        "{}\n{}",
        result.stdout_text(),
        result.stderr_text()
    ))
}

fn probe_tool(runner: &ToolRunner, tool: &mut ToolDiagnostic) -> Result<(), &'static str> {
    let args: &[&str] = match tool.name {
        "ffmpeg" | "ffprobe" => &["-version"],
        "packager" => &["--version"],
        _ => &[],
    };
    let banner = output(runner, tool, args, tool.name == "mp4dump")?;
    let pattern = match tool.name {
        "ffmpeg" => r"(?m)^ffmpeg version ([0-9]+)\.([0-9]+)(?:\.([0-9]+))?",
        "ffprobe" => r"(?m)^ffprobe version ([0-9]+)\.([0-9]+)(?:\.([0-9]+))?",
        "packager" => r"(?m)^packager version v?([0-9]+)\.([0-9]+)(?:\.([0-9]+))?",
        _ => r"\(Bento4 Version ([0-9]+)\.([0-9]+)\.([0-9]+)",
    };
    let Some(captures) = Regex::new(pattern)
        .expect("fixed banner pattern")
        .captures(&banner)
    else {
        tool.version_status = "unrecognized";
        return Err("probe-version-unrecognized");
    };
    let numbers: Option<Vec<u64>> = (1..=3)
        .map(|i| captures.get(i).map_or(Some(0), |m| m.as_str().parse().ok()))
        .collect();
    let Some(v) = numbers else {
        tool.version_status = "unrecognized";
        return Err("probe-version-unrecognized");
    };
    tool.version = Some(format!("{}.{}.{}", v[0], v[1], v[2]));
    let minimum: &[u64] = match tool.name {
        "ffmpeg" | "ffprobe" => &[9, 0, 1],
        "packager" => &[3, 8, 0],
        _ => &[1, 6, 0],
    };
    tool.version_status = if v.as_slice() < minimum {
        "below-minimum"
    } else if tool.name == "mp4dump" {
        "build-unverified"
    } else {
        "supported"
    };
    let (help, features): (String, &[&'static str]) = match tool.name {
        "ffmpeg" => (
            format!(
                "{}\n{}",
                output(runner, tool, &["-hide_banner", "-h", "muxer=mp4"], false)?,
                output(runner, tool, &["-hide_banner", "-h", "demuxer=mov"], false)?
            ),
            &["encryption_scheme", "decryption_key"],
        ),
        "ffprobe" => (
            output(runner, tool, &["-hide_banner", "-h", "full"], false)?,
            &["show_packets", "show_data_hash", "show_entries"],
        ),
        "packager" => (
            output(runner, tool, &["--help"], true)?,
            &[
                "enable_raw_key_encryption",
                "enable_raw_key_decryption",
                "protection_scheme",
                "segment_duration",
                "mpd_output",
                "hls_master_playlist_output",
            ],
        ),
        _ => (banner, &["format"]),
    };
    tool.features = features
        .iter()
        .map(|&name| {
            let pattern = format!(r"(?m)^\s*--?{}(?:\s|\b)", regex::escape(name));
            FeatureCheck {
                name,
                available: Regex::new(&pattern)
                    .expect("fixed option pattern")
                    .is_match(&help),
            }
        })
        .collect();
    tool.feature_status = if tool.features.iter().all(|f| f.available) {
        "available"
    } else {
        "missing"
    };
    Ok(())
}

fn is_executable(path: &Path) -> bool {
    let Ok(metadata) = path.metadata() else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}
