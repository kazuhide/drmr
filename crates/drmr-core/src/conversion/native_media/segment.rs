//! Original-resource MP4 evidence with a separate, identity-bound initialization file.
use super::{
    BAD, CHUNK_BYTES, ConversionError, InputKeys, MAX_METADATA_BYTES, Movie, SampleInfo,
    TrackDescriptor, UNSUPPORTED, catalogue, fragment, init, read,
};
use crate::{concurrency::Stop, native::FileIdentity};
use sha2::{Digest as _, Sha256};
use std::path::{Path, PathBuf};
use zeroize::Zeroizing;

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
#[cfg(test)]
type ProtectionLayout = Vec<Vec<Vec<(u32, u32)>>>;

/// Raw resource authority and semantic evidence from the same guarded sample read.
#[derive(Clone, PartialEq, Eq)]
pub(in crate::conversion) struct SegmentReceipt {
    pub evidence: SegmentEvidence,
    pub fingerprint: [u8; 32],
    pub identity: FileIdentity,
}
impl std::fmt::Debug for SegmentReceipt {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SegmentReceipt")
            .field("evidence", &self.evidence)
            .field("fingerprint", &"[redacted]")
            .field("identity", &"[bound]")
            .finish()
    }
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
    #[cfg(test)]
    pub(super) fn protection_layout(
        &self,
        resource: &Path,
        keys: &InputKeys,
        stop: &Stop,
    ) -> Result<ProtectionLayout, ConversionError> {
        let (mut reader, layout, initialization) = self.prepare(resource, keys, stop)?;
        let mut tracks = vec![Vec::new(); initialization.tracks.len()];
        for range in &layout.moofs {
            let fragment = fragment::parse(&mut reader, range, &layout, &initialization)?;
            for part in &fragment.tracks {
                let track = initialization.tracks.get(part.index).ok_or(BAD)?;
                let mut auxiliary = source_auxiliary(&fragment, part, track)?.ok_or(BAD)?;
                let mut ordinal = 0;
                for run in &part.runs {
                    run.walk(&fragment.bytes, stop, &mut ordinal, &mut |info| {
                        let protection = auxiliary.next(info.size)?;
                        tracks[part.index].push(
                            protection
                                .subsamples
                                .into_iter()
                                .map(|span| (span.clear, span.encrypted))
                                .collect(),
                        );
                        Ok(())
                    })?;
                }
                auxiliary.finish()?;
            }
        }
        reader.finish()?;
        self.check_initialization(stop)?;
        Ok(tracks)
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
        if initialization
            .movie
            .tracks
            .iter()
            .all(|track| track.sample_count == 0)
        {
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
    #[cfg(test)]
    pub fn scan(
        &self,
        resource: &Path,
        keys: &InputKeys,
        stop: &Stop,
        visitor: &mut impl FnMut(&TrackDescriptor, &SampleInfo, &[u8]) -> Result<(), ConversionError>,
    ) -> Result<Movie, ConversionError> {
        self.scan_with_evidence(resource, keys, stop, visitor)
            .map(|(movie, _)| movie)
    }
    #[cfg(test)]
    pub fn evidence(
        &self,
        resource: &Path,
        keys: &InputKeys,
        stop: &Stop,
    ) -> Result<SegmentEvidence, ConversionError> {
        self.scan_with_evidence(resource, keys, stop, &mut |_, _, _| Ok(()))
            .map(|(_, receipt)| receipt.evidence)
    }
    /// The returned receipt describes exactly the guarded payload delivered to the visitor.
    pub fn scan_with_evidence(
        &self,
        resource: &Path,
        keys: &InputKeys,
        stop: &Stop,
        visitor: &mut impl FnMut(&TrackDescriptor, &SampleInfo, &[u8]) -> Result<(), ConversionError>,
    ) -> Result<(Movie, SegmentReceipt), ConversionError> {
        let (mut reader, layout, initialization) = self.prepare(resource, keys, stop)?;
        let fingerprint = reader.expected_fingerprint().ok_or(BAD)?;
        let identity = FileIdentity::read(&reader.file).map_err(|_| BAD)?;
        let mut hashes = initialization
            .tracks
            .iter()
            .map(|track| TrackHash::new(&track.descriptor))
            .collect::<Vec<_>>();
        for (fragment_index, range) in layout.moofs.iter().enumerate() {
            let fragment = fragment::parse(&mut reader, range, &layout, &initialization)?;
            for (part_index, part) in fragment.tracks.iter().enumerate() {
                let hash = hashes.get_mut(part.index).ok_or(BAD)?;
                hash.grouping.update((fragment_index as u64).to_be_bytes());
                hash.grouping.update((part_index as u64).to_be_bytes());
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
                &mut |_, _| Ok(()),
                &mut |descriptor, info, bytes| {
                    hashes
                        .get_mut(descriptor.index)
                        .ok_or(BAD)?
                        .record(info, bytes)?;
                    visitor(descriptor, info, bytes)
                },
            )?;
        }
        reader.finish()?;
        self.check_initialization(stop)?;
        let evidence = SegmentEvidence {
            fragment_count: u64::try_from(layout.moofs.len()).map_err(|_| BAD)?,
            tracks: hashes
                .into_iter()
                .filter(|hash| hash.count != 0)
                .map(TrackHash::finish)
                .collect::<Result<_, _>>()?,
        };
        Ok((
            initialization.movie,
            SegmentReceipt {
                evidence,
                fingerprint,
                identity,
            },
        ))
    }
}

/// Rewrites each original resource without joining resources or changing sample grouping.
pub(in crate::conversion) struct SegmentTransformer<'a> {
    source: SegmentInput,
    scheme: super::Scheme,
    key: Option<&'a super::super::OutputKey>,
    constant_ivs: Vec<Zeroizing<[u8; 16]>>,
}
impl<'a> SegmentTransformer<'a> {
    pub fn new(
        source_init: &Path,
        output_init: &Path,
        target: crate::inspection::TargetProtection,
        key: Option<&'a super::super::OutputKey>,
        stop: &Stop,
    ) -> Result<Self, ConversionError> {
        use crate::inspection::TargetProtection;
        let scheme = match target {
            TargetProtection::Clear => super::Scheme::Clear,
            TargetProtection::Cenc => super::Scheme::Cenc,
            TargetProtection::Cbcs => super::Scheme::Cbcs,
            _ => return Err(UNSUPPORTED),
        };
        if scheme != super::Scheme::Clear && key.is_none() {
            return Err(ConversionError("output-key-required"));
        }
        let source = SegmentInput::new(source_init, stop)?;
        for track in &source.movie.tracks {
            match &track.codec {
                b"avc1" | b"avc3" => {
                    super::super::ts::avc::Configuration::from_avcc(&track.decoder_config)?;
                }
                b"mp4a" => {
                    super::AacConfiguration::parse(&track.decoder_config)?;
                }
                _ => return Err(UNSUPPORTED),
            }
        }
        let parsed = init::parse(&source.layout)?;
        let constant_ivs = if scheme == super::Scheme::Cbcs {
            source
                .movie
                .tracks
                .iter()
                .map(|_| super::encrypt::constant_iv())
                .collect::<Result<Vec<_>, _>>()?
        } else {
            Vec::new()
        };
        let bytes = super::write::initialization(
            &source.original,
            &source.layout,
            &parsed,
            scheme,
            key.map(|key| &key.0.id),
            &constant_ivs,
            stop,
        )?;
        let mut output = super::write::Output::new(output_init)?;
        for bytes in bytes.chunks(CHUNK_BYTES) {
            read::check_stop(stop)?;
            output.write(bytes)?;
        }
        source.check_initialization(stop)?;
        output.finish()?;
        Ok(Self {
            source,
            scheme,
            key,
            constant_ivs,
        })
    }
    #[cfg(test)]
    pub fn source(&self) -> &SegmentInput {
        &self.source
    }
    #[cfg(test)]
    pub fn movie(&self) -> &Movie {
        self.source.movie()
    }
    pub fn rewrite_segment(
        &self,
        resource: &Path,
        output_path: &Path,
        keys: &InputKeys,
        stop: &Stop,
    ) -> Result<SegmentEvidence, ConversionError> {
        let (mut reader, layout, initialization) = self.source.prepare(resource, keys, stop)?;
        let mut state = ResourceState {
            hashes: initialization
                .tracks
                .iter()
                .map(|track| TrackHash::new(&track.descriptor))
                .collect(),
            ordinals: vec![0; initialization.tracks.len()],
        };
        let mut output = super::write::Output::new(output_path)?;
        // Regenerated manifests own the timeline; old byte-index and DRM records are omitted.
        for (fragment_index, range) in layout.moofs.iter().enumerate() {
            let fragment = fragment::parse(&mut reader, range, &layout, &initialization)?;
            let source = SourceFragment {
                fragment: &fragment,
                initialization: &initialization,
                keys,
            };
            let starts = state.ordinals.clone();
            let plans = self.plan_fragment(&mut reader, &source, fragment_index, &mut state)?;
            let (moof, mdat) = super::write::fragment_header(
                &fragment,
                &plans,
                &initialization.movie.tracks,
                self.scheme,
            )?;
            read::check_stop(stop)?;
            output.write(&moof)?;
            output.write(&mdat)?;
            self.write_payload(&mut reader, &mut output, &source, &plans, starts)?;
        }
        reader.finish()?;
        self.source.check_initialization(stop)?;
        let evidence = SegmentEvidence {
            fragment_count: u64::try_from(layout.moofs.len()).map_err(|_| BAD)?,
            tracks: state
                .hashes
                .into_iter()
                .filter(|hash| hash.count != 0)
                .map(TrackHash::finish)
                .collect::<Result<_, _>>()?,
        };
        read::check_stop(stop)?;
        output.finish()?;
        Ok(evidence)
    }
    fn plan_fragment(
        &self,
        reader: &mut read::Reader<'_>,
        source: &SourceFragment<'_>,
        fragment_index: usize,
        state: &mut ResourceState,
    ) -> Result<Vec<super::write::OutputPart>, ConversionError> {
        let stop = reader.stop;
        let fragment = source.fragment;
        let mut plans = Vec::new();
        let mut budget = 0_usize;
        for (part_index, part) in fragment.tracks.iter().enumerate() {
            let track = source.initialization.tracks.get(part.index).ok_or(BAD)?;
            let hash = state.hashes.get_mut(part.index).ok_or(BAD)?;
            hash.grouping.update((fragment_index as u64).to_be_bytes());
            hash.grouping.update((part_index as u64).to_be_bytes());
            hash.grouping.update(part.count.to_be_bytes());
            hash.grouping.update((part.runs.len() as u64).to_be_bytes());
            let first_ordinal = state.ordinals[part.index];
            let mut auxiliary = source_auxiliary(fragment, part, track)?;
            let mut runs = Vec::new();
            for run in &part.runs {
                hash.grouping.update(run.count.to_be_bytes());
                let mut samples = Vec::new();
                run.walk(
                    &fragment.bytes,
                    stop,
                    &mut state.ordinals[part.index],
                    &mut |mut info| {
                        (info.roll_distance, info.preroll_distance) = part
                            .groups
                            .at(info.ordinal.checked_sub(first_ordinal).ok_or(BAD)?);
                        let encryption = auxiliary
                            .as_mut()
                            .map(|aux| aux.next(info.size))
                            .transpose()?;
                        let sample = self.plan_sample(
                            reader,
                            super::Payload {
                                track,
                                info,
                                encryption,
                            },
                            source.keys,
                            hash,
                        )?;
                        budget =
                            budget
                                .checked_add(std::mem::size_of::<super::write::OutputSample>())
                                .and_then(|n| {
                                    n.checked_add(sample.spans.len().checked_mul(
                                        std::mem::size_of::<super::crypto::Subsample>(),
                                    )?)
                                })
                                .filter(|n| *n <= MAX_METADATA_BYTES)
                                .ok_or(ConversionError("resource-budget"))?;
                        samples.push(sample);
                        Ok(())
                    },
                )?;
                runs.push(samples);
            }
            if let Some(auxiliary) = auxiliary {
                auxiliary.finish()?;
            }
            plans.push(super::write::OutputPart {
                index: part.index,
                runs,
            });
        }
        Ok(plans)
    }
    fn plan_sample(
        &self,
        reader: &mut read::Reader<'_>,
        sample: super::Payload<'_>,
        keys: &InputKeys,
        hash: &mut TrackHash,
    ) -> Result<super::write::OutputSample, ConversionError> {
        let stop = reader.stop;
        let track = sample.track;
        let info = sample.info.clone();
        let video = track.descriptor.kind == crate::inspection::TrackKind::Video;
        let buffered = self.scheme != super::Scheme::Clear && video;
        let mut data = Zeroizing::new(Vec::new());
        if buffered {
            if info.size > 8 * 1024 * 1024 {
                return Err(ConversionError("ts-access-unit-size-limit"));
            }
            data.try_reserve_exact(usize::try_from(info.size).map_err(|_| BAD)?)
                .map_err(|_| ConversionError("resource-budget"))?;
        }
        let mut digest = Sha256::new();
        super::payload(reader, keys, sample, &mut |_, part_info, bytes| {
            hash.record(part_info, bytes)?;
            digest.update(bytes);
            if buffered {
                data.extend_from_slice(bytes);
            }
            Ok(())
        })?;
        let (iv, spans) = if self.scheme == super::Scheme::Clear {
            (None, Vec::new())
        } else {
            let spans = if video {
                super::encrypt::subsamples(&track.descriptor, &data, self.scheme, stop)?
            } else {
                vec![super::crypto::Subsample {
                    clear: 0,
                    encrypted: info.size,
                }]
            };
            let iv = if self.scheme == super::Scheme::Cbcs {
                self.constant_ivs
                    .get(track.descriptor.index)
                    .ok_or(BAD)?
                    .clone()
            } else {
                super::encrypt::iv()?
            };
            (Some(iv), spans)
        };
        Ok(super::write::OutputSample {
            info,
            iv,
            spans,
            digest: digest.finalize().into(),
        })
    }
    fn write_payload(
        &self,
        reader: &mut read::Reader<'_>,
        output: &mut super::write::Output,
        source: &SourceFragment<'_>,
        plans: &[super::write::OutputPart],
        mut ordinals: Vec<u64>,
    ) -> Result<(), ConversionError> {
        let stop = reader.stop;
        let fragment = source.fragment;
        for (part, plan) in fragment.tracks.iter().zip(plans) {
            let track = source.initialization.tracks.get(part.index).ok_or(BAD)?;
            let first_ordinal = ordinals[part.index];
            let mut auxiliary = source_auxiliary(fragment, part, track)?;
            for (run, samples) in part.runs.iter().zip(&plan.runs) {
                let mut plans = samples.iter();
                run.walk(
                    &fragment.bytes,
                    stop,
                    &mut ordinals[part.index],
                    &mut |mut info| {
                        (info.roll_distance, info.preroll_distance) = part
                            .groups
                            .at(info.ordinal.checked_sub(first_ordinal).ok_or(BAD)?);
                        let encryption = auxiliary
                            .as_mut()
                            .map(|aux| aux.next(info.size))
                            .transpose()?;
                        let plan = plans.next().ok_or(BAD)?;
                        if info != plan.info {
                            return Err(ConversionError("native-source-changed"));
                        }
                        let mut writer = super::write::SampleWriter::new(
                            output,
                            plan,
                            self.scheme,
                            self.key.map(|key| &*key.0.bytes),
                            track.descriptor.kind == crate::inspection::TrackKind::Video,
                        )?;
                        super::payload(
                            reader,
                            source.keys,
                            super::Payload {
                                track,
                                info,
                                encryption,
                            },
                            &mut |_, _, bytes| writer.push(bytes),
                        )?;
                        writer.finish()
                    },
                )?;
                if plans.next().is_some() {
                    return Err(BAD);
                }
            }
            if let Some(auxiliary) = auxiliary {
                auxiliary.finish()?;
            }
        }
        Ok(())
    }
}

