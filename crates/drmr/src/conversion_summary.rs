//! Completion facts from successful reports and explicit local destinations.
use crate::download_cli::Input;
use drmr_core::conversion::{
    ConversionError, ConversionReport, ProcessingMode,
    plan::bundle::{BundleConversionReport, BundleOutputIntent},
};
use drmr_core::inspection::{
    HlsKeyMethod, InputFormat, InputPreparation, OutputFormat, ProcessingRouteId, TargetProtection,
};
use indicatif::HumanBytes;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

pub(crate) enum Source {
    Local(PathBuf),
    Retained(PathBuf),
}

impl Source {
    pub(crate) fn direct(input: &Input, download: Option<&Path>) -> Self {
        match input {
            Input::Local(path) => Self::Local(path.clone()),
            Input::Url(_) => Self::Retained(
                download
                    .expect("successful URL conversion retained its download directory")
                    .to_path_buf(),
            ),
        }
    }
}

struct Output {
    id: Option<String>,
    format: OutputFormat,
    target: TargetProtection,
    destination: PathBuf,
    processing: Option<ProcessingMode>,
}

pub(crate) struct Summary {
    source: Source,
    input_format: &'static str,
    input_protection: &'static str,
    outputs: Vec<Output>,
}

impl Summary {
    pub(crate) fn single(
        report: &ConversionReport,
        source: Source,
        destination: &Path,
        format: OutputFormat,
        target: TargetProtection,
    ) -> Self {
        Self::new(
            report,
            source,
            vec![Output {
                id: None,
                format,
                target,
                destination: destination.to_path_buf(),
                processing: report.processing.as_ref().and_then(|p| p.mode),
            }],
        )
    }

    pub(crate) fn bundle(
        report: &BundleConversionReport,
        source: Source,
        root: &Path,
        intents: &[BundleOutputIntent],
    ) -> Result<Self, ConversionError> {
        let missing = ConversionError("result-output-unavailable");
        let first = report.outputs.first().ok_or(missing)?;
        if report.outputs.len() != intents.len() {
            return Err(missing);
        }
        let outputs = report
            .outputs
            .iter()
            .map(|output| {
                let intent = intents.iter().find(|i| i.id == output.id).ok_or(missing)?;
                Ok(Output {
                    id: Some(intent.id.clone()),
                    format: intent.format,
                    target: intent.target,
                    // Every named result has its own directory, including MP4 outputs.
                    destination: root.join(&intent.id),
                    processing: output.conversion.processing.as_ref().and_then(|p| p.mode),
                })
            })
            .collect::<Result<Vec<_>, ConversionError>>()?;
        Ok(Self::new(&first.conversion, source, outputs))
    }

    fn new(report: &ConversionReport, source: Source, outputs: Vec<Output>) -> Self {
        let format = input_format(report);
        let method = report.input.details.as_ref().and_then(|details| {
            let tracks = &details.track_protection;
            let first = tracks.first()?.hls_key_methods.first().copied()?;
            tracks
                .iter()
                .all(|track| track.hls_key_methods == [first])
                .then_some(first)
        });
        let protection = if format == "HLS MPEG-TS" {
            match method {
                Some(HlsKeyMethod::SampleAes) => "SAMPLE-AES",
                Some(HlsKeyMethod::Aes128) => "AES-128",
                _ => report.input.protection.as_str(),
            }
        } else {
            report.input.protection.as_str()
        };
        Self {
            source,
            input_format: format,
            input_protection: protection,
            outputs,
        }
    }

