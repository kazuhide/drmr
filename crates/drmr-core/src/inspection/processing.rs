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
}
impl ProcessingRouteId {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ProgressiveMp4 => "progressive-mp4",
            Self::FragmentedMp4 => "fragmented-mp4",
            Self::DashPresentation => "dash-presentation",
            Self::Fmp4HlsPresentation => "fmp4-hls-presentation",
            Self::MpegTsHls => "mpeg-ts-hls",
        }
    }
    pub const fn explanation(self) -> &'static str {
        match self {
            Self::ProgressiveMp4 => {
                "Copy the complete original tracks directly into progressive MP4 to preserve a finite audio presentation, then verify every original track."
            }
            Self::FragmentedMp4 => {
                "Decrypt and repackage one complete track through fragmented MP4, apply the requested output protection, then compare it with the original."
            }
            Self::DashPresentation => {
                "Prepare complete local tracks, package them into DASH segments, and verify each original track against the output presentation."
            }
            Self::Fmp4HlsPresentation => {
                "Prepare complete local tracks, package fragmented MP4 HLS resources, apply the requested resource or sample protection, and verify each original track."
            }
            Self::MpegTsHls => {
                "Admit the original local transport-stream resources, demultiplex their tracks, package fragmented MP4 HLS, and verify the actual final playlists against the original transport streams."
            }
        }
    }
    pub fn required_tools(self) -> Vec<RequiredTool> {
        let mut tools = vec![RequiredTool::Ffmpeg, RequiredTool::Ffprobe];
        if self != Self::ProgressiveMp4 {
            tools.push(RequiredTool::ShakaPackager);
        }
        tools
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
    LocalTrackFiles,
}
impl InputPreparation {
    pub const fn description(self) -> &'static str {
        match self {
            Self::CompleteFile => "the complete original MP4 file",
            Self::LocalDashResources => "local DASH resources assembled into complete tracks",
            Self::LocalHlsResources => "local HLS resources assembled into complete tracks",
            Self::MpegTsDemux => "local transport-stream resources demultiplexed into tracks",
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
    pub required_tools: Vec<RequiredTool>,
}
impl CandidateRoute {
    pub(super) fn new(
        route: ProcessingRouteId,
        output_format: super::OutputFormat,
        condition: &'static str,
    ) -> Self {
        Self {
            route,
            output_format,
            revalidation_required: true,
            condition_codes: vec![condition.into()],
            reason_code: format!("{}-candidate", route.as_str()),
            explanation: route.explanation().into(),
            required_tools: route.required_tools(),
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