struct SourceFragment<'a> {
    fragment: &'a fragment::Fragment,
    initialization: &'a init::Initialization,
    keys: &'a InputKeys,
}
struct ResourceState {
    hashes: Vec<TrackHash>,
    ordinals: Vec<u64>,
}

fn source_auxiliary<'a>(
    fragment: &'a fragment::Fragment,
    part: &fragment::TrackFragment,
    track: &'a init::Track,
) -> Result<Option<super::auxiliary::Auxiliary<'a>>, ConversionError> {
    track
        .encryption
        .as_ref()
        .map(|protection| {
            super::auxiliary::Auxiliary::new(
                &part.auxiliary,
                &fragment.bytes,
                protection,
                usize::try_from(part.count).map_err(|_| BAD)?,
                fragment.source_base,
                part.auxiliary_base,
            )
        })
        .transpose()
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
    pub(super) fn record(
        &mut self,
        info: &SampleInfo,
        bytes: &[u8],
    ) -> Result<(), ConversionError> {
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
            self.end_pts = self
                .end_pts
                .max(info.pts.checked_add(i64::from(info.duration)).ok_or(BAD)?);
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
        self.received = self
            .received
            .checked_add(u32::try_from(bytes.len()).map_err(|_| BAD)?)
            .ok_or(BAD)?;
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
