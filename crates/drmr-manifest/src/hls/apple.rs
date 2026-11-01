use super::{
    apple_rule, check,
    syntax::{self, Attribute, Line},
};
use crate::{CheckStatus, DiagnosticSink, NotCheckedReason, Severity, Span};
use std::collections::BTreeSet;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Audio,
    Video,
    Unknown,
}

fn kind(attributes: &[Attribute<'_>]) -> Kind {
    let codecs = syntax::get(attributes, "CODECS");
    let video = |codec: &str| {
        matches!(
            codec.split('.').next(),
            Some("avc1" | "avc3" | "hev1" | "hvc1" | "dvh1" | "dvhe" | "av01" | "mjpg")
        )
    };
    if syntax::get(attributes, "RESOLUTION").is_some()
        || syntax::get(attributes, "VIDEO").is_some()
        || codecs.is_some_and(|codecs| codecs.value.split(',').any(video))
    {
        return Kind::Video;
    }
    let audio = |codec: &str| {
        matches!(
            codec.split('.').next(),
            Some(
                "mp4a"
                    | "ac-3"
                    | "ec-3"
                    | "ac-4"
                    | "alac"
                    | "fLaC"
                    | "flac"
                    | "Opus"
                    | "opus"
                    | "apac"
            )
        )
    };
    if codecs.is_some_and(|codecs| !codecs.value.is_empty() && codecs.value.split(',').all(audio)) {
        Kind::Audio
    } else {
        Kind::Unknown
    }
}

pub(super) fn is_video(attributes: &[Attribute<'_>]) -> bool {
    kind(attributes) == Kind::Video
}

pub(super) fn variant(
    attributes: &[Attribute<'_>],
    line: Line<'_>,
    iframe: bool,
    sink: &mut DiagnosticSink<'_>,
) {
    let present = |name| syntax::get(attributes, name).is_some();
    if syntax::get(attributes, "CODECS").is_some_and(|attribute| {
        attribute
            .value
            .split(',')
            .any(|codec| codec.trim().split('.').next() == Some("vp09"))
    }) {
        check(
            sink,
            apple_rule(
                "APPLE-HLS-VIDEO-CODEC",
                "apple-hls-video-codec-not-supported",
                "1.1",
            ),
            false,
            line.span,
        );
    }
    let video = kind(attributes);
    let valid = present("CODECS")
        && (!(iframe || video == Kind::Video) || present("RESOLUTION"))
        && (iframe || video != Kind::Video || present("FRAME-RATE"));
    check(
        sink,
        apple_rule(
            "APPLE-HLS-VARIANT-ATTRIBUTES",
            "apple-hls-variant-attributes-required",
            "9.1-9.4,9.15",
        ),
        valid,
        line.span,
    );
    let average = if iframe {
        apple_rule(
            "APPLE-HLS-IFRAME-AVERAGE-BANDWIDTH",
            "apple-hls-iframe-average-bandwidth-required",
            "9.14",
        )
    } else {
        apple_rule(
            "APPLE-HLS-AVERAGE-BANDWIDTH",
            "apple-hls-average-bandwidth-required",
            "9.14",
        )
    };
    check(sink, average, present("AVERAGE-BANDWIDTH"), line.span);
    if video == Kind::Unknown && !iframe && present("CODECS") {
        sink.check(
            apple_rule(
                "APPLE-HLS-VIDEO-CONDITIONS",
                "apple-hls-video-conditions-unknown",
                "9.2,9.9,9.15",
            ),
            CheckStatus::NotChecked(NotCheckedReason::UnsupportedFeature),
            Some(line.span),
        );
    }
}

pub(super) fn rendition(
    attributes: &[Attribute<'_>],
    line: Line<'_>,
    sink: &mut DiagnosticSink<'_>,
) {
    let value = |name| syntax::get(attributes, name).map(|attribute| attribute.value);
    if value("TYPE") != Some("VIDEO") {
        check(
            sink,
            apple_rule(
                "APPLE-HLS-LANGUAGE",
                "apple-hls-rendition-language-required",
                "8.10",
            ),
            value("LANGUAGE").is_some_and(|language| !language.is_empty()),
            line.span,
        );
    }
    if value("TYPE") == Some("AUDIO")
        && value("CHARACTERISTICS").is_some_and(|value| {
            value
                .split(',')
                .any(|item| item == "public.accessibility.describes-video")
        })
    {
        let valid = value("AUTOSELECT") == Some("YES")
            && value("LANGUAGE").is_some_and(|language| !language.is_empty());
        check(
            sink,
            apple_rule(
                "APPLE-HLS-AUDIO-DESCRIPTION",
                "apple-hls-audio-description-selection-invalid",
                "2.13,2.27",
            ),
            valid,
            line.span,
        );
    }
}

#[derive(Default)]
pub(super) struct VideoVariants {
    rates: BTreeSet<u64>,
    highest: Option<(u128, u64, u64)>,
    highest_rates: BTreeSet<u64>,
}

impl VideoVariants {
    pub fn record(&mut self, attributes: &[Attribute<'_>]) {
        let Some(rate) = syntax::get(attributes, "BANDWIDTH")
            .and_then(|attribute| syntax::integer(attribute.value))
        else {
            return;
        };
        self.rates.insert(rate);
        let resolution = syntax::get(attributes, "RESOLUTION")
            .and_then(|attribute| attribute.value.split_once('x'))
            .and_then(|(width, height)| Some((syntax::integer(width)?, syntax::integer(height)?)));
        let Some((width, height)) = resolution else {
            return;
        };
        let resolution = (u128::from(width) * u128::from(height), width, height);
        if self.highest.is_none_or(|previous| resolution > previous) {
            self.highest = Some(resolution);
            self.highest_rates.clear();
        }
        if self.highest == Some(resolution) {
            self.highest_rates.insert(rate);
        }
    }

    pub fn finish(&self, span: Span, sink: &mut DiagnosticSink<'_>) {
        if self.rates.is_empty() {
            return;
        }
        check(
            sink,
            apple_rule(
                "APPLE-HLS-VARIANTS",
                "apple-hls-multiple-video-variants-required",
                "9.9",
            ),
            self.rates.len() > 1,
            span,
        );
        if self
            .highest
            .is_some_and(|(_, width, height)| width.max(height) >= 1280 && width.min(height) >= 720)
        {
            let mut requirement = apple_rule(
                "APPLE-HLS-HD-VARIANTS",
                "apple-hls-highest-resolution-variants-recommended",
                "9.10",
            );
            requirement.severity = Severity::Warning;
            check(sink, requirement, self.highest_rates.len() > 1, span);
        }
    }
}
