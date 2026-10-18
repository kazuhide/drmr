//! Transformation intent, key roles, and compatibility without executing a backend.

use super::{Finding, InputFormat, Inspection, Protection, TrackKind};
use serde::{Deserialize, Serialize};
use std::str::FromStr;

/// Requested output packaging, independent of the protection scheme.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum OutputFormat {
    Mp4,
    Dash,
    Hls,
    HlsTs,
}

impl OutputFormat {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Mp4 => "mp4",
            Self::Dash => "dash",
            Self::Hls => "hls",
            Self::HlsTs => "hls-ts",
        }
    }
}

impl From<InputFormat> for OutputFormat {
    fn from(value: InputFormat) -> Self {
        match value {
            InputFormat::Mp4 => Self::Mp4,
            InputFormat::Dash => Self::Dash,
            InputFormat::Hls => Self::Hls,
        }
    }
}

impl FromStr for OutputFormat {
    type Err = &'static str;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "mp4" => Ok(Self::Mp4),
            "dash" => Ok(Self::Dash),
            "hls" => Ok(Self::Hls),
            "hls-ts" => Ok(Self::HlsTs),
            _ => Err("expected mp4, dash, hls, or hls-ts"),
        }
    }
}

/// Initial conversion target profiles. An HLS segment cipher is not an MP4 scheme.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TargetProtection {
    Clear,
    Cenc,
    Cbcs,
    HlsAes128,
    HlsSampleAes,
}

impl TargetProtection {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Clear => "clear",
            Self::Cenc => "cenc",
            Self::Cbcs => "cbcs",
            Self::HlsAes128 => "hls-aes128",
            Self::HlsSampleAes => "hls-sample-aes",
        }
    }
}

impl FromStr for TargetProtection {
    type Err = &'static str;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "clear" => Ok(Self::Clear),
            "cenc" => Ok(Self::Cenc),
            "cbcs" => Ok(Self::Cbcs),
            "hls-aes128" => Ok(Self::HlsAes128),
            "hls-sample-aes" => Ok(Self::HlsSampleAes),
            _ => Err("expected clear, cenc, cbcs, hls-aes128, or hls-sample-aes"),
        }
    }
}

/// A key's purpose; actual key and IV values never enter an inspection or plan.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeyRequirement {
    pub role: String,
    pub known_key_ids: Vec<String>,
}

/// A plan is intent, never an authorization or a trusted saved execution artifact.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConversionPlan {
    pub schema_version: u32,
    pub input: Inspection,
    pub target: TargetProtection,
    pub formats: Vec<OutputFormat>,
    pub operations: Vec<String>,
    pub keys: Vec<KeyRequirement>,
    pub compatible: bool,
    pub execution_available: bool,
    pub findings: Vec<Finding>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub processing: Option<super::PlannedProcessing>,
}

fn progressive_candidate(input: &Inspection, target: TargetProtection) -> bool {
    input.format == InputFormat::Mp4
        && target == TargetProtection::Clear
        && input.tracks.len() == 2
        && input
            .tracks
            .iter()
            .all(|track| track.protection == input.protection)
        && matches!(input.protection, Protection::Clear | Protection::Cenc)
        && input
            .tracks
            .iter()
            .filter(|track| {
                track.kind == TrackKind::Audio && track.codec.as_deref() == Some("mp4a")
            })
            .count()
            == 1
        && input
            .tracks
            .iter()
            .filter(|track| {
                track.kind == TrackKind::Video && track.codec.as_deref() == Some("avc1")
            })
            .count()
            == 1
}

fn sample_aes_candidate(input: &Inspection) -> bool {
    use super::{DescriptionEvidence, Evidence, HlsKeyMethod};
    if input.format != InputFormat::Hls
        || input.evidence != Evidence::Manifest
        || input.tracks.is_empty()
    {
        return false;
    }
    let Some(details) = &input.details else {
        return false;
    };
    details.track_protection.len() == input.tracks.len()
        && input
            .tracks
            .iter()
            .zip(&details.track_protection)
            .all(|(track, detail)| {
                track.protection == Protection::Unknown
                    && track.kind != TrackKind::Text
                    && detail.track_id.as_deref() == Some(track.id.as_str())
                    && detail.evidence == DescriptionEvidence::ManifestDeclaration
                    && detail.hls_key_methods == [HlsKeyMethod::SampleAes]
            })
}

