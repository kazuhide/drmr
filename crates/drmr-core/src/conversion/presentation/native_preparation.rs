//! Inspection and independent verification never require a normalized demux.
use super::{
    ConversionError, Converter, Evidence, InputFormat, InputKeys, Inspection, Prepared, Protection,
    SourceCompatibilityNote, SourceProtection, StreamView, TrackKind, VerificationReport, hls,
    inspection, key_for, metadata,
};

pub(super) fn transport(
    localized: &hls::Localized,
    admission: &super::super::ts::Admission,
    backend: &super::super::backend::Backend,
) -> Result<Vec<Prepared>, ConversionError> {
    transport_view(
        &localized.combined,
        &localized.transport_segments,
        &localized.native,
        localized.sample_transport,
        admission,
        backend,
    )
}

pub(super) fn transport_segments(
    localized: &hls::Segmented,
    admission: &super::super::ts::Admission,
    backend: &super::super::backend::Backend,
) -> Result<Vec<Prepared>, ConversionError> {
    if backend.validation_depth != super::super::ValidationDepth::Native {
        return Err(ConversionError("native-view-requires-native-validation"));
    }
    transport_view(
        &localized.reference,
        &localized.resources,
        &localized.native,
        localized.sample_transport,
        admission,
        backend,
    )
}

fn transport_view(
    source: &std::path::Path,
    resources: &[(std::path::PathBuf, String)],
    anchors: &std::collections::BTreeMap<u16, std::sync::Arc<super::super::ts::NativeEvidence>>,
    sample_transport: bool,
    admission: &super::super::ts::Admission,
    backend: &super::super::backend::Backend,
) -> Result<Vec<Prepared>, ConversionError> {
    let streams = backend.transport_streams(source, admission, anchors)?;
    let clock_index = streams
        .iter()
        .find(|(_, kind)| *kind == TrackKind::Video)
        .unwrap_or(&streams[0])
        .0;
    let pid = admission
        .tracks
        .get(clock_index)
        .ok_or(ConversionError("ts-stream-count-mismatch"))?
        .pid;
    let audio_presentation = super::super::native::durations::transport(
        anchors
            .get(&pid)
            .ok_or(ConversionError("ts-original-evidence-required"))?,
        resources,
    )?;
    streams
        .into_iter()
        .map(|(index, kind)| {
            let pid = admission
                .tracks
                .get(index)
                .ok_or(ConversionError("ts-stream-count-mismatch"))?
                .pid;
            let native = anchors
                .get(&pid)
                .cloned()
                .ok_or(ConversionError("ts-original-evidence-required"))?;
            native.validate()?;
            native.metadata()?;
            native.timing()?;
            native.evidence()?;
            let clock = super::super::native::ManifestAacClock::from_transport(&native)?;
            let report = Inspection::new(
                InputFormat::Mp4,
                Evidence::ContainerMetadata,
                vec![inspection::Track {
                    id: format!("pid-{pid}"),
                    kind,
                    codec: Some(
                        if kind == TrackKind::Video {
                            "avc1"
                        } else {
                            "mp4a"
                        }
                        .into(),
                    ),
                    protection: Protection::Clear,
                    key_ids: Vec::new(),
                    pattern: None,
                }],
                Vec::new(),
                Vec::new(),
            );
            Ok(Prepared {
                metadata: metadata::Metadata::default(),
                outer_protection: SourceProtection::Clear,
                compatibility: if admission.has_empty_payload_padding() {
                    vec![SourceCompatibilityNote::TsEmptyPayloadPadding]
                } else {
                    Vec::new()
                },
                source_dictionary: false,
                source: source.to_path_buf(),
                media: source.to_path_buf(),
                index,
                report,
                key_id: None,
                transport_index: Some(index),
                transport_demux_view: None,
                transport_native: Some(native),
                transport_sample: sample_transport,
                playlist_reference: None,
                dash_reference: false,
                audio_presentation: audio_presentation.filter(|_| kind == TrackKind::Audio),
                clock,
                hls_target_seconds: None,
                hls_video_segment_ms: None,
            })
        })
        .collect()
}

impl Converter {
    pub(super) fn native_prepared_pair(
        &self,
        a: &Prepared,
        b: &Prepared,
        source_keys: &InputKeys,
        result_keys: &InputKeys,
    ) -> Result<VerificationReport, ConversionError> {
        let mut report = if a.transport_index.is_some() || b.transport_index.is_some() {
            self.backend.native_verify_transport_with_clock(
                super::super::ts::View {
                    input: &a.source,
                    metadata: &a.media,
                    index: a.transport_index.unwrap_or(a.index),
                    key: key_for(a, source_keys),
                    all_keys: None,
                    transport: a.transport_index.is_some(),
                    native: a.transport_native.as_deref(),
                    unedited: a.audio_presentation.is_some(),
                }
                .with_all_keys(source_keys),
                super::super::ts::View {
                    input: &b.source,
                    metadata: &b.media,
                    index: b.transport_index.unwrap_or(b.index),
                    key: key_for(b, result_keys),
                    all_keys: None,
                    transport: b.transport_index.is_some(),
                    native: b.transport_native.as_deref(),
                    unedited: b.audio_presentation.is_some(),
                }
                .with_all_keys(result_keys),
                a.clock,
                b.clock,
            )?
        } else {
            self.backend.native_verify_views(
                StreamView::new(&a.source, a.index, key_for(a, source_keys))
                    .with_metadata(&a.media)
                    .with_clock(a.clock)
                    .with_all_keys(source_keys),
                StreamView::new(&b.source, b.index, key_for(b, result_keys))
                    .with_metadata(&b.media)
                    .with_clock(b.clock)
                    .with_all_keys(result_keys),
            )?
        };
        report.checks.packet_timing_equal &= a.audio_presentation == b.audio_presentation;
        report.refresh_passed();
        Ok(report)
    }
}
