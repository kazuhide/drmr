//! Private snapshots are qualified before independent whole-track normalization.
use super::{Prepared, TrackClock, TrackConversionRequest, key_for, prepare};
use crate::{
    conversion::{
        ConversionError, Converter, InputKeys, LocalTrack, audio, copy_private, files, finite,
        keys::ProbeKeys, measurement::Phase, media_progress, progressive, signaling,
        verify::StreamView,
    },
    inspection::{self, Inspection, Protection, TrackKind},
    resources::{Claim, ReservationError},
    toolrun::{DEFAULT_STDERR_BYTES, DEFAULT_STDOUT_BYTES},
};
use std::path::{Path, PathBuf};

impl Converter {
    pub(super) fn normalize_tracks(
        &self,
        request: &TrackConversionRequest<'_>,
        work: &Path,
        clocks: Option<&[TrackClock]>,
    ) -> Result<(Vec<Prepared>, Vec<PathBuf>), ConversionError> {
        if clocks.is_some_and(|clocks| clocks.len() != request.tracks.len()) {
            return Err(ConversionError("native-clock-profile-mismatch"));
        }
        let snapshots = media_progress::Batch::units(
            self.backend.activity.clone(),
            media_progress::Operation::Snapshot,
            media_progress::Subject::Source,
            media_progress::Purpose::Conversion,
            media_progress::Unit::Tracks,
            request.tracks.len(),
        );
        // Unknown profiles use conservative admission until every private snapshot is inspected.
        let sources = self.run_tracks(request.tracks.len(), |ordinal| {
            let task = snapshots
                .as_ref()
                .and_then(|progress| progress.start(ordinal));
            let prepared = self.normalization_source(
                &request.tracks[ordinal],
                &work.join(format!("normalize-{ordinal:04}")),
                clocks.map(|clocks| &clocks[ordinal]),
                request.input_keys,
            )?;
            self.backend.check_cancelled()?;
            if let Some(task) = task {
                task.finish();
            }
            Ok(prepared)
        })?;
        if let Some(snapshots) = &snapshots {
            snapshots.finish();
        }
        let normalization = media_progress::Batch::units(
            self.backend.activity.clone(),
            media_progress::Operation::Normalization,
            media_progress::Subject::Source,
            media_progress::Purpose::Conversion,
            media_progress::Unit::Tracks,
            sources.len(),
        );
        let clears = self.run_tracks(sources.len(), |ordinal| {
            let task = normalization
                .as_ref()
                .and_then(|progress| progress.start(ordinal));
            let clear = self.normalize_track(&sources[ordinal], request.input_keys)?;
            self.backend.check_cancelled()?;
            if let Some(task) = task {
                task.finish();
            }
            Ok(clear)
        })?;
        if let Some(normalization) = &normalization {
            normalization.finish();
        }
        Ok((sources, clears))
    }

    fn normalization_source(
        &self,
        track: &LocalTrack,
        directory: &Path,
        clock: Option<&TrackClock>,
        keys: &InputKeys,
    ) -> Result<Prepared, ConversionError> {
        let (prepared, progressive) =
            self.inspect_normalization_snapshot(track, directory, clock, keys)?;
        // Profiles and optional codec probes acquire their own shared CPU/memory permits.
        if let Some(report) = &progressive {
            self.progressive_codecs(
                &prepared.source,
                report,
                (report.protection != Protection::Clear).then_some(ProbeKeys::All(keys)),
            )?;
        }
        let profile = self.backend.profile_stream_keys(
            &prepared.source,
            track.index,
            if prepared.source_dictionary {
                Some(ProbeKeys::All(keys))
            } else {
                key_for(&prepared, keys).map(ProbeKeys::One)
            },
        )?;
        self.validate_signaling_codec(&profile, track.kind)?;
        if profile["codec_type"].as_str()
            != Some(if track.kind == TrackKind::Audio {
                "audio"
            } else {
                "video"
            })
        {
            return Err(ConversionError("track-role-mismatch"));
        }
        Ok(prepared)
    }

    fn inspect_normalization_snapshot(
        &self,
        track: &LocalTrack,
        directory: &Path,
        clock: Option<&TrackClock>,
        keys: &InputKeys,
    ) -> Result<(Prepared, Option<Inspection>), ConversionError> {
        let claim = Claim::for_workload(
            self.backend
                .runner
                .registered_workload()
                .unwrap_or_default(),
            DEFAULT_STDOUT_BYTES,
            DEFAULT_STDERR_BYTES,
        );
        let _permit = self
            .backend
            .runner
            .native_permit(claim, &self.backend.stop, self.backend.timeout)
            .map_err(|error| match error {
                ReservationError::Cancelled => ConversionError("cancelled"),
                ReservationError::Insufficient => ConversionError("resource-budget"),
                ReservationError::Timeout => ConversionError("media-tool-timeout"),
            })?;
        // A completed tool seals only its own directory while sibling tools may write.
        files::create_dir(directory)?;
        let source = directory.join("source.mp4");
        copy_private(&track.source, &source)?;
        self.backend.check_cancelled()?;
        if self
            .output_signaling
            .as_ref()
            .is_some_and(|value| !value.is_multi())
        {
            signaling::clear_mp4(&source, &self.backend.stop)?;
        }
        let mut prepared = prepare(&source, track.index, track.kind, keys)?;
        if let Some(clock) = clock {
            clock.bind(track, &mut prepared)?;
        }
        let progressive = if finite::has_audio(&source)? && !audio::movie(&source)?.fragmented {
            let report =
                inspection::inspect(&source).map_err(|_| ConversionError("invalid-input-media"))?;
            if !progressive::validate_input(&source, &report, keys)? {
                return Err(ConversionError("progressive-mp4-profile-not-supported"));
            }
            Some(report)
        } else {
            None
        };
        Ok((prepared, progressive))
    }

    fn normalize_track(
        &self,
        source: &Prepared,
        keys: &InputKeys,
    ) -> Result<PathBuf, ConversionError> {
        let directory = source
            .media
            .parent()
            .ok_or(ConversionError("temporary-workspace-unavailable"))?;
        let clear = directory.join("clear.mp4");
        let normalization_scope = self.backend.scope(Phase::Normalization);
        let whole_clear = directory.join("whole-clear.mp4");
        let packaging_source = if source.source_dictionary {
            let report = inspection::inspect(&source.source)
                .map_err(|_| ConversionError("invalid-input-media"))?;
            self.clear_finite(&source.source, &whole_clear, &report, keys)?;
            &whole_clear
        } else {
            &source.media
        };
        self.backend.package_stream(
            packaging_source,
            source.index,
            &clear,
            (source.key_id.is_some() && !source.source_dictionary).then_some(keys),
            None,
        )?;
        drop(normalization_scope);
        let original = StreamView::new(&source.source, source.index, key_for(source, keys))
            .with_clock(source.clock);
        let normalization = self.backend.verify_views(
            original.with_all_keys(keys),
            StreamView::new(&clear, 0, None).with_clock(source.clock),
        )?;
        if !normalization.passed {
            return Err(ConversionError("source-normalization-verification-failed"));
        }
        Ok(clear)
    }
}