fn compatibility_findings(
    input: &Inspection,
    target: TargetProtection,
    formats: &[OutputFormat],
) -> Vec<Finding> {
    let mut findings = vec![Finding::new("media-validation-required")];
    if target == TargetProtection::HlsAes128
        && formats
            .iter()
            .any(|format| !matches!(format, OutputFormat::Hls | OutputFormat::HlsTs))
    {
        findings.push(Finding::new("hls-aes128-requires-hls-output"));
    }
    if target == TargetProtection::HlsSampleAes && formats != [OutputFormat::HlsTs] {
        findings.push(Finding::new("hls-sample-aes-requires-hls-ts-output"));
    }
    if formats.contains(&OutputFormat::HlsTs) {
        if matches!(target, TargetProtection::Cenc | TargetProtection::Cbcs) {
            findings.push(Finding::new("mp4-protection-requires-mp4-container"));
        }
        if input.format != InputFormat::Hls {
            findings.push(Finding::new(
                "hls-ts-output-requires-transport-stream-input",
            ));
        } else if !sample_aes_candidate(input)
            && !input.tracks.is_empty()
            && input
                .tracks
                .iter()
                .any(|track| !matches!(track.protection, Protection::Clear | Protection::HlsAes128))
        {
            findings.push(Finding::new("hls-ts-source-protection-not-supported"));
        }
    }
    if target == TargetProtection::Cenc && formats.contains(&OutputFormat::Hls) {
        findings.push(Finding::new("cenc-hls-output-not-supported"));
    }
    if input.tracks.is_empty()
        || input
            .tracks
            .iter()
            .any(|track| matches!(track.protection, Protection::Unknown | Protection::Mixed))
    {
        findings.push(Finding::new("source-protection-unresolved"));
    }
    if input
        .tracks
        .iter()
        .any(|track| matches!(track.protection, Protection::Cens | Protection::Cbc1))
    {
        findings.push(Finding::new("source-protection-not-supported"));
    }
    findings
}

/// Describe a requested transformation. Source media must be revalidated at execution.
pub fn plan(
    input: Inspection,
    target: TargetProtection,
    mut formats: Vec<OutputFormat>,
) -> ConversionPlan {
    if formats.is_empty() {
        formats.push(input.format.into());
    }
    formats.sort();
    formats.dedup();
    let mut findings = compatibility_findings(&input, target, &formats);
    // A SAMPLE-AES declaration can select a conditional handler without establishing
    // its container scheme. Every original track must carry its own declaration.
    let sample_aes = sample_aes_candidate(&input);
    let mut compatible = findings.iter().all(|finding| {
        finding.code == "media-validation-required"
            || (sample_aes && finding.code == "source-protection-unresolved")
    });
    if sample_aes {
        findings.push(Finding::new("hls-sample-aes-input-key-and-iv-required"));
    }
    let mut execution_available = compatible && formats.len() == 1;
    if formats.len() != 1 {
        findings.push(Finding::new("convert-one-output-format-per-invocation"));
    }
    let progressive_candidate = progressive_candidate(&input, target);
    if progressive_candidate && formats == [OutputFormat::Mp4] {
        findings.push(Finding::new("progressive-mp4-profile-validation-required"));
    }
    if formats == [OutputFormat::Mp4] && input.tracks.len() != 1 && !progressive_candidate {
        execution_available = false;
        findings.push(Finding::new("mp4-output-requires-one-track"));
    }
    findings.push(Finding::new("source-profile-validation-required"));
    let mut operations = vec!["validate-source".into()];
    let mut keys = Vec::new();
    if input
        .tracks
        .iter()
        .any(|track| track.protection != Protection::Clear)
    {
        operations.push("decrypt-input".into());
        let mut ids: Vec<_> = input
            .tracks
            .iter()
            .flat_map(|track| track.key_ids.clone())
            .collect();
        ids.sort();
        ids.dedup();
        keys.push(KeyRequirement {
            role: "input".into(),
            known_key_ids: ids,
        });
    }
    operations.push("repackage-media".into());
    if target != TargetProtection::Clear {
        operations.push("encrypt-output".into());
        keys.push(KeyRequirement {
            role: "output".into(),
            known_key_ids: Vec::new(),
        });
    }
    if formats.iter().any(|format| *format != OutputFormat::Mp4) {
        operations.push("write-manifests".into());
    }
    operations.push("verify-output".into());
    let processing = describe_processing(&input, target, &formats, compatible);
    if compatible
        && formats.iter().any(|format| {
            !processing
                .candidates
                .iter()
                .any(|candidate| candidate.output_format == *format)
        })
    {
        compatible = false;
        findings.push(Finding::new(if processing.candidates.is_empty() {
            "no-supported-processing-route"
        } else {
            "output-format-has-no-processing-route"
        }));
    }
    execution_available &= compatible;
    ConversionPlan {
        schema_version: 1,
        input,
        target,
        formats,
        operations,
        keys,
        compatible,
        execution_available,
        findings,
        processing: Some(processing),
    }
}

