//! Fixed processing descriptions shared by candidate plans and successful execution reports.
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ProcessingRouteId {
    ProgressiveMp4,
    FragmentedMp4,
    DashPresentation,
    Fmp4HlsPresentation,
    MpegTsHls,
    MpegTsDash,
    MpegTsSampleAesHls,
    MpegTsHlsTs,
}
impl ProcessingRouteId {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ProgressiveMp4 => "progressive-mp4",
            Self::FragmentedMp4 => "fragmented-mp4",
            Self::DashPresentation => "dash-presentation",
            Self::Fmp4HlsPresentation => "fmp4-hls-presentation",
            Self::MpegTsHls => "mpeg-ts-hls",
            Self::MpegTsDash => "mpeg-ts-dash",
            Self::MpegTsSampleAesHls => "mpeg-ts-sample-aes-hls",
            Self::MpegTsHlsTs => "mpeg-ts-hls-ts",
        }
    }
    pub const fn explanation(self) -> &'static str {
        match self {
            Self::ProgressiveMp4 => {
                "Copy the complete original tracks directly into progressive MP4 to preserve a finite audio presentation, then verify every original track."
            }
            Self::FragmentedMp4 => {
                "Decrypt when needed and repackage complete source tracks through fragmented MP4, preserve their presentation windows, apply the requested output protection, then compare them with the original."
            }
            Self::DashPresentation => {
                "Prepare complete local tracks, package them into DASH segments, and verify each original track against the output presentation."
            }
            Self::Fmp4HlsPresentation => {
                "Prepare complete local tracks, package fragmented MP4 HLS resources, apply output encryption when requested, and verify each original track."
            }
            Self::MpegTsDash => {
                "Admit the original local transport streams, decrypt when needed, demultiplex complete tracks without changing their timestamps, package static fragmented MP4 DASH, and verify the actual final manifest against every original access unit."
            }
            Self::MpegTsHls => {
                "Admit the original local transport-stream resources, demultiplex their tracks, package fragmented MP4 HLS, and verify the actual final playlists against the original transport streams."
            }
            Self::MpegTsHlsTs => {
                "Admit the original local transport streams, decrypt when needed, bind complete tracks to their original encoded frames and timestamps, then package MPEG-TS HLS with the requested protection and verify the actual final playlists against the original media."
            }
            Self::MpegTsSampleAesHls => {
                "Validate the original SAMPLE-AES transport streams, decrypt encoded frames with supplied keys and initialization vectors, verify every decrypted frame and original timestamp, then package fragmented MP4 HLS and compare its actual playlists with the original media."
            }
        }
    }
    /// Minimum tools for native conversion; conditional copying and decoding add tools.
    pub fn required_tools(self) -> Vec<RequiredTool> {
        if self == Self::ProgressiveMp4 {
            Vec::new()
        } else {
            vec![RequiredTool::ShakaPackager]
        }
    }

    pub(crate) const fn can_copy_encrypted_finite_source(self) -> bool {
        matches!(
            self,
            Self::ProgressiveMp4
                | Self::FragmentedMp4
                | Self::DashPresentation
                | Self::Fmp4HlsPresentation
        )
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RequiredTool {
    Ffmpeg,
    Ffprobe,
    ShakaPackager,
}
impl RequiredTool {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ffmpeg => "ffmpeg",
            Self::Ffprobe => "ffprobe",
            Self::ShakaPackager => "shaka-packager",
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum InputPreparation {
    CompleteFile,
    LocalDashResources,
    LocalHlsResources,
    MpegTsDemux,
    MpegTsSampleAesDecrypt,
    LocalTrackFiles,
}
impl InputPreparation {
    pub const fn description(self) -> &'static str {
        match self {
            Self::CompleteFile => "the complete original MP4 file",
            Self::LocalDashResources => "local DASH resources assembled into complete tracks",
            Self::LocalHlsResources => "local HLS resources assembled into complete tracks",
            Self::MpegTsDemux => "local transport-stream resources demultiplexed into tracks",
            Self::MpegTsSampleAesDecrypt => {
                "local SAMPLE-AES transport streams decrypted and checked against original packet evidence before track demultiplexing"
            }
            Self::LocalTrackFiles => "the supplied complete local track files",
        }
    }
}
/// A successful handler supplies this description; it is not a saved execution instruction.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActualProcessing {
    pub route: ProcessingRouteId,
    pub input_preparation: InputPreparation,
    pub reason_code: String,
    pub explanation: String,
    /// Tools required at the actual depth, including any encrypted finite-source copy.
    pub required_tools: Vec<RequiredTool>,
}
impl ActualProcessing {
    pub fn new(route: ProcessingRouteId, input_preparation: InputPreparation) -> Self {
        Self {
            route,
            input_preparation,
            reason_code: format!("{}-handler-completed", route.as_str()),
            explanation: route.explanation().into(),
            required_tools: route.required_tools(),
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CandidateRoute {
    pub route: ProcessingRouteId,
    pub output_format: super::OutputFormat,
    pub revalidation_required: bool,
    pub condition_codes: Vec<String>,
    pub reason_code: String,
    pub explanation: String,
    /// Native minimum; `condition_codes` and explanation state additional tool requirements.
    pub required_tools: Vec<RequiredTool>,
}
impl CandidateRoute {
    pub(super) fn new(
        route: ProcessingRouteId,
        output_format: super::OutputFormat,
        condition: &'static str,
    ) -> Self {
        let mut condition_codes = vec![
            condition.into(),
            "additional-complete-decoding-requires-ffmpeg-and-ffprobe".into(),
        ];
        let mut explanation = format!(
            "{} Required tools list the native minimum. Additional complete decoding requires FFmpeg and ffprobe.",
            route.explanation()
        );
        if route.can_copy_encrypted_finite_source() {
            condition_codes.push("encrypted-finite-progressive-source-copy-requires-ffmpeg".into());
            explanation.push_str(
                " Copying an encrypted finite progressive source also requires FFmpeg for a container remux without transcoding.",
            );
        }
        Self {
            route,
            output_format,
            revalidation_required: true,
            condition_codes,
            reason_code: format!("{}-candidate", route.as_str()),
            explanation,
            required_tools: route.required_tools(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conditional_routes_distinguish_native_minima_from_additional_work() {
        for route in [
            ProcessingRouteId::ProgressiveMp4,
            ProcessingRouteId::FragmentedMp4,
            ProcessingRouteId::DashPresentation,
            ProcessingRouteId::Fmp4HlsPresentation,
            ProcessingRouteId::MpegTsHls,
            ProcessingRouteId::MpegTsDash,
            ProcessingRouteId::MpegTsSampleAesHls,
            ProcessingRouteId::MpegTsHlsTs,
        ] {
            let candidate = CandidateRoute::new(
                route,
                super::super::OutputFormat::Mp4,
                "source-profile-must-pass",
            );
            assert_eq!(candidate.condition_codes[0], "source-profile-must-pass");
            assert!(candidate.condition_codes.iter().any(|condition| {
                condition == "additional-complete-decoding-requires-ffmpeg-and-ffprobe"
            }));
            assert_eq!(
                candidate.condition_codes.iter().any(|condition| {
                    condition == "encrypted-finite-progressive-source-copy-requires-ffmpeg"
                }),
                route.can_copy_encrypted_finite_source()
            );
            assert_eq!(
                candidate.required_tools,
                if route == ProcessingRouteId::ProgressiveMp4 {
                    Vec::new()
                } else {
                    vec![RequiredTool::ShakaPackager]
                }
            );
            assert!(candidate.revalidation_required);
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PlanningStatus {
    MetadataOnly,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlannedProcessing {
    pub status: PlanningStatus,
    pub target: super::ProtectionDescription,
    pub candidates: Vec<CandidateRoute>,
}
