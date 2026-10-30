//! Manifest declarations and original resource membership stay separate from media evidence.
use super::{ConversionError, Converter};
use crate::{
    conversion::{
        FrozenSource, VerificationSource, dash, files, presentation::metadata,
        timing::FragmentClock,
    },
    inspection::{self, InputFormat, Inspection, OutputFormat, Protection, TrackKind},
};
use std::path::{Path, PathBuf};

#[derive(Clone)]
pub(super) struct Resource {
    pub path: PathBuf,
    pub duration: String,
    pub dash: dash::SegmentBoundary,
}

#[derive(Clone)]
pub(super) struct Part {
    pub initialization: PathBuf,
    pub resources: Vec<Resource>,
    pub metadata: metadata::Metadata,
    pub role: Option<TrackKind>,
    pub outer: Protection,
    pub window: dash::Window,
    pub empty_cue_pair: bool,
}

#[derive(Clone)]
pub(super) struct Presentation {
    pub report: Inspection,
    pub format: OutputFormat,
    pub parts: Vec<Part>,
}

pub(super) struct DashSource {
    pub presentation: Presentation,
    frozen: FrozenSource,
    _workspace: files::Stage,
}

pub(super) enum Source {
    Hls(super::super::source::HlsSource),
    Dash(DashSource),
}

fn compare(a: (u64, u64), b: (u64, u64)) -> std::cmp::Ordering {
    (u128::from(a.0) * u128::from(b.1)).cmp(&(u128::from(b.0) * u128::from(a.1)))
}
pub(super) fn equal(a: (u64, u64), b: (u64, u64)) -> bool {
    compare(a, b).is_eq()
}

pub(super) fn hls(
    converter: &Converter,
    source: &super::super::source::HlsSource,
) -> Result<Presentation, ConversionError> {
    converter.backend.check_cancelled()?;
    if source.format != OutputFormat::Hls {
        return Err(ConversionError("processing-container-change-not-supported"));
    }
    let mut parts = Vec::with_capacity(source.parts.len());
    for part in &source.parts {
        converter.backend.check_cancelled()?;
        let initialization = part
            .media
            .initialization
            .as_ref()
            .ok_or(ConversionError("hls-map-required"))?;
        let mut clock = FragmentClock::read(initialization)?;
        let mut resources = Vec::with_capacity(part.media.resources.len());
        for (ordinal, (path, duration)) in part.media.resources.iter().enumerate() {
            converter.backend.check_cancelled()?;
            let (time, ticks, timescale) = clock.dash_segment(path)?;
            resources.push(Resource {
                path: path.clone(),
                duration: duration.clone(),
                dash: dash::SegmentBoundary {
                    number: u64::try_from(ordinal)
                        .map_err(|_| ConversionError("source-graph-limit"))?
                        + 1,
                    time,
                    duration: ticks,
                    timescale,
                },
            });
        }
        let (first, end, scale) = clock.presentation_window()?;
        parts.push(Part {
            initialization: initialization.clone(),
            resources,
            metadata: part.declaration.metadata.clone(),
            role: part.declaration.role,
            outer: part.media.report.protection,
            window: dash::Window {
                origin: (first, scale),
                first: (first, scale),
                end: (end, scale),
            },
            empty_cue_pair: part.media.empty_cue_pair,
        });
    }
    let origin = parts
        .iter()
        .map(|part| part.window.first)
        .min_by(|a, b| compare(*a, *b))
        .ok_or(ConversionError("track-count-limit"))?;
    for part in &mut parts {
        part.window.origin = origin;
    }
    Ok(Presentation {
        report: source.report.clone(),
        format: source.format,
        parts,
    })
}

