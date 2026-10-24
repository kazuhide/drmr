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
    let streams = backend.transport_streams(&localized.combined, admission, &localized.native)?;
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
        localized
            .native
            .get(&pid)
            .ok_or(ConversionError("ts-original-evidence-required"))?,
        &localized.transport_segments,
    )?;
    streams
        .into_iter()
        .map(|(index, kind)| {
            let pid = admission
                .tracks
                .get(index)
                .ok_or(ConversionError("ts-stream-count-mismatch"))?
                .pid;
            let native = localized
                .native
                .get(&pid)
                .cloned()
                .ok_or(ConversionError("ts-original-evidence-required"))?;
            native.validate()?;
            native.metadata()?;
            native.timing()?;
            native.evidence()?;
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
                source: localized.combined.clone(),
                media: localized.combined.clone(),
                index,
                report,
                key_id: None,
                transport_index: Some(index),
                transport_demux_view: None,
                transport_native: Some(native),
                transport_sample: localized.sample_transport,
                playlist_reference: None,
                dash_reference: false,
                audio_presentation: audio_presentation.filter(|_| kind == TrackKind::Audio),
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
            self.backend.native_verify_transport(
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
            )?
        } else {
            self.backend.native_verify_views(
                StreamView::new(&a.source, a.index, key_for(a, source_keys))
                    .with_metadata(&a.media)
                    .with_all_keys(source_keys),
                StreamView::new(&b.source, b.index, key_for(b, result_keys))
                    .with_metadata(&b.media)
                    .with_all_keys(result_keys),
            )?
        };
        report.checks.packet_timing_equal &= a.audio_presentation == b.audio_presentation;
        report.refresh_passed();
        Ok(report)
    }
}