    pub(crate) fn write(
        &self,
        out: &mut impl Write,
        elapsed: Duration,
        downloaded: Option<[u64; 2]>,
    ) -> io::Result<()> {
        writeln!(out, "\nSummary")?;
        writeln!(
            out,
            "  Input: {} / {}",
            self.input_format, self.input_protection
        )?;
        match &self.source {
            Source::Local(path) => writeln!(out, "  Source: {}", path_text(path))?,
            Source::Retained(path) => {
                writeln!(out, "  Retained originals: {}", path_text(path))?;
            }
        }
        for output in &self.outputs {
            write!(out, "  Output")?;
            if let Some(id) = &output.id {
                write!(out, " {}", escaped(id))?;
            }
            writeln!(
                out,
                ": {} / {}",
                output_format(output.format),
                output.target.as_str()
            )?;
            writeln!(out, "    Destination: {}", path_text(&output.destination))?;
            if let Some(processing) = output.processing {
                writeln!(out, "    Processing: {}", processing.as_str())?;
            }
        }
        write!(
            out,
            "  Stats: {} {}",
            self.outputs.len(),
            if self.outputs.len() == 1 {
                "output"
            } else {
                "outputs"
            },
        )?;
        if let Some([files, bytes]) = downloaded {
            write!(
                out,
                "; {files} {} downloaded; {} received",
                if files == 1 { "file" } else { "files" },
                HumanBytes(bytes)
            )?;
        }
        writeln!(out)?;
        writeln!(out, "  Elapsed: {}", crate::progress::duration(elapsed))
    }
}

fn input_format(report: &ConversionReport) -> &'static str {
    match report.input.format {
        InputFormat::Mp4 => "MP4",
        InputFormat::Dash => "DASH",
        InputFormat::Hls => match report.processing.as_ref() {
            Some(processing)
                if matches!(
                    processing.route,
                    ProcessingRouteId::MpegTsHls
                        | ProcessingRouteId::MpegTsDash
                        | ProcessingRouteId::MpegTsSampleAesHls
                        | ProcessingRouteId::MpegTsHlsTs
                        | ProcessingRouteId::MpegTsSegments
                ) || matches!(
                    processing.input_preparation,
                    InputPreparation::MpegTsDemux | InputPreparation::MpegTsSampleAesDecrypt
                ) =>
            {
                "HLS MPEG-TS"
            }
            _ => "HLS",
        },
    }
}

fn output_format(format: OutputFormat) -> &'static str {
    match format {
        OutputFormat::Mp4 => "MP4",
        OutputFormat::Dash => "DASH fMP4",
        OutputFormat::Hls => "HLS fMP4",
        OutputFormat::HlsTs => "HLS MPEG-TS",
    }
}

fn path_text(path: &Path) -> String {
    escaped(&path.to_string_lossy())
}

