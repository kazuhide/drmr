//! Source-relative resource placement without URL values in diagnostics or saved plans.
use super::ConversionError;
use serde::{Deserialize, Serialize};
mod paths;
pub use paths::{UrlPathMapper, path_text, path_uri};
mod selection;
pub use selection::{SourceLayout, SourceNames, SourceTrack, TrackPlacement};
mod naming;
pub use naming::SegmentNames;
mod grouping;
mod source;
mod stage;
pub(in crate::conversion) use stage::RelocatedTemplate;

/// Native admission's ordered selection; excluded from public reports.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct QualifiedTrack {
    pub kind: crate::inspection::TrackKind,
    pub selector: usize,
}

/// Directory placement for retained originals and newly generated media.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LayoutMode {
    /// Use fixed generated directories, including for existing saved plans.
    #[default]
    Normalized,
    /// Retain the source-relative directory layout where it is unambiguous and confined.
    Source,
}

const INVALID: ConversionError = ConversionError("source-layout-path-unsafe");
const COLLISION: ConversionError = ConversionError("source-layout-collision");
const UNSUPPORTED: ConversionError = ConversionError("source-layout-not-supported");

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn source_paths_keep_nested_directories_and_strip_query_values() {
        let mut mapper = UrlPathMapper::new(
            "https://example.invalid/vod/master.m3u8",
            LayoutMode::Source,
        )
        .unwrap();
        let path = mapper
            .map(
                "https://example.invalid/vod/video/1080p/seg_0.ts?token=private",
                "media/r000000.ts",
            )
            .unwrap();
        assert_eq!(path, Path::new("video/1080p/seg_0.ts"));
    }

    #[test]
    fn normalized_paths_keep_the_explicit_generated_name() {
        let mut mapper = UrlPathMapper::new(
            "https://example.invalid/vod/master.m3u8",
            LayoutMode::Normalized,
        )
        .unwrap();
        assert_eq!(
            mapper
                .map("https://cdn.invalid/other/segment.ts", "media/r000000.ts")
                .unwrap(),
            Path::new("media/r000000.ts")
        );
    }

    #[test]
    fn relative_paths_are_percent_encoded_without_encoding_directory_separators() {
        assert_eq!(
            path_uri(Path::new("video/a b/音声.m4s")).unwrap(),
            "video/a%20b/%E9%9F%B3%E5%A3%B0.m4s"
        );
    }

    #[test]
    fn query_aliases_and_case_aliases_never_share_a_local_file() {
        for other in [
            "video/seg.ts?token=other",
            "Video/seg.ts?token=first",
            "video/SEG.ts?token=first",
        ] {
            let mut mapper = UrlPathMapper::new(
                "https://example.invalid/vod/master.m3u8",
                LayoutMode::Source,
            )
            .unwrap();
            let first = "https://example.invalid/vod/video/seg.ts?token=first";
            let expected = mapper.map(first, "unused").unwrap();
            assert_eq!(mapper.map(first, "different-unused").unwrap(), expected);
            assert_eq!(
                mapper
                    .map(&format!("https://example.invalid/vod/{other}"), "unused")
                    .unwrap_err(),
                COLLISION
            );
        }
    }

    #[test]
    fn directories_and_file_names_cannot_overlap() {
        for names in [["video/seg.ts", "video"], ["video", "video/seg.ts"]] {
            let mut mapper = UrlPathMapper::new(
                "https://example.invalid/vod/master.m3u8",
                LayoutMode::Source,
            )
            .unwrap();
            mapper
                .map(
                    &format!("https://example.invalid/vod/{}", names[0]),
                    "unused",
                )
                .unwrap();
            assert_eq!(
                mapper
                    .map(
                        &format!("https://example.invalid/vod/{}", names[1]),
                        "unused"
                    )
                    .unwrap_err(),
                COLLISION
            );
        }
    }

    #[test]
    fn source_paths_reject_escapes_unsafe_encodings_and_reserved_names() {
        for path in [
            "../escape",
            "%2e%2e/escape",
            "video%2fescape.ts",
            "video%5cescape.ts",
            "bad%GG.ts",
            "%00.ts",
            "%ff.ts",
            "CON.ts",
            "video/trailing.",
            "raw/index.json",
            "RAW/index.json",
            "Raw",
            "input.m3u8",
            "Input.M3U8",
            "INPUT.MPD",
            "Input.mp4",
        ] {
            let mut mapper = UrlPathMapper::new(
                "https://example.invalid/vod/master.m3u8",
                LayoutMode::Source,
            )
            .unwrap();
            assert!(
                mapper
                    .map(&format!("https://example.invalid/vod/{path}"), "unused")
                    .is_err(),
                "accepted {path}"
            );
        }
        let mut mapper = UrlPathMapper::new(
            "https://example.invalid/vod/master.m3u8",
            LayoutMode::Source,
        )
        .unwrap();
        assert_eq!(
            mapper
                .map("https://example.invalid/outside/seg.ts", "unused")
                .unwrap_err(),
            UNSUPPORTED
        );
    }

    #[test]
    fn unicode_composition_aliases_cannot_create_distinct_files() {
        let mut mapper = UrlPathMapper::new(
            "https://example.invalid/vod/master.m3u8",
            LayoutMode::Source,
        )
        .unwrap();
        mapper
            .map("https://example.invalid/vod/caf%C3%A9/seg.ts", "unused")
            .unwrap();
        assert_eq!(
            mapper
                .map("https://example.invalid/vod/cafe%CC%81/other.ts", "unused")
                .unwrap_err(),
            COLLISION
        );
    }

    #[test]
    fn distinct_origins_still_require_unique_source_relative_paths() {
        let mut mapper = UrlPathMapper::new(
            "https://example.invalid/vod/master.m3u8",
            LayoutMode::Source,
        )
        .unwrap();
        assert_eq!(
            mapper
                .map("https://cdn.invalid/vod/video/720p/segment.ts", "unused")
                .unwrap(),
            Path::new("video/720p/segment.ts")
        );
        assert_eq!(
            mapper
                .map("https://example.invalid/vod/audio/en/segment.ts", "unused")
                .unwrap(),
            Path::new("audio/en/segment.ts")
        );
        assert_eq!(
            mapper
                .map("https://other.invalid/vod/video/720p/segment.ts", "unused")
                .unwrap_err(),
            COLLISION
        );
    }
}
