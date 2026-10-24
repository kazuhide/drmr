//! Original-resource MP4 evidence with a separate, identity-bound initialization file.
use super::{
    BAD, CHUNK_BYTES, ConversionError, InputKeys, MAX_METADATA_BYTES, Movie, SampleInfo,
    TrackDescriptor, UNSUPPORTED, catalogue, fragment, init, read,
};
use crate::{concurrency::Stop, native::FileIdentity};
use sha2::{Digest as _, Sha256};
use std::path::{Path, PathBuf};

/// Sample membership and exact clocks for one track in one original media resource.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::conversion) struct SegmentTrackEvidence {
    pub index: usize,
    pub track_id: u32,
    pub timescale: u32,
    pub sample_count: u64,
    pub first_dts: i64,
    pub end_dts: i64,
    pub first_pts: i64,
    pub end_pts: i64,
    pub payload: [u8; 32],
    pub samples: [u8; 32],
    pub grouping: [u8; 32],
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::conversion) struct SegmentEvidence {
    pub fragment_count: u64,
    pub tracks: Vec<SegmentTrackEvidence>,
}

/// Immutable initialization metadata can be shared by bounded resource workers.
pub(in crate::conversion) struct SegmentInput {
    pub(super) layout: read::Layout,
    pub(super) movie: Movie,
    pub(super) original: Vec<u8>,
    source: PathBuf,
    identity: FileIdentity,
    fingerprint: [u8; 32],
}
impl SegmentInput {
    pub fn new(source: &Path, stop: &Stop) -> Result<Self, ConversionError> {
        let mut reader = read::Reader::open(source, stop, true)?;
        if reader.length > MAX_METADATA_BYTES as u64 {
            return Err(ConversionError("resource-budget"));
        }
        let layout = reader.layout()?;
        if !layout.moofs.is_empty() || layout.mdats.iter().any(|range| !range.is_empty()) {
            return Err(UNSUPPORTED);
        }
        let mut initialization = init::parse(&layout)?;
        if !initialization.movie.fragmented {
            return Err(UNSUPPORTED);
        }
        catalogue(&mut reader, &layout, &mut initialization)?;
        let identity = FileIdentity::read(&reader.file).map_err(|_| BAD)?;
        let fingerprint = reader.expected_fingerprint().ok_or(BAD)?;
        let length = reader.length;
        let original = reader.read(0..length, MAX_METADATA_BYTES)?;
        reader.finish()?;
        Ok(Self {
            layout,
            movie: initialization.movie,
            original,
            source: source.into(),
            identity,
            fingerprint,
        })
    }
    pub fn movie(&self) -> &Movie {
        &self.movie
    }
    pub(super) fn check_initialization(&self, stop: &Stop) -> Result<(), ConversionError> {
        let reader = read::Reader::open(&self.source, stop, true)?;
        if FileIdentity::read(&reader.file).map_err(|_| BAD)? != self.identity
            || reader.expected_fingerprint() != Some(self.fingerprint)
        {
            return Err(ConversionError("native-source-changed"));
        }
        reader.finish()
    }
    pub(super) fn prepare<'a>(
        &self,
        resource: &Path,
        keys: &InputKeys,
        stop: &'a Stop,
    ) -> Result<(read::Reader<'a>, read::Layout, init::Initialization), ConversionError> {
        self.check_initialization(stop)?;
        let mut reader = read::Reader::open(resource, stop, true)?;
        let layout = reader.resource_layout(&self.layout)?;
        if layout.moofs.is_empty() {
            return Err(BAD);
        }
        let mut initialization = init::parse(&layout)?;
        catalogue(&mut reader, &layout, &mut initialization)?;
        if initialization.movie.tracks.iter().all(|track| track.sample_count == 0) {
            return Err(BAD);
        }
        for track in &initialization.tracks {
            if let Some(protection) = &track.encryption
                && !keys.keys.iter().any(|key| key.id == protection.kid)
            {
                return Err(ConversionError("input-key-required"));
            }
        }
        Ok((reader, layout, initialization))
    }
    /// Callback data is provisional until all resource and initialization checks finish.
    pub fn scan(
        &self,
        resource: &Path,
        keys: &InputKeys,
        stop: &Stop,
        visitor: &mut impl FnMut(&TrackDescriptor, &SampleInfo, &[u8]) -> Result<(), ConversionError>,
    ) -> Result<Movie, ConversionError> {
        let (mut reader, layout, initialization) = self.prepare(resource, keys, stop)?;
        for track in &initialization.tracks {
            super::visit_fragments(&mut reader, &layout, &initialization, track, keys, visitor)?;
        }
        reader.finish()?;
        self.check_initialization(stop)?;
        Ok(initialization.movie)
    }
    pub fn evidence(
        &self,
        resource: &Path,
        keys: &InputKeys,
        stop: &Stop,
    ) -> Result<SegmentEvidence, ConversionError> {
        let (mut reader, layout, initialization) = self.prepare(resource, keys, stop)?;
        let mut hashes = initialization
            .tracks
            .iter()
            .map(|track| TrackHash::new(&track.descriptor))
            .collect::<Vec<_>>();
        for range in &layout.moofs {
            let fragment = fragment::parse(&mut reader, range, &layout, &initialization)?;
            for part in &fragment.tracks {
                let hash = hashes.get_mut(part.index).ok_or(BAD)?;
                hash.grouping.update(part.count.to_be_bytes());
                hash.grouping.update((part.runs.len() as u64).to_be_bytes());
                for run in &part.runs {
                    hash.grouping.update(run.count.to_be_bytes());
                }
            }
        }
        for track in &initialization.tracks {
            super::visit_fragments(
                &mut reader,
                &layout,
                &initialization,
                track,
                keys,
                &mut |descriptor, info, bytes| {
                    hashes.get_mut(descriptor.index).ok_or(BAD)?.record(info, bytes)
                },
            )?;
        }
        reader.finish()?;
        self.check_initialization(stop)?;
        Ok(SegmentEvidence {
            fragment_count: u64::try_from(layout.moofs.len()).map_err(|_| BAD)?,
            tracks: hashes
                .into_iter()
                .filter(|hash| hash.count != 0)
                .map(TrackHash::finish)
                .collect::<Result<_, _>>()?,
        })
    }
}

