//! Preserve a finite audio presentation by muxing its original progressive sample tables.
//! Fragmenting first can discard the distinction between coded padding and presented samples.
use super::{
    ConversionError, ConversionReport, Converter, VerificationChecks, VerificationReport, audio,
};
use crate::{
    inspection::{Inspection, Protection, TrackKind},
    mp4::{self, Mp4Box},
};
use std::path::Path;

const UNSUPPORTED: ConversionError = ConversionError("progressive-mp4-profile-not-supported");
const WINDOW_UNSUPPORTED: ConversionError = ConversionError("presentation-window-not-supported");
#[derive(PartialEq, Eq)]
struct Layout {
    movie_timescale: u32,
    movie_duration: u64,
    video_timescale: Option<u32>,
    windows: Vec<Window>,
}
#[derive(PartialEq, Eq)]
struct Window {
    audio: bool,
    timescale: u32,
    media_time: i64,
    duration: u64,
    display_extent: Option<(u32, u32)>,
}
#[derive(Debug, PartialEq, Eq)]
struct Edit {
    duration: u64,
    media_time: i64,
}
fn child(node: &Mp4Box, kind: [u8; 4]) -> Result<&Mp4Box, ConversionError> {
    let mut found = node.children.iter().filter(|node| node.kind() == kind);
    let value = found.next().ok_or(UNSUPPORTED)?;
    if found.next().is_some() {
        return Err(UNSUPPORTED);
    }
    Ok(value)
}
fn integer(data: &[u8], offset: usize) -> Result<u32, ConversionError> {
    data.get(offset..offset + 4)
        .and_then(|v| v.try_into().ok())
        .map(u32::from_be_bytes)
        .ok_or(UNSUPPORTED)
}
fn duration(data: &[u8], version_zero: usize, version_one: usize) -> Result<u64, ConversionError> {
    match data.first() {
        Some(0) => integer(data, version_zero).map(u64::from),
        Some(1) => data
            .get(version_one..version_one + 8)
            .and_then(|v| v.try_into().ok())
            .map(u64::from_be_bytes)
            .ok_or(UNSUPPORTED),
        _ => Err(UNSUPPORTED),
    }
}
fn clock(node: &Mp4Box, data: &[u8]) -> Result<(u32, u64), ConversionError> {
    let payload = node.header.payload(data);
    let offset = match payload.first() {
        Some(0) => 12,
        Some(1) => 20,
        _ => return Err(UNSUPPORTED),
    };
    let scale = integer(payload, offset)?;
    if scale == 0 || scale > i32::MAX as u32 || payload.get(1..4) != Some(&[0, 0, 0]) {
        return Err(UNSUPPORTED);
    }
    Ok((scale, duration(payload, 16, 24)?))
}
fn media_ticks(value: u64, movie_scale: u32, media_scale: u32) -> Result<u64, ConversionError> {
    let product = u128::from(value) * u128::from(media_scale);
    if product % u128::from(movie_scale) != 0 {
        return Err(UNSUPPORTED);
    }
    u64::try_from(product / u128::from(movie_scale)).map_err(|_| UNSUPPORTED)
}
/// One ordinary rate-1 edit. Empty edits, multiple windows and fractional track ticks fail closed.
fn edit(
    track: &Mp4Box,
    data: &[u8],
    movie_scale: u32,
    track_scale: u32,
) -> Result<Option<Edit>, ConversionError> {
    let edts: Vec<_> = track
        .children
        .iter()
        .filter(|n| n.kind() == *b"edts")
        .collect();
    if edts.is_empty() {
        return Ok(None);
    }
    if edts.len() != 1 || edts[0].children.len() != 1 {
        return Err(UNSUPPORTED);
    }
    let payload = child(edts[0], *b"elst")?.header.payload(data);
    let width = match payload.first() {
        Some(0) => 4,
        Some(1) => 8,
        _ => return Err(UNSUPPORTED),
    };
    if payload.get(1..4) != Some(&[0, 0, 0])
        || integer(payload, 4)? != 1
        || payload.get(8 + 2 * width..) != Some(&[0, 1, 0, 0])
    {
        return Err(UNSUPPORTED);
    }
    let (duration, media_time) = if width == 4 {
        (
            u64::from(integer(payload, 8)?),
            i64::from(i32::from_be_bytes(payload[12..16].try_into().unwrap())),
        )
    } else {
        (
            u64::from_be_bytes(payload[8..16].try_into().unwrap()),
            i64::from_be_bytes(payload[16..24].try_into().unwrap()),
        )
    };
    if media_time < 0 {
        return Err(UNSUPPORTED);
    }
    media_ticks(duration, movie_scale, track_scale)?;
    Ok(Some(Edit {
        duration,
        media_time,
    }))
}
fn profile_shape(report: &Inspection) -> bool {
    matches!(report.tracks.len(), 1 | 2)
        && report
            .tracks
            .iter()
            .filter(|t| t.kind == TrackKind::Audio && t.codec.as_deref() == Some("mp4a"))
            .count()
            == 1
        && report
            .tracks
            .iter()
            .filter(|t| t.kind == TrackKind::Video && t.codec.as_deref() == Some("avc1"))
            .count()
            == report.tracks.len() - 1
}
fn samples(
    track: &Mp4Box,
    actual: &crate::inspection::Track,
    data: &[u8],
) -> Result<(), ConversionError> {
    let stbl = child(child(child(track, *b"mdia")?, *b"minf")?, *b"stbl")?;
    let stsd = child(stbl, *b"stsd")?;
    let expected = if actual.kind == TrackKind::Audio {
        *b"mp4a"
    } else {
        *b"avc1"
    };
    if stsd.children.len() != 1
        || integer(stsd.header.payload(data), 4)? != 1
        || integer(child(stbl, *b"stsz")?.header.payload(data), 8)? == 0
    {
        return Err(UNSUPPORTED);
    }
    let entry = &stsd.children[0];
    match actual.protection {
        Protection::Clear if entry.kind() == expected => Ok(()),
        Protection::Cenc | Protection::Cbcs => {
            let protected = if actual.kind == TrackKind::Audio {
                *b"enca"
            } else {
                *b"encv"
            };
            if entry.kind() != protected
                || child(child(entry, *b"sinf")?, *b"frma")?
                    .header
                    .payload(data)
                    != expected
            {
                return Err(UNSUPPORTED);
            }
            Ok(())
        }
        _ => Err(UNSUPPORTED),
    }
}
fn movie_playback(header: &Mp4Box, data: &[u8]) -> Result<(), ConversionError> {
    let payload = header.header.payload(data);
    let offset = match payload.first() {
        Some(0) => 20,
        Some(1) => 32,
        _ => return Err(UNSUPPORTED),
    };
    if integer(payload, offset)? != 1 << 16 || payload.get(offset + 4..offset + 6) != Some(&[1, 0])
    {
        return Err(UNSUPPORTED);
    }
    Ok(())
}
fn track_playback(track: &Mp4Box, audio: bool, data: &[u8]) -> Result<(), ConversionError> {
    let payload = child(track, *b"tkhd")?.header.payload(data);
    let offset = match payload.first() {
        Some(0) => 34,
        Some(1) => 46,
        _ => return Err(UNSUPPORTED),
    };
    // One enabled movie track per media role; the usual singleton audio group is harmless.
    let group = payload.get(offset..offset + 2).ok_or(UNSUPPORTED)?;
    let ordinary_group = group == [0, 0] || (audio && group == [0, 1]);
    let volume = if audio { [1, 0] } else { [0, 0] };
    if !matches!(payload.get(1..4), Some([0, 0, 3 | 7]))
        || !ordinary_group
        || payload.get(offset + 2..offset + 4) != Some(&volume)
    {
        return Err(UNSUPPORTED);
    }
    Ok(())
}
fn display_extent(track: &Mp4Box, data: &[u8]) -> Result<(u32, u32), ConversionError> {
    let payload = child(track, *b"tkhd")?.header.payload(data);
    let offset = match payload.first() {
        Some(0) => 76,
        Some(1) => 88,
        _ => return Err(UNSUPPORTED),
    };
    Ok((integer(payload, offset)?, integer(payload, offset + 4)?))
}
fn layout(movie: &audio::Movie, report: &Inspection) -> Result<Option<Layout>, ConversionError> {
    let data = &movie.bytes;
    let boxes = mp4::parse_boxes(data).map_err(|_| UNSUPPORTED)?;
    let moov = boxes
        .iter()
        .find(|n| n.kind() == *b"moov")
        .ok_or(UNSUPPORTED)?;
    if movie.fragmented || moov.children.iter().any(|n| n.kind() == *b"mvex") {
        return Ok(None);
    }
    if !profile_shape(report) {
        return Err(UNSUPPORTED);
    }
    let (movie_timescale, movie_duration) = clock(child(moov, *b"mvhd")?, data)?;
    movie_playback(child(moov, *b"mvhd")?, data)?;
    let tracks: Vec<_> = moov
        .children
        .iter()
        .filter(|n| n.kind() == *b"trak")
        .collect();
    if tracks.len() != report.tracks.len() {
        return Err(UNSUPPORTED);
    }
    let mut finite_audio = false;
    let mut video_timescale = None;
    let mut maximum_duration = 0;
    let mut windows = Vec::new();
    for (track, actual) in tracks.iter().zip(&report.tracks) {
        let mdia = child(track, *b"mdia")?;
        let handler = child(mdia, *b"hdlr")?
            .header
            .payload(data)
            .get(8..12)
            .ok_or(UNSUPPORTED)?;
        if !matches!(
            (actual.kind, handler),
            (TrackKind::Audio, b"soun") | (TrackKind::Video, b"vide")
        ) {
            return Err(UNSUPPORTED);
        }
        let (scale, _) = clock(child(mdia, *b"mdhd")?, data)?;
        let edited = edit(track, data, movie_timescale, scale)?;
        let track_duration = duration(child(track, *b"tkhd")?.header.payload(data), 20, 28)?;
        let edited_duration = edited.as_ref().map_or(track_duration, |e| e.duration);
        if actual.kind == TrackKind::Audio {
            finite_audio |= edited.as_ref().is_some_and(|e| e.duration > 0);
        } else {
            video_timescale = Some(scale);
        }
        windows.push((
            Window {
                audio: actual.kind == TrackKind::Audio,
                timescale: scale,
                media_time: edited.as_ref().map_or(0, |e| e.media_time),
                duration: media_ticks(edited_duration, movie_timescale, scale)?,
                display_extent: if actual.kind == TrackKind::Video {
                    Some(display_extent(track, data)?)
                } else {
                    None
                },
            },
            track_duration,
            edited_duration,
        ));
        maximum_duration = maximum_duration.max(track_duration);
        track_playback(track, actual.kind == TrackKind::Audio, data)?;
        samples(track, actual, data)?;
    }
    if !finite_audio {
        return Ok(None);
    }
    if movie_duration != maximum_duration
        || windows
            .iter()
            .any(|(_, track, edited)| *edited == 0 || track != edited)
    {
        return Err(UNSUPPORTED);
    }
    let mut windows: Vec<_> = windows.into_iter().map(|(window, _, _)| window).collect();
    windows.sort_by_key(|w| w.audio);
    Ok(Some(Layout {
        movie_timescale,
        movie_duration,
        video_timescale,
        windows,
    }))
}
/// Add exact edit-window evidence only for the finite progressive source profile.
/// Protection and keys are independently validated; clear and encrypted tracks expose the same windows.
pub(super) fn presentation_windows_equal(
    source: &Path,
    result: &Path,
) -> Result<bool, ConversionError> {
    let Ok(input) = crate::inspection::inspect(source) else {
        return Ok(true);
    };
    if input.format != crate::inspection::InputFormat::Mp4
        || input.tracks.iter().any(|track| {
            !matches!(
                track.protection,
                Protection::Clear | Protection::Cenc | Protection::Cbcs
            )
        })
        || !profile_shape(&input)
    {
        return Ok(true);
    }
    let source = layout(&audio::movie(source)?, &input).map_err(|_| WINDOW_UNSUPPORTED)?;
    let Some(source) = source else {
        return Ok(true);
    };
    let output = crate::inspection::inspect(result).map_err(|_| WINDOW_UNSUPPORTED)?;
    let result = layout(&audio::movie(result)?, &output)
        .map_err(|_| WINDOW_UNSUPPORTED)?
        .ok_or(WINDOW_UNSUPPORTED)?;
    Ok(source.windows == result.windows)
}
/// Return whether this is a qualified finite whole-file input; never normalize unsupported encryption.
pub(super) fn validate_input(
    source: &Path,
    input: &Inspection,
    keys: &super::InputKeys,
) -> Result<bool, ConversionError> {
    if !input.tracks.iter().any(|t| t.kind == TrackKind::Audio) {
        return Ok(false);
    }
    let movie = audio::movie(source)?;
    let protected = input
        .tracks
        .iter()
        .any(|t| t.protection != Protection::Clear);
    let finite = layout(&movie, input)
        .map_err(|error| {
            if protected {
                ConversionError("progressive-cenc-profile-not-supported")
            } else {
                error
            }
        })?
        .is_some();
    if finite && protected {
        super::cenc::validate(&movie, input, keys)?;
    }
    Ok(finite)
}
impl Converter {
    pub(super) fn progressive_mp4(
        &self,
        source: &Path,
        destination: &Path,
        input: &Inspection,
        keys: &super::InputKeys,
    ) -> Result<Option<ConversionReport>, ConversionError> {
        if !input.tracks.iter().any(|t| t.kind == TrackKind::Audio) {
            return Ok(None);
        }
        if !validate_input(source, input, keys)? {
            return Ok(None);
        }
        let source_keys =
            (input.protection != Protection::Clear).then_some(super::keys::ProbeKeys::All(keys));
        let source_layout = {
            let movie = audio::movie(source)?;
            let Some(layout) = layout(&movie, input)? else {
                return Ok(None);
            };
            layout
        };
        audio::validate_sample_groups(source)?;
        self.progressive_codecs(source, input, source_keys)?;
        let mut workload = self
            .backend
            .runner
            .registered_workload()
            .unwrap_or_default();
        workload.tracks = workload.tracks.max(input.tracks.len());
        self.backend.runner.register_workload(workload);
        let stage = super::workspace()?;
        let candidate = stage.path().join("progressive.mp4");
        self.mux_progressive(
            source,
            &candidate,
            input.tracks.len(),
            &source_layout,
            source_keys,
        )?;
        super::private_file_mode(&candidate)?;
        let output = crate::inspection::inspect(&candidate)
            .map_err(|_| ConversionError("invalid-output-media"))?;
        if output.protection != Protection::Clear
            || output.tracks.len() != input.tracks.len()
            || output
                .tracks
                .iter()
                .zip(&input.tracks)
                .any(|(a, b)| a.kind != b.kind || a.codec != b.codec)
        {
            return Err(ConversionError("output-protection-mismatch"));
        }
        super::cenc::clear(&audio::movie(&candidate)?)?;
        if layout(&audio::movie(&candidate)?, &output)? != Some(source_layout) {
            return Err(ConversionError("invalid-output-media"));
        }
        let checks = self.run_tracks(input.tracks.len(), |index| {
            let source_view = super::verify::StreamView::new(source, index, None);
            let source_view = if source_keys.is_some() {
                source_view.with_all_keys(keys)
            } else {
                source_view
            };
            self.backend.verify_views(
                source_view,
                super::verify::StreamView::new(&candidate, index, None),
            )
        })?;
        let mut combined = VerificationChecks {
            stream_metadata: super::CheckOutcome::Pass,
            packet_payloads_equal: true,
            packet_timing_equal: true,
            decoded_content_equal: true,
        };
        for check in checks {
            if !check.passed {
                return Err(ConversionError("round-trip-verification-failed"));
            }
            combined
                .stream_metadata
                .include(check.checks.stream_metadata);
            combined.packet_payloads_equal &= check.checks.packet_payloads_equal;
            combined.packet_timing_equal &= check.checks.packet_timing_equal;
            combined.decoded_content_equal &= check.checks.decoded_content_equal;
        }
        self.publish(&candidate, destination)?;
        Ok(Some(ConversionReport {
            schema_version: 1,
            input: input.clone(),
            output,
            verification: VerificationReport {
                schema_version: 1,
                passed: true,
                media_verified: true,
                checks: combined,
                license_playback_verified: false,
            },
        }))
    }
    fn progressive_codecs(
        &self,
        source: &Path,
        input: &Inspection,
        keys: Option<super::keys::ProbeKeys<'_>>,
    ) -> Result<(), ConversionError> {
        for (index, track) in input.tracks.iter().enumerate() {
            let profile = self.backend.profile_stream_keys(source, index, keys)?;
            let expected = if track.kind == TrackKind::Audio {
                "aac"
            } else {
                "h264"
            };
            if profile["codec_name"] != expected {
                return Err(UNSUPPORTED);
            }
        }
        // The AAC profile is queried separately so the established metadata comparison stays unchanged.
        let keyed = super::keys::KeyedArguments::new(
            vec![
                self.backend.ffprobe.clone(),
                "-v".into(),
                "error".into(),
                "-show_entries".into(),
                "stream=codec_type,profile".into(),
                "-of".into(),
                "json".into(),
                super::files::tool_path(source)?,
            ],
            keys,
        );
        let bytes = self.backend.run(&keyed.argv, "ffprobe", &keyed.secrets())?;
        if bytes.len() > 64 * 1024 {
            return Err(UNSUPPORTED);
        }
        let profiles: serde_json::Value =
            serde_json::from_slice(&bytes).map_err(|_| UNSUPPORTED)?;
        let streams = profiles["streams"].as_array().ok_or(UNSUPPORTED)?;
        if streams.len() != input.tracks.len()
            || streams.iter().zip(&input.tracks).any(|(stream, track)| {
                let expected = if track.kind == TrackKind::Audio {
                    "audio"
                } else {
                    "video"
                };
                stream["codec_type"] != expected
                    || (track.kind == TrackKind::Audio && stream["profile"] != "LC")
            })
        {
            return Err(UNSUPPORTED);
        }
        Ok(())
    }
    fn mux_progressive(
        &self,
        source: &Path,
        candidate: &Path,
        tracks: usize,
        source_layout: &Layout,
        keys: Option<super::keys::ProbeKeys<'_>>,
    ) -> Result<(), ConversionError> {
        let mut argv = vec![
            self.backend.ffmpeg.clone(),
            "-v".into(),
            "error".into(),
            "-nostdin".into(),
            "-xerror".into(),
            "-threads".into(),
            "1".into(),
            "-filter_threads".into(),
            "1".into(),
            "-filter_complex_threads".into(),
            "1".into(),
            "-copyts".into(),
            "-protocol_whitelist".into(),
            "file,crypto".into(),
            "-i".into(),
            super::files::tool_path(source)?,
        ];
        for index in 0..tracks {
            argv.extend(["-map".into(), format!("0:{index}")]);
        }
        argv.extend([
            "-c".into(),
            "copy".into(),
            "-avoid_negative_ts".into(),
            "disabled".into(),
            "-use_editlist".into(),
            "1".into(),
            "-movie_timescale".into(),
            source_layout.movie_timescale.to_string(),
        ]);
        if let Some(scale) = source_layout.video_timescale {
            argv.extend(["-video_track_timescale".into(), scale.to_string()]);
        }
        argv.push(super::files::tool_path(candidate)?);
        let keyed = super::keys::KeyedArguments::new(argv, keys);
        self.backend.run(&keyed.argv, "ffmpeg", &keyed.secrets())?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn box_bytes(kind: [u8; 4], payload: &[u8]) -> Vec<u8> {
        [
            &(u32::try_from(payload.len() + 8).unwrap()).to_be_bytes()[..],
            &kind[..],
            payload,
        ]
        .concat()
    }
    fn edit_bytes(version: u8, duration: u64, media_time: i64) -> Vec<u8> {
        let mut payload = vec![version, 0, 0, 0];
        payload.extend(1_u32.to_be_bytes());
        if version == 0 {
            payload.extend(u32::try_from(duration).unwrap().to_be_bytes());
            payload.extend(i32::try_from(media_time).unwrap().to_be_bytes());
        } else {
            payload.extend(duration.to_be_bytes());
            payload.extend(media_time.to_be_bytes());
        }
        payload.extend([0, 1, 0, 0]);
        box_bytes(
            *b"trak",
            &box_bytes(*b"edts", &box_bytes(*b"elst", &payload)),
        )
    }
    #[test]
    fn both_edit_versions_require_exact_integer_clocks_and_normal_nonempty_media() {
        for version in [0, 1] {
            let data = edit_bytes(version, 480_000, 1024);
            let boxes = mp4::parse_boxes(&data).unwrap();
            assert_eq!(
                edit(&boxes[0], &data, 48_000, 48_000).unwrap(),
                Some(Edit {
                    duration: 480_000,
                    media_time: 1024
                })
            );
            assert_eq!(
                edit(&boxes[0], &data, 48_001, 48_000).unwrap_err(),
                UNSUPPORTED
            );
            let data = edit_bytes(version, 480_000, -1);
            let boxes = mp4::parse_boxes(&data).unwrap();
            assert_eq!(
                edit(&boxes[0], &data, 48_000, 48_000).unwrap_err(),
                UNSUPPORTED
            );
        }
    }
    #[test]
    fn truncated_edit_payloads_cannot_panic_or_authorize_a_presentation() {
        for version in [0, 1] {
            let full = edit_bytes(version, 480_000, 1024);
            let parsed = mp4::parse_boxes(&full).unwrap();
            let payload = parsed[0].children[0].children[0].header.payload(&full);
            for length in 0..payload.len() {
                let data = box_bytes(
                    *b"trak",
                    &box_bytes(*b"edts", &box_bytes(*b"elst", &payload[..length])),
                );
                let boxes = mp4::parse_boxes(&data).unwrap();
                assert_eq!(
                    edit(&boxes[0], &data, 48_000, 48_000).unwrap_err(),
                    UNSUPPORTED
                );
            }
        }
    }
    #[test]
    fn display_extents_are_exact_and_require_both_complete_fields() {
        for (version, offset) in [(0, 76), (1, 88)] {
            let mut payload = vec![0; offset + 8];
            payload[0] = version;
            payload[offset..offset + 4].copy_from_slice(&(320_u32 << 16).to_be_bytes());
            payload[offset + 4..offset + 8].copy_from_slice(&(180_u32 << 16).to_be_bytes());
            let bytes = box_bytes(*b"trak", &box_bytes(*b"tkhd", &payload));
            let boxes = mp4::parse_boxes(&bytes).unwrap();
            assert_eq!(
                display_extent(&boxes[0], &bytes).unwrap(),
                (320 << 16, 180 << 16)
            );
            for length in 0..payload.len() {
                let bytes = box_bytes(*b"trak", &box_bytes(*b"tkhd", &payload[..length]));
                let boxes = mp4::parse_boxes(&bytes).unwrap();
                assert_eq!(display_extent(&boxes[0], &bytes).unwrap_err(), UNSUPPORTED);
            }
        }
    }
}
