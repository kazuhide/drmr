//! Exact DASH timelines use decode bounds, finite presentation bounds and the source origin.
use super::{
    ConversionError, ConversionRequest, Converter, OutputPart, Path, Presentation, TrackKind,
    Zeroizing, protection, write as write_manifest, xml,
};
use std::fmt::Write as _;

fn offset(origin: (u64, u64), scale: u64) -> Result<u64, ConversionError> {
    let value = u128::from(origin.0) * u128::from(scale);
    if origin.1 == 0 || value % u128::from(origin.1) != 0 {
        return Err(ConversionError(
            "dash-presentation-origin-timescale-not-supported",
        ));
    }
    u64::try_from(value / u128::from(origin.1))
        .map_err(|_| ConversionError("dash-presentation-duration-limit"))
}

fn duration(presentation: &Presentation) -> Result<String, ConversionError> {
    let origin = presentation
        .parts
        .first()
        .ok_or(ConversionError("track-count-limit"))?
        .window
        .origin;
    let mut maximum = 0_u64;
    for part in &presentation.parts {
        if !super::super::source::equal(origin, part.window.origin) {
            return Err(ConversionError("dash-incoherent-presentation-offset"));
        }
        let end = part.window.end;
        let numerator = (u128::from(end.0) * u128::from(origin.1))
            .checked_sub(u128::from(origin.0) * u128::from(end.1))
            .ok_or(ConversionError("dash-presentation-duration-limit"))?;
        let denominator = u128::from(end.1) * u128::from(origin.1);
        let nanos = numerator
            .checked_mul(1_000_000_000)
            .ok_or(ConversionError("dash-presentation-duration-limit"))?
            .div_ceil(denominator);
        maximum = maximum.max(
            u64::try_from(nanos)
                .map_err(|_| ConversionError("dash-presentation-duration-limit"))?,
        );
    }
    if maximum == 0 {
        return Err(ConversionError("dash-presentation-duration-limit"));
    }
    Ok(format!(
        "PT{}.{:09}S",
        maximum / 1_000_000_000,
        maximum % 1_000_000_000
    ))
}

pub(in crate::conversion::segments::fmp4) fn write(
    converter: &Converter,
    root: &Path,
    source: &Presentation,
    parts: &[OutputPart],
    request: &ConversionRequest<'_>,
) -> Result<(), ConversionError> {
    let period = duration(source)?;
    let mut text = Zeroizing::new(format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<MPD xmlns=\"urn:mpeg:dash:schema:mpd:2011\" xmlns:cenc=\"urn:mpeg:cenc:2013\" xmlns:mspr=\"urn:microsoft:playready\" type=\"static\" profiles=\"urn:mpeg:dash:profile:isoff-live:2011\" minBufferTime=\"PT2S\" mediaPresentationDuration=\"{period}\"><Period id=\"0\" start=\"PT0S\" duration=\"{period}\">"
    ));
    let protection = protection(converter, request)?;
    for (ordinal, (source, output)) in source.parts.iter().zip(parts).enumerate() {
        converter.backend.check_cancelled()?;
        let inspected = crate::inspection::inspect(&output.initialization)
            .map_err(|_| ConversionError("invalid-output-media"))?;
        let kind = inspected
            .tracks
            .first()
            .ok_or(ConversionError("track-count-limit"))?
            .kind;
        let content_type = if kind == TrackKind::Audio {
            "audio"
        } else if kind == TrackKind::Video {
            "video"
        } else {
            return Err(ConversionError("track-role-mismatch"));
        };
        let first = source
            .resources
            .first()
            .ok_or(ConversionError("dash-empty-segment-timeline"))?
            .dash;
        let scale = first.timescale;
        let origin = offset(source.window.origin, scale)?;
        for declaration in &source.resources {
            if declaration.dash.timescale != scale || declaration.dash.duration == 0 {
                return Err(ConversionError("dash-invalid-timeline"));
            }
        }
        let bandwidth = output.bandwidth;
        write!(text, "<AdaptationSet id=\"{ordinal}\" contentType=\"{content_type}\" mimeType=\"{content_type}/mp4\"").map_err(|_| ConversionError("output-staging-failed"))?;
        if let Some(audio) = &source.metadata.audio {
            write!(
                text,
                " lang=\"{}\" selectionPriority=\"{}\"",
                xml(&audio.language),
                if audio.is_default.unwrap_or(false) {
                    2
                } else {
                    1
                }
            )
            .map_err(|_| ConversionError("output-staging-failed"))?;
        }
        text.push('>');
        text.push_str(&protection);
        if let Some(label) = source
            .metadata
            .audio
            .as_ref()
            .and_then(|audio| audio.name.as_ref())
        {
            write!(text, "<Label>{}</Label>", xml(label))
                .map_err(|_| ConversionError("output-staging-failed"))?;
        }
        write!(
            text,
            "<Representation id=\"{ordinal}\" bandwidth=\"{bandwidth}\" codecs=\"{}\"",
            xml(&output.codecs.join(","))
        )
        .map_err(|_| ConversionError("output-staging-failed"))?;
        if let Some((width, height)) = output.resolution {
            write!(text, " width=\"{width}\" height=\"{height}\"")
                .map_err(|_| ConversionError("output-staging-failed"))?;
        }
        write!(text, "><SegmentTemplate timescale=\"{scale}\" presentationTimeOffset=\"{origin}\" startNumber=\"{}\" initialization=\"track-{ordinal:04}/init.mp4\" media=\"track-{ordinal:04}/seg_$Number$.m4s\"><SegmentTimeline>", first.number).map_err(|_| ConversionError("output-staging-failed"))?;
        for resource in &source.resources {
            write!(
                text,
                "<S t=\"{}\" d=\"{}\"/>",
                resource.dash.time, resource.dash.duration
            )
            .map_err(|_| ConversionError("output-staging-failed"))?;
        }
        text.push_str("</SegmentTimeline></SegmentTemplate></Representation></AdaptationSet>");
        if text.len() as u64 > crate::inspection::MAX_MANIFEST_BYTES {
            return Err(ConversionError("manifest-size-limit"));
        }
    }
    text.push_str("</Period></MPD>\n");
    write_manifest(&root.join("manifest.mpd"), &text)
}