fn escaped(text: &str) -> String {
    use std::fmt::Write as _;
    let mut safe = String::with_capacity(text.len());
    for c in text.chars() {
        if c.is_control()
            || matches!(c, '\u{2028}' | '\u{2029}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
        {
            write!(safe, "{}", c.escape_default()).expect("string write");
        } else {
            safe.push(c);
        }
    }
    safe
}

#[cfg(test)]
mod tests {
    use super::*;
    use drmr_core::conversion::{
        CheckOutcome, DecodeEvidence, ValidationDepth, VerificationChecks, VerificationReport,
        VerificationScope,
        plan::bundle::{BundleConversionOutput, DrmSignalingReport, ManifestValidation},
    };
    use drmr_core::inspection::{
        ActualProcessing, Evidence, Inspection, Protection, Track, TrackKind,
    };

    fn report() -> ConversionReport {
        let input = Inspection::new(
            InputFormat::Hls,
            Evidence::Manifest,
            vec![Track {
                id: "metadata-secret".into(),
                kind: TrackKind::Video,
                codec: Some("codec-secret".into()),
                protection: Protection::Clear,
                key_ids: vec!["kid-secret".into()],
                pattern: None,
            }],
            vec!["drm-secret".into()],
            vec![],
        );
        ConversionReport {
            schema_version: 2,
            output: input.clone(),
            input,
            verification: VerificationReport::new(
                ValidationDepth::Native,
                VerificationScope::Comparison,
                VerificationChecks {
                    stream_metadata: CheckOutcome::Pass,
                    packet_payloads_equal: true,
                    packet_timing_equal: true,
                },
                DecodeEvidence::not_requested(),
            ),
            processing: Some(ActualProcessing::new(
                ProcessingRouteId::MpegTsHlsTs,
                InputPreparation::MpegTsDemux,
            )),
        }
    }

    #[test]
    fn url_and_metadata_secrets_never_enter_typed_completion_facts() {
        let input = Input::Url("https://user:password@origin.invalid/private?url-secret".into());
        let summary = Summary::single(
            &report(),
            Source::direct(&input, Some(Path::new("retained"))),
            Path::new("result"),
            OutputFormat::HlsTs,
            TargetProtection::HlsSampleAes,
        );
        let mut bytes = Vec::new();
        summary
            .write(&mut bytes, Duration::from_secs(75), Some([7, 1024]))
            .unwrap();
        let text = String::from_utf8(bytes).unwrap();
        assert!(text.contains("Input: HLS MPEG-TS / clear"), "{text}");
        assert!(
            text.contains("Output: HLS MPEG-TS / hls-sample-aes"),
            "{text}"
        );
        assert!(text.contains("Retained originals: retained"), "{text}");
        assert!(text.contains("Elapsed: 1m 15s"), "{text}");
        assert!(
            text.contains("7 files downloaded; 1.00 KiB received"),
            "{text}"
        );
        for forbidden in ["secret", "password", "origin.invalid", "https://", "KID"] {
            assert!(!text.contains(forbidden), "{text}");
        }
    }

    #[test]
    fn named_output_paths_follow_ids_even_if_recipe_order_differs() {
        let report = BundleConversionReport {
            kind: "bundle-conversion",
            schema_version: 2,
            plan_id: "plan-secret".into(),
            outputs: ["hls", "dash"]
                .map(|id| BundleConversionOutput {
                    id: id.into(),
                    conversion: report(),
                    signaling: DrmSignalingReport {
                        systems: vec![],
                        verified: false,
                    },
                    conformance: None,
                })
                .into(),
        };
        let intents =
            [("dash", OutputFormat::Dash), ("hls", OutputFormat::Hls)].map(|(id, format)| {
                BundleOutputIntent {
                    id: id.into(),
                    format,
                    target: TargetProtection::Clear,
                    drms: vec![],
                    conformance: ManifestValidation::None,
                    processing: None,
                    dash_addressing: None,
                }
            });
        let source = || Source::Local(PathBuf::from("source/master.m3u8"));
        let summary = Summary::bundle(&report, source(), Path::new("result"), &intents).unwrap();
        let mut bytes = Vec::new();
        summary
            .write(&mut bytes, Duration::from_secs(2), None)
            .unwrap();
        let text = String::from_utf8(bytes).unwrap();
        for (id, format) in [("hls", "HLS fMP4"), ("dash", "DASH fMP4")] {
            assert!(
                text.contains(&format!("Output {id}: {format} / clear")),
                "{text}"
            );
            assert!(
                text.contains(&path_text(&Path::new("result").join(id))),
                "{text}"
            );
        }
        assert!(text.contains("Stats: 2 outputs"), "{text}");
        assert!(
            !text.contains("downloaded") && !text.contains("received"),
            "{text}"
        );
        assert!(!text.contains("secret"), "{text}");
        assert_eq!(
            Summary::bundle(&report, source(), Path::new("result"), &intents[..1]).err(),
            Some(ConversionError("result-output-unavailable")),
        );
    }

    #[test]
    fn local_paths_preserve_text_and_escape_terminal_controls() {
        let text = path_text(Path::new("日本語 folder/C:\\media\n\r\t\u{1b}[2J\u{202e}"));
        assert_eq!(text, "日本語 folder/C:\\media\\n\\r\\t\\u{1b}[2J\\u{202e}");
        assert!(!text.chars().any(char::is_control));
    }
}