fn describe_processing(
    input: &Inspection,
    target: TargetProtection,
    formats: &[OutputFormat],
    compatible: bool,
) -> super::PlannedProcessing {
    use super::{
        CandidateRoute, DescriptionEvidence, PlannedProcessing, PlanningStatus,
        ProcessingRouteId as Route,
    };
    let protection = match target {
        TargetProtection::Clear => Protection::Clear,
        TargetProtection::Cenc => Protection::Cenc,
        TargetProtection::Cbcs => Protection::Cbcs,
        TargetProtection::HlsAes128 => Protection::HlsAes128,
        TargetProtection::HlsSampleAes => Protection::Unknown,
    };
    let mut candidates = Vec::new();
    // Unknown manifest roles need resource admission; known text tracks are refused by every handler.
    if compatible
        && !input
            .tracks
            .iter()
            .any(|track| track.kind == TrackKind::Text)
    {
        for &format in formats {
            match format {
                OutputFormat::Mp4 => {
                    let finite = input.format == InputFormat::Mp4
                        && target == TargetProtection::Clear
                        && matches!(input.protection, Protection::Clear | Protection::Cenc)
                        && (progressive_candidate(input, target)
                            || (input.tracks.len() == 1
                                && input.tracks[0].kind == TrackKind::Audio
                                && input.tracks[0].codec.as_deref() == Some("mp4a")));
                    if finite {
                        candidates.push(CandidateRoute::new(
                            Route::ProgressiveMp4,
                            format,
                            "finite-progressive-profile-must-pass",
                        ));
                    }
                    if input.tracks.len() == 1 {
                        candidates.push(CandidateRoute::new(
                            Route::FragmentedMp4,
                            format,
                            "single-track-fragmented-profile-without-finite-audio-tail-must-pass",
                        ));
                    }
                }
                OutputFormat::Dash => candidates.push(CandidateRoute::new(
                    Route::DashPresentation,
                    format,
                    "local-mp4-or-fragmented-manifest-profile-must-pass",
                )),
                OutputFormat::HlsTs => {
                    if input.format == InputFormat::Hls {
                        candidates.push(CandidateRoute::new(
                            Route::MpegTsHlsTs,
                            format,
                            "local-transport-stream-profile-must-pass",
                        ));
                    }
                }
                OutputFormat::Hls => {
                    candidates.push(CandidateRoute::new(
                        Route::Fmp4HlsPresentation,
                        format,
                        "local-mp4-or-fragmented-manifest-profile-must-pass",
                    ));
                    if input.format == InputFormat::Hls {
                        let (route, condition) = if sample_aes_candidate(input) {
                            (
                                Route::MpegTsSampleAesHls,
                                "sample-aes-transport-profile-must-pass",
                            )
                        } else {
                            (Route::MpegTsHls, "local-transport-stream-profile-must-pass")
                        };
                        candidates.push(CandidateRoute::new(route, format, condition));
                    }
                }
            }
        }
    }
    let mut target_description =
        super::ProtectionDescription::new(protection, DescriptionEvidence::RequestedTarget);
    if target == TargetProtection::HlsSampleAes {
        target_description.hls_methods(&std::collections::BTreeSet::from(["SAMPLE-AES".into()]));
    }
    PlannedProcessing {
        status: PlanningStatus::MetadataOnly,
        target: target_description,
        candidates,
    }
}