impl DashSource {
    pub fn entrypoint(&self) -> &Path {
        self.frozen.entrypoint()
    }
    pub fn check_original(&self) -> Result<(), ConversionError> {
        self.frozen.check_original()
    }
    pub fn load(
        converter: &Converter,
        input: VerificationSource<'_>,
    ) -> Result<Self, ConversionError> {
        converter.backend.check_cancelled()?;
        let frozen = converter.capture_source(input.input)?;
        let workspace = crate::conversion::workspace()?;
        let captured = dash::snapshot_segments(
            frozen.entrypoint(),
            workspace.path(),
            &converter.backend.stop,
        )?;
        let descriptions = metadata::read_dash(&captured.manifest)?;
        if descriptions.len() != captured.representations.len() {
            return Err(ConversionError("presentation-track-count-mismatch"));
        }
        let mut parts = Vec::with_capacity(descriptions.len());
        for (ordinal, (representation, metadata)) in captured
            .representations
            .into_iter()
            .zip(descriptions)
            .enumerate()
        {
            let declared = captured
                .report
                .tracks
                .get(ordinal)
                .ok_or(ConversionError("presentation-track-count-mismatch"))?
                .kind;
            if declared != TrackKind::Unknown && declared != representation.kind {
                return Err(ConversionError("track-role-mismatch"));
            }
            let mut clock = FragmentClock::read(&representation.initialization)?;
            let resources = representation
                .resources
                .into_iter()
                .map(|(path, boundary)| {
                    converter.backend.check_cancelled()?;
                    // DASH addressing retains complete coded intervals. HLS EXTINF
                    // instead describes the visible interval, including a final AAC clip.
                    let (first, end, scale) = clock.segment_presentation(&path)?;
                    Ok(Resource {
                        path,
                        duration: crate::conversion::finite::manifest::seconds(end - first, scale)?,
                        dash: boundary,
                    })
                })
                .collect::<Result<_, ConversionError>>()?;
            clock.presentation_window()?;
            parts.push(Part {
                initialization: representation.initialization,
                resources,
                metadata,
                role: Some(representation.kind),
                outer: representation.protection,
                window: representation.window,
                empty_cue_pair: false,
            });
        }
        frozen.check_original()?;
        Ok(Self {
            presentation: Presentation {
                report: captured.report,
                format: OutputFormat::Dash,
                parts,
            },
            frozen,
            _workspace: workspace,
        })
    }
}

impl Source {
    pub fn frozen(&self) -> &FrozenSource {
        match self {
            Self::Hls(source) => source.frozen(),
            Self::Dash(source) => &source.frozen,
        }
    }
    pub fn presentation(&self, converter: &Converter) -> Result<Presentation, ConversionError> {
        converter.backend.check_cancelled()?;
        match self {
            Self::Hls(source) => hls(converter, source),
            Self::Dash(source) => Ok(source.presentation.clone()),
        }
    }
    pub fn load(
        converter: &Converter,
        input: VerificationSource<'_>,
    ) -> Result<Self, ConversionError> {
        converter.backend.check_cancelled()?;
        let report = inspection::inspect(input.input)
            .map_err(|_| ConversionError("input-inspection-failed"))?;
        match report.format {
            InputFormat::Hls => Ok(Self::Hls(super::super::source::HlsSource::load(
                converter, input,
            )?)),
            InputFormat::Dash => Ok(Self::Dash(DashSource::load(converter, input)?)),
            InputFormat::Mp4 => Err(ConversionError("processing-segments-source-required")),
        }
    }
    pub fn check_original(&self) -> Result<(), ConversionError> {
        match self {
            Self::Hls(source) => source.check_original(),
            Self::Dash(source) => source.frozen.check_original(),
        }
    }
    pub fn entrypoint(&self) -> &Path {
        match self {
            Self::Hls(source) => source.entrypoint(),
            Self::Dash(source) => source.frozen.entrypoint(),
        }
    }
}

pub(super) fn windows_equal(a: &Part, b: &Part) -> bool {
    equal(a.window.origin, b.window.origin)
        && equal(a.window.first, b.window.first)
        && equal(a.window.end, b.window.end)
}
