//! A declared terminal AAC range is separate from complete coded media and raw clocks.
use super::{ConversionError, backend::Backend};
use crate::{concurrency::Stop, toolrun::RunOptions};
use std::path::Path;
mod edit;
const BAD: ConversionError = ConversionError("audio-presentation-range-not-supported");
pub(super) const MISMATCH: ConversionError = ConversionError("audio-presentation-range-mismatch");
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct AudioEvidence {
    pub sample_rate: u32,
    pub coded_frames: u64,
    pub raw_first_pts: i64,
    pub raw_coded_end: i64,
}
impl AudioEvidence {
    pub(super) fn coded_samples(self) -> Result<u64, ConversionError> {
        self.coded_frames.checked_mul(1024).ok_or(BAD)
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct AudioClaim {
    pub sample_rate: u32,
    pub raw_first_pts: i64,
    pub presented_samples: u64,
}
impl AudioClaim {
    pub(super) fn bind(
        self,
        evidence: AudioEvidence,
    ) -> Result<AudioPresentation, ConversionError> {
        if evidence.sample_rate != self.sample_rate || evidence.raw_first_pts != self.raw_first_pts
        {
            return Err(MISMATCH);
        }
        AudioPresentation::new(evidence, self.presented_samples)
    }
    pub(super) fn movie_scale(self) -> Result<u32, ConversionError> {
        match self.sample_rate {
            44_100 => Ok(4_410_000),
            48_000 => Ok(720_000),
            _ => Err(BAD),
        }
    }
    pub(super) fn end(self) -> Result<(u64, u32), ConversionError> {
        let scale = self.movie_scale()?;
        let first = u128::try_from(self.raw_first_pts)
            .map_err(|_| BAD)?
            .checked_mul(u128::from(scale))
            .ok_or(BAD)?
            / 90_000;
        let duration = u128::from(self.presented_samples)
            .checked_mul(u128::from(scale))
            .ok_or(BAD)?
            / u128::from(self.sample_rate);
        Ok((
            u64::try_from(first.checked_add(duration).ok_or(BAD)?).map_err(|_| BAD)?,
            scale,
        ))
    }
    pub(super) fn interval(self, first: u64, end: u64) -> Result<(u64, u64, u64), ConversionError> {
        let (limit, scale) = self.end()?;
        let factor = u64::from(scale) / 90_000;
        let first = first.checked_mul(factor).ok_or(MISMATCH)?;
        let end = end.checked_mul(factor).ok_or(MISMATCH)?.min(limit);
        if end <= first {
            return Err(MISMATCH);
        }
        Ok((first, end, u64::from(scale)))
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct AudioPresentation {
    pub evidence: AudioEvidence,
    pub presented_samples: u64,
}
impl AudioPresentation {
    pub(super) fn new(
        evidence: AudioEvidence,
        presented_samples: u64,
    ) -> Result<Self, ConversionError> {
        let full = evidence.coded_samples()?;
        if !matches!(evidence.sample_rate, 44_100 | 48_000)
            || evidence.raw_first_pts < 0
            || evidence.raw_coded_end <= evidence.raw_first_pts
            || presented_samples == 0
            || presented_samples >= full
            || full
                .checked_sub(presented_samples)
                .is_none_or(|n| n >= 1024)
        {
            return Err(BAD);
        }
        let presentation = Self {
            evidence,
            presented_samples,
        };
        let (end, scale) = presentation.movie_end()?;
        let coded_end = u128::try_from(evidence.raw_coded_end)
            .map_err(|_| BAD)?
            .checked_mul(u128::from(scale))
            .ok_or(BAD)?
            / 90_000;
        if u128::from(end) > coded_end {
            return Err(BAD);
        }
        Ok(presentation)
    }
    pub(super) fn movie_scale(self) -> Result<u32, ConversionError> {
        self.claim().movie_scale()
    }
    pub(super) fn movie_end(self) -> Result<(u64, u32), ConversionError> {
        self.claim().end()
    }
    fn claim(self) -> AudioClaim {
        AudioClaim {
            sample_rate: self.evidence.sample_rate,
            raw_first_pts: self.evidence.raw_first_pts,
            presented_samples: self.presented_samples,
        }
    }
    pub(super) fn from_final(
        evidence: AudioEvidence,
        last_frames: u64,
        declared: &str,
    ) -> Result<Self, ConversionError> {
        let bad = ConversionError("ts-segment-duration-mismatch");
        if last_frames == 0 || last_frames > evidence.coded_frames {
            return Err(bad);
        }
        let base = last_frames
            .checked_sub(1)
            .and_then(|n| n.checked_mul(1024))
            .ok_or(bad)?;
        let mut found = None;
        for last in 1_u64..1024 {
            let segment_samples = base.checked_add(last).ok_or(bad)?;
            if check_sample_duration(declared, segment_samples, evidence.sample_rate).is_ok() {
                if found.is_some() {
                    return Err(bad);
                }
                let presented = evidence
                    .coded_samples()?
                    .checked_sub(1024 - last)
                    .ok_or(bad)?;
                found = Some(Self::new(evidence, presented).map_err(|_| bad)?);
            }
        }
        found.ok_or(bad)
    }
}
pub(super) fn read_claim(path: &Path) -> Result<Option<AudioClaim>, ConversionError> {
    edit::read_claim(path)
}
pub(super) fn read(
    path: &Path,
    evidence: AudioEvidence,
) -> Result<Option<AudioPresentation>, ConversionError> {
    edit::read(path, evidence)
}
pub(super) fn restore(
    path: &Path,
    presentation: AudioPresentation,
    stop: &Stop,
) -> Result<(), ConversionError> {
    edit::restore(path, presentation, stop)
}
pub(super) fn strip(
    path: &Path,
    presentation: AudioPresentation,
    stop: &Stop,
) -> Result<(), ConversionError> {
    edit::strip(path, presentation, stop)
}
/// The unique output spelling is derived from integer presentation bounds.
pub(super) fn seconds(numerator: u128, denominator: u128) -> Result<String, ConversionError> {
    if numerator == 0 || denominator == 0 {
        return Err(MISMATCH);
    }
    let n = numerator
        .checked_mul(1_000_000_000)
        .ok_or(MISMATCH)?
        .div_ceil(denominator);
    let mut text = format!("{}.{:09}", n / 1_000_000_000, n % 1_000_000_000);
    while text.ends_with('0') {
        text.pop();
    }
    if text.ends_with('.') {
        text.pop();
    }
    Ok(text)
}
/// Exact canonical output declarations do not inherit the input's optional binary64 spelling.
pub(super) fn declaration_matches(
    text: &str,
    numerator: u128,
    denominator: u128,
) -> Result<(), ConversionError> {
    let expected = seconds(numerator, denominator)?;
    let decimal = |value: &str| -> Option<(u128, u128)> {
        let (whole, part) = value.split_once('.').unwrap_or((value, ""));
        if part.len() > 18
            || whole.is_empty()
            || !whole
                .bytes()
                .chain(part.bytes())
                .all(|b| b.is_ascii_digit())
        {
            return None;
        }
        let scale = 10_u128.checked_pow(part.len().try_into().ok()?)?;
        let fraction = if part.is_empty() {
            0
        } else {
            part.parse().ok()?
        };
        Some((
            whole
                .parse::<u128>()
                .ok()?
                .checked_mul(scale)?
                .checked_add(fraction)?,
            scale,
        ))
    };
    let (a, b) = decimal(text).ok_or(MISMATCH)?;
    let (c, d) = decimal(&expected).ok_or(MISMATCH)?;
    if a.checked_mul(d).ok_or(MISMATCH)? != c.checked_mul(b).ok_or(MISMATCH)? {
        return Err(MISMATCH);
    }
    Ok(())
}
/// Source decimals retain their written precision; generated decimals round upward to nanoseconds.
pub(super) fn check_sample_duration(
    text: &str,
    samples: u64,
    rate: u32,
) -> Result<(), ConversionError> {
    if super::ts::check_duration_ratio(text, u128::from(samples), rate).is_ok()
        || (text
            .split_once('.')
            .is_some_and(|(_, part)| part.len() == 9)
            && declaration_matches(text, u128::from(samples), u128::from(rate)).is_ok())
    {
        return Ok(());
    }
    Err(ConversionError("ts-segment-duration-mismatch"))
}
impl Backend {
    /// Compare full native PCM and independently selected decoded ordinal ranges.
    pub(super) fn verify_audio_presentation(
        &self,
        a: super::ts::View<'_>,
        b: super::ts::View<'_>,
        first: AudioPresentation,
        second: AudioPresentation,
    ) -> Result<bool, ConversionError> {
        if first != second {
            return Ok(false);
        }
        Ok(self.audio_presentation_pcm(a, first)? == self.audio_presentation_pcm(b, second)?)
    }
    fn audio_presentation_pcm(
        &self,
        view: super::ts::View<'_>,
        range: AudioPresentation,
    ) -> Result<super::finite::Decoded, ConversionError> {
        let profile = self.profile_stream_keys_options(
            view.input,
            view.index,
            view.probe_keys(),
            view.unedited,
        )?;
        let channels = profile["channels"]
            .as_u64()
            .and_then(|n| u32::try_from(n).ok())
            .ok_or(BAD)?;
        let mut argv = vec![
            self.ffmpeg.clone(),
            "-v".into(),
            "error".into(),
            "-nostdin".into(),
            "-xerror".into(),
            "-threads".into(),
            "1".into(),
            "-filter_threads".into(),
            "1".into(),
            "-flags2".into(),
            "+skip_manual".into(),
        ];
        if view.unedited {
            argv.extend(["-ignore_editlist".into(), "1".into()]);
        }
        argv.extend([
            "-protocol_whitelist".into(),
            "file,crypto".into(),
            "-i".into(),
            super::files::tool_path(view.input)?,
            "-map".into(),
            format!("0:{}", view.index),
            "-threads".into(),
            "1".into(),
            "-c:a".into(),
            "pcm_f32le".into(),
            "-f".into(),
            "f32le".into(),
            "-".into(),
        ]);
        let keyed = super::keys::KeyedArguments::new(argv, view.probe_keys());
        let secrets = keyed.secrets();
        let shown = ["ffmpeg".into(), "[arguments redacted]".into()];
        let options = RunOptions {
            stop: Some(&self.stop),
            secrets: &secrets,
            argv_redacted: Some(&shown),
            timeout: Some(self.timeout),
            max_output_chars: 0,
            max_stdout_bytes: 16 * 1024,
            log_stdout: false,
            ..Default::default()
        };
        let mut pcm = super::finite::Pcm::new(
            super::finite::Window {
                start: 0,
                duration: range.presented_samples,
                scale: range.evidence.sample_rate,
                audio: true,
            },
            range.evidence.sample_rate,
            channels,
        )?;
        let effective = Self::effective_argv(&keyed.argv, "ffmpeg");
        let result = self
            .runner
            .run_tool_streamed(&effective, "ffmpeg", &options, |bytes| pcm.push(bytes))
            .map_err(|e| super::backend::tool_error(&e))?;
        self.check_cancelled()?;
        if result.returncode != Some(0) {
            return Err(ConversionError("media-tool-failed"));
        }
        let decoded = pcm.finish()?;
        if decoded.raw_samples != range.evidence.coded_samples()? {
            return Err(MISMATCH);
        }
        Ok(decoded)
    }
}
fn write_manifest(path: &Path, text: &str) -> Result<(), ConversionError> {
    use std::io::Write as _;
    super::files::seal(path.parent().ok_or(MISMATCH)?)?;
    let mut file = crate::native::create_media_or_truncate(path).map_err(|_| MISMATCH)?;
    file.write_all(text.as_bytes()).map_err(|_| MISMATCH)?;
    file.sync_all().map_err(|_| MISMATCH)
}
pub(super) fn finish_hls(root: &Path, tracks: usize, stop: &Stop) -> Result<(), ConversionError> {
    for ordinal in 0..tracks {
        let directory = root.join(format!("track-{ordinal:04}"));
        let init = directory.join("init.mp4");
        if read_claim(&init)?.is_none() {
            continue;
        }
        let path = directory.join("media.m3u8");
        let text = String::from_utf8(super::files::read_manifest(&path)?).map_err(|_| MISMATCH)?;
        let mut clock = super::timing::FragmentClock::read(&init)?;
        let mut lines: Vec<_> = text.lines().map(str::to_owned).collect();
        let mut pending = None;
        for index in 0..lines.len() {
            if stop.is_set() {
                return Err(ConversionError("cancelled"));
            }
            if lines[index].starts_with("#EXTINF:") {
                if pending.replace(index).is_some() {
                    return Err(MISMATCH);
                }
            } else if !lines[index].is_empty() && !lines[index].starts_with('#') {
                let at = pending.take().ok_or(MISMATCH)?;
                let resource = &lines[index];
                if resource.contains(['/', '\\', ':']) {
                    return Err(MISMATCH);
                }
                let (first, end, scale) = clock.segment_presentation(&directory.join(resource))?;
                lines[at] = format!(
                    "#EXTINF:{},",
                    seconds(u128::from(end - first), u128::from(scale))?
                );
            }
        }
        if pending.is_some() {
            return Err(MISMATCH);
        }
        clock.presentation_window()?;
        write_manifest(&path, &(lines.join("\n") + "\n"))?;
    }
    Ok(())
}
pub(super) fn finish_transport_hls(
    root: &Path,
    ordinal: usize,
    range: AudioPresentation,
    keys: &super::HlsInputKeys,
    backend: &Backend,
) -> Result<(), ConversionError> {
    let owner = super::files::Stage::new(None)?;
    let directory = root.join(format!("track-{ordinal:04}"));
    let path = directory.join("media.m3u8");
    let localized = super::hls::localize(&path, owner.path(), keys, backend)?;
    let index = 0;
    let evidence = backend.audio_evidence(super::ts::View {
        input: &localized.combined,
        metadata: &localized.combined,
        index,
        key: None,
        all_keys: None,
        transport: true,
        native: None,
        unedited: false,
    })?;
    if evidence != range.evidence {
        return Err(MISMATCH);
    }
    let tail = evidence
        .coded_samples()?
        .checked_sub(range.presented_samples)
        .ok_or(MISMATCH)?;
    let mut declarations = Vec::new();
    for (ordinal, (segment, _)) in localized.transport_segments.iter().enumerate() {
        let observed = backend.audio_evidence(super::ts::View {
            input: segment,
            metadata: segment,
            index,
            key: None,
            all_keys: None,
            transport: true,
            native: None,
            unedited: false,
        })?;
        if observed.sample_rate != evidence.sample_rate {
            return Err(MISMATCH);
        }
        let samples = observed
            .coded_samples()?
            .checked_sub(if ordinal + 1 == localized.transport_segments.len() {
                tail
            } else {
                0
            })
            .ok_or(MISMATCH)?;
        let n = u128::from(samples)
            .checked_mul(1_000_000_000)
            .ok_or(MISMATCH)?
            .div_ceil(u128::from(evidence.sample_rate));
        declarations.push(format!("{}.{:09}", n / 1_000_000_000, n % 1_000_000_000));
    }
    let text = String::from_utf8(super::files::read_manifest(&path)?).map_err(|_| MISMATCH)?;
    let mut lines: Vec<_> = text.lines().map(str::to_owned).collect();
    let mut declarations = declarations.into_iter();
    for line in &mut lines {
        if line.starts_with("#EXTINF:") {
            *line = format!("#EXTINF:{},", declarations.next().ok_or(MISMATCH)?);
        }
    }
    if declarations.next().is_some() {
        return Err(MISMATCH);
    }
    write_manifest(&path, &(lines.join("\n") + "\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn evidence(rate: u32) -> AudioEvidence {
        let frames = 432_u64;
        let first = 126_000;
        AudioEvidence {
            sample_rate: rate,
            coded_frames: frames,
            raw_first_pts: first,
            raw_coded_end: first + i64::try_from(frames * 1024 * 90_000 / u64::from(rate)).unwrap(),
        }
    }

    #[test]
    fn a_final_declaration_selects_one_sample_count_without_moving_raw_origin() {
        for rate in [44_100, 48_000] {
            let raw = evidence(rate);
            let declared = format!("{:.20}", 240_f64 / f64::from(rate));
            let range = AudioPresentation::from_final(raw, 1, &declared).unwrap();
            assert_eq!(range.evidence, raw);
            assert_eq!(range.presented_samples, 431 * 1024 + 240);
            let (end, scale) = range.movie_end().unwrap();
            assert_eq!(
                end,
                u64::try_from(raw.raw_first_pts).unwrap() * u64::from(scale) / 90_000
                    + range.presented_samples * u64::from(scale) / u64::from(rate)
            );
        }
    }

    #[test]
    fn final_frame_candidates_must_be_unique_and_cannot_remove_a_whole_frame() {
        let raw = evidence(44_100);
        for invalid in ["0.01", "0", "0.023219954648526078"] {
            assert_eq!(
                AudioPresentation::from_final(raw, 1, invalid)
                    .unwrap_err()
                    .0,
                "ts-segment-duration-mismatch"
            );
        }
        assert_eq!(
            AudioPresentation::from_final(raw, 0, "0.005442176870748299")
                .unwrap_err()
                .0,
            "ts-segment-duration-mismatch"
        );
    }

    #[test]
    fn a_sample_endpoint_cannot_extend_its_own_quantized_coded_end() {
        let mut raw = evidence(44_100);
        let count = raw.coded_samples().unwrap();
        let first = raw.raw_first_pts;
        // Deliberately put the observed final end before the ordinal endpoint.
        raw.raw_coded_end = first + i64::try_from((count - 1) * 90_000 / 44_100).unwrap();
        assert_eq!(
            AudioPresentation::new(raw, count - 1).unwrap_err().0,
            "audio-presentation-range-not-supported"
        );
    }

    #[test]
    fn canonical_declarations_compare_checked_exact_decimal_values() {
        let text = seconds(240, 44_100).unwrap();
        assert_eq!(text, "0.005442177");
        declaration_matches("0.0054421770", 240, 44_100).unwrap();
        for invalid in ["0.005442176", "0.005442178", "1e-3", "-0.005442177"] {
            assert!(declaration_matches(invalid, 240, 44_100).is_err());
        }
    }

    #[test]
    fn generated_ts_declarations_allow_only_exact_nanosecond_rounding() {
        // The previous nearest spelling and the producer's upward spelling have
        // distinct, explicitly supported integer serialization rules.
        check_sample_duration("0.021333333", 1024, 48_000).unwrap();
        check_sample_duration("0.021333334", 1024, 48_000).unwrap();
        for invalid in ["0.021333332", "0.021333335"] {
            assert_eq!(
                check_sample_duration(invalid, 1024, 48_000).unwrap_err().0,
                "ts-segment-duration-mismatch"
            );
        }
    }
}