pub(super) struct TrackHash {
    index: usize,
    track_id: u32,
    timescale: u32,
    count: u64,
    first_dts: Option<i64>,
    end_dts: i64,
    first_pts: i64,
    end_pts: i64,
    received: u32,
    payload: Sha256,
    samples: Sha256,
    pub(super) grouping: Sha256,
}
impl TrackHash {
    pub(super) fn new(track: &TrackDescriptor) -> Self {
        Self {
            index: track.index,
            track_id: track.track_id,
            timescale: track.timescale,
            count: 0,
            first_dts: None,
            end_dts: 0,
            first_pts: i64::MAX,
            end_pts: i64::MIN,
            received: 0,
            payload: Sha256::new(),
            samples: Sha256::new(),
            grouping: Sha256::new(),
        }
    }
    pub(super) fn record(&mut self, info: &SampleInfo, bytes: &[u8]) -> Result<(), ConversionError> {
        if bytes.is_empty() || bytes.len() > CHUNK_BYTES || info.chunk_offset != self.received {
            return Err(BAD);
        }
        if info.chunk_offset == 0 {
            if self.first_dts.is_some() && self.end_dts != info.dts {
                return Err(ConversionError("fragment-timeline-gap-or-overlap"));
            }
            self.first_dts.get_or_insert(info.dts);
            self.end_dts = info.dts.checked_add(i64::from(info.duration)).ok_or(BAD)?;
            self.first_pts = self.first_pts.min(info.pts);
            self.end_pts = self.end_pts.max(info.pts.checked_add(i64::from(info.duration)).ok_or(BAD)?);
            self.payload.update(info.size.to_be_bytes());
            self.samples.update(info.dts.to_be_bytes());
            self.samples.update(info.pts.to_be_bytes());
            self.samples.update(info.duration.to_be_bytes());
            self.samples.update(info.size.to_be_bytes());
            self.samples.update(info.flags.to_be_bytes());
            for distance in [info.roll_distance, info.preroll_distance] {
                self.samples.update([u8::from(distance.is_some())]);
                self.samples.update(distance.unwrap_or(0).to_be_bytes());
            }
        }
        self.payload.update(bytes);
        self.received = self.received.checked_add(bytes.len() as u32).ok_or(BAD)?;
        if self.received > info.size || info.is_last_chunk != (self.received == info.size) {
            return Err(BAD);
        }
        if info.is_last_chunk {
            self.count = self.count.checked_add(1).ok_or(BAD)?;
            self.received = 0;
        }
        Ok(())
    }
    pub(super) fn finish(self) -> Result<SegmentTrackEvidence, ConversionError> {
        if self.received != 0 {
            return Err(BAD);
        }
        Ok(SegmentTrackEvidence {
            index: self.index,
            track_id: self.track_id,
            timescale: self.timescale,
            sample_count: self.count,
            first_dts: self.first_dts.ok_or(BAD)?,
            end_dts: self.end_dts,
            first_pts: self.first_pts,
            end_pts: self.end_pts,
            payload: self.payload.finalize().into(),
            samples: self.samples.finalize().into(),
            grouping: self.grouping.finalize().into(),
        })
    }
}
