use super::{
    check, references, rule, start,
    syntax::{self, Line},
};
use crate::{CheckStatus, DiagnosticSink, NotCheckedReason};

pub(super) fn lint(
    lines: &[Line<'_>],
    sink: &mut DiagnosticSink<'_>,
) -> (u64, Option<u64>, Option<[u8; 32]>) {
    let iframe = lines.iter().any(|line| line.text == "#EXT-X-I-FRAMES-ONLY");
    let mut minimum = 1;
    let mut version = None;
    let mut start_value = None;
    let mut seen = std::collections::BTreeSet::new();
    for &line in lines {
        let (name, value) = line.tag();
        if [
            "#EXTM3U",
            "#EXT-X-VERSION",
            "#EXT-X-TARGETDURATION",
            "#EXT-X-MEDIA-SEQUENCE",
            "#EXT-X-DISCONTINUITY-SEQUENCE",
            "#EXT-X-ENDLIST",
            "#EXT-X-PLAYLIST-TYPE",
            "#EXT-X-I-FRAMES-ONLY",
            "#EXT-X-INDEPENDENT-SEGMENTS",
            "#EXT-X-START",
        ]
        .contains(&name)
        {
            check(
                sink,
                rule("HLS-TAG-CARDINALITY", "hls-singleton-tag-duplicated", "4.3"),
                seen.insert(name),
                line.span,
            );
        }
        match name {
            "#EXT-X-VERSION" => {
                version = value.and_then(syntax::integer);
                check(
                    sink,
                    rule("HLS-VERSION-VALUE", "hls-version-invalid", "4.3.1.2"),
                    version.is_some_and(|number| number > 0),
                    line.span,
                );
            }
            "#EXTINF"
                if value
                    .and_then(|value| value.split_once(','))
                    .is_some_and(|(duration, _)| duration.contains('.')) =>
            {
                minimum = minimum.max(3);
            }
            "#EXT-X-BYTERANGE" | "#EXT-X-I-FRAMES-ONLY" => minimum = minimum.max(4),
            "#EXT-X-MAP" => minimum = minimum.max(if iframe { 5 } else { 6 }),
            "#EXT-X-START" => start_value = start(line, sink),
            name if line.text.starts_with("#EXT")
                && !references::is_master_tag(name)
                && !references::is_media_tag(name)
                && !matches!(
                    name,
                    "#EXTM3U" | "#EXT-X-VERSION" | "#EXT-X-INDEPENDENT-SEGMENTS" | "#EXT-X-START"
                ) =>
            {
                sink.check(
                    rule("HLS-UNKNOWN-TAGS", "hls-extension-tag-not-checked", "4.3"),
                    CheckStatus::NotChecked(NotCheckedReason::UnsupportedFeature),
                    Some(line.span),
                );
            }
            _ => {}
        }
        if matches!(
            name,
            "#EXTM3U"
                | "#EXT-X-ENDLIST"
                | "#EXT-X-INDEPENDENT-SEGMENTS"
                | "#EXT-X-I-FRAMES-ONLY"
                | "#EXT-X-DISCONTINUITY"
        ) {
            check(
                sink,
                rule("HLS-TAG-SYNTAX", "hls-tag-value-unexpected", "4.3"),
                value.is_none(),
                line.span,
            );
        }
    }

    (minimum, version, start_value)
}
