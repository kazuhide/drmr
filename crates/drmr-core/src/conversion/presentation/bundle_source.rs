//! One owned, immutable transport preparation feeds independently checked outputs.
use super::{
    BundleAdmission, ConversionError, ConversionReport, ConversionRequest, Converter, LocalTrack,
    OutputFormat, Prepared, SourceValidationPurpose, TrackClock, TrackConversionRequest,
    VerificationSource, describe_transport, metadata, prepare_presentation, secure_tree,
    validate_output, validate_prepared_output, workspace,
};
use crate::{
    conversion::{backend::Backend, files},
    native::FileIdentity,
    resources::{Claim, Permit, ReservationError},
    sha256::Sha256,
};
use std::{
    collections::BTreeSet,
    fs::File,
    io::{Read as _, Write as _},
    path::{Path, PathBuf},
};

const CHANGED: ConversionError = ConversionError("shared-preparation-changed");
const BUFFER: usize = 64 * 1024;

struct Receipt {
    path: PathBuf,
    identity: FileIdentity,
    bytes: u64,
    digest: [u8; 32],
}

fn permit(backend: &Backend) -> Result<Permit, ConversionError> {
    backend
        .runner
        .native_permit(
            Claim {
                cpu_threads: 1,
                memory_bytes: 2 * BUFFER as u64,
            },
            &backend.stop,
            backend.timeout,
        )
        .map_err(|error| match error {
            ReservationError::Cancelled => ConversionError("cancelled"),
            ReservationError::Insufficient => ConversionError("resource-budget"),
            ReservationError::Timeout => ConversionError("media-tool-timeout"),
        })
}

fn digest(
    input: &mut File,
    mut output: Option<&mut File>,
    bytes: u64,
    backend: &Backend,
) -> Result<[u8; 32], ConversionError> {
    let mut hash = Sha256::new();
    let mut read = 0_u64;
    let mut buffer = vec![0; BUFFER];
    loop {
        backend.check_cancelled()?;
        let count = input.read(&mut buffer).map_err(|_| CHANGED)?;
        if count == 0 {
            return if read == bytes {
                Ok(hash.finalize())
            } else {
                Err(CHANGED)
            };
        }
        read = read
            .checked_add(count as u64)
            .filter(|read| *read <= bytes)
            .ok_or(CHANGED)?;
        if let Some(output) = &mut output {
            output.write_all(&buffer[..count]).map_err(|_| CHANGED)?;
        }
        hash.update(&buffer[..count]);
    }
}

impl Receipt {
    fn capture(path: &Path, backend: &Backend) -> Result<Self, ConversionError> {
        let _permit = permit(backend)?;
        backend.check_cancelled()?;
        let mut input = files::regular_input(path)?;
        let identity = FileIdentity::read(&input).map_err(|_| CHANGED)?;
        let bytes = input.metadata().map_err(|_| CHANGED)?.len();
        let digest = digest(&mut input, None, bytes, backend)?;
        let receipt = Self {
            path: path.into(),
            identity,
            bytes,
            digest,
        };
        receipt.unchanged(&input)?;
        Ok(receipt)
    }

    fn unchanged(&self, held: &File) -> Result<(), ConversionError> {
        super::super::validate_held_input(held)?;
        let current = files::regular_input(&self.path).map_err(|_| CHANGED)?;
        if FileIdentity::read(held).map_err(|_| CHANGED)? != self.identity
            || FileIdentity::read(&current).map_err(|_| CHANGED)? != self.identity
            || held.metadata().map_err(|_| CHANGED)?.len() != self.bytes
            || current.metadata().map_err(|_| CHANGED)?.len() != self.bytes
        {
            return Err(CHANGED);
        }
        Ok(())
    }

    fn check(&self, backend: &Backend) -> Result<(), ConversionError> {
        let _permit = permit(backend)?;
        let mut input = files::regular_input(&self.path).map_err(|_| CHANGED)?;
        self.unchanged(&input)?;
        if digest(&mut input, None, self.bytes, backend)? != self.digest {
            return Err(CHANGED);
        }
        self.unchanged(&input)
    }

    fn copy(
        &self,
        owner: &files::Stage,
        output: &Path,
        backend: &Backend,
    ) -> Result<(), ConversionError> {
        let _permit = permit(backend)?;
        backend.check_cancelled()?;
        let mut input = files::regular_input(&self.path).map_err(|_| CHANGED)?;
        self.unchanged(&input)?;
        let mut destination = owner.create_file(output)?;
        if digest(&mut input, Some(&mut destination), self.bytes, backend)? != self.digest {
            return Err(CHANGED);
        }
        destination.flush().map_err(|_| CHANGED)?;
        self.unchanged(&input)?;
        backend.check_cancelled()
    }
}

pub(in crate::conversion) struct BundlePrepared<'a> {
    source: VerificationSource<'a>,
    work: files::Stage,
    input: crate::inspection::Inspection,
    prepared: Vec<Prepared>,
    sources: Vec<Prepared>,
    tracks: Vec<LocalTrack>,
    clears: Vec<PathBuf>,
    receipts: Vec<Receipt>,
}

impl BundlePrepared<'_> {
    pub(in crate::conversion) fn check(&self, command: &Converter) -> Result<(), ConversionError> {
        self.work.directory_source()?;
        command.run_tracks(self.receipts.len(), |index| {
            self.receipts[index].check(&command.backend)
        })?;
        self.work.directory_source()?;
        Ok(())
    }

    fn copy_clears(
        &self,
        owner: &files::Stage,
        command: &Converter,
    ) -> Result<Vec<PathBuf>, ConversionError> {
        self.work.directory_source()?;
        command.run_tracks(self.clears.len(), |index| {
            let receipt = self
                .receipts
                .iter()
                .find(|receipt| receipt.path == self.clears[index])
                .ok_or(CHANGED)?;
            let path = owner.path().join(format!("packaging-{index:04}.mp4"));
            receipt.copy(owner, &path, &command.backend)?;
            Ok(path)
        })
    }
}

impl Converter {
    pub(in crate::conversion) fn prepare_bundle_presentation<'a>(
        &self,
        source: VerificationSource<'a>,
    ) -> Result<BundlePrepared<'a>, ConversionError> {
        let work = workspace()?;
        let input = self.inspect_input(source.input)?;
        let stage = self
            .backend
            .scope(super::super::measurement::Phase::InputPreparation);
        let source_dir = work.path().join("source");
        files::create_dir(&source_dir)?;
        let prepared = prepare_presentation(
            &source,
            &source_dir,
            &self.backend,
            self.maximum_workers,
            SourceValidationPurpose::Conversion,
        )?;
        let mut tracks = Vec::new();
        self.verify_source_tracks(&prepared, &mut tracks, source.input_keys)?;
        let clocks: Vec<_> = prepared.iter().map(TrackClock::from_prepared).collect();
        let normalized = work.path().join("normalized");
        files::create_dir(&normalized)?;
        let request = TrackConversionRequest {
            tracks: &tracks,
            output: &normalized,
            format: OutputFormat::Dash,
            target: crate::inspection::TargetProtection::Clear,
            input_keys: source.input_keys,
            output_key: None,
            hls_key_uri: None,
            hls_output_key: None,
        };
        let (mut sources, clears) = self.normalize_tracks(&request, &normalized, Some(&clocks))?;
        for (source, original) in sources.iter_mut().zip(&prepared) {
            source.audio_presentation = original.audio_presentation;
            source.metadata = original.metadata.clone();
        }
        metadata::materialize(&mut sources, &self.backend, source.input_keys)?;
        secure_tree(work.path())?;
        let mut paths = BTreeSet::new();
        for track in prepared.iter().chain(&sources) {
            paths.insert(track.source.clone());
            paths.insert(track.media.clone());
            paths.extend(track.transport_demux_view.iter().cloned());
        }
        paths.extend(clears.iter().cloned());
        let paths = paths.into_iter().collect::<Vec<_>>();
        if paths.iter().any(|path| !path.starts_with(work.path())) {
            return Err(CHANGED);
        }
        let receipts = self.run_tracks(paths.len(), |index| {
            Receipt::capture(&paths[index], &self.backend)
        })?;
        work.directory_source()?;
        self.backend.checkpoint(&[&work]);
        drop(stage);
        Ok(BundlePrepared {
            source,
            work,
            input,
            prepared,
            sources,
            tracks,
            clears,
            receipts,
        })
    }

    pub(super) fn convert_prepared_bundle(
        &self,
        request: ConversionRequest<'_>,
        admitted: &BundleAdmission<'_>,
        shared: &BundlePrepared<'_>,
    ) -> Result<ConversionReport, ConversionError> {
        if shared.source.input != admitted.source.input
            || !std::ptr::eq(shared.source.input_keys, request.input_keys)
            || !std::ptr::eq(shared.source.hls_keys, request.hls_input_keys)
            || self.processing_mode != super::super::ProcessingMode::Stitch
        {
            return Err(ConversionError("source-admission-mismatch"));
        }
        self.validate_signaling_input(&request, Some(admitted.qualified))?;
        validate_output(&request)?;
        validate_prepared_output(&request, &shared.prepared, true)?;
        self.validate_complete_clock_output(&request, &shared.prepared)?;
        let command =
            self.using_prepared_output(&request, &shared.prepared, shared.input.format)?;
        let destination = super::destination(request.output)?;
        let stage = self.output_stage(&destination)?;
        let work = workspace()?;
        // Packaging temporarily rebases audio clocks. Each child owns those writable copies.
        let clears = shared.copy_clears(&work, self)?;
        let track_request = TrackConversionRequest {
            tracks: &shared.tracks,
            output: stage.path(),
            format: request.format,
            target: request.target,
            input_keys: request.input_keys,
            output_key: request.output_key,
            hls_key_uri: request.hls_key_uri,
            hls_output_key: request.hls_output_key,
        };
        command.validate_signaling_tracks(&track_request, true)?;
        super::validate_request(&track_request)?;
        for source in &shared.sources {
            let profile = self
                .backend
                .profile_stream_keys(&source.media, source.index, None)?;
            self.validate_signaling_codec(&profile, source.report.tracks[0].kind)?;
        }
        let mut report = command.package_prepared_tracks(
            &track_request,
            Some(&stage),
            None,
            &shared.sources,
            &clears,
            &work,
        )?;
        report.verification = self.verify_transport_output(
            &request,
            shared.prepared.clone(),
            stage.path(),
            work.path(),
        )?;
        let manifest = stage.path().join(if request.format == OutputFormat::Dash {
            "manifest.mpd"
        } else {
            "master.m3u8"
        });
        report.verification.native_manifest = Some(command.publish_manifest_output(
            &stage,
            &destination,
            &manifest,
            request.format,
        )?);
        report.input = shared.input.clone();
        describe_transport(
            &self.backend,
            &mut report,
            shared.prepared.iter().any(|track| track.transport_sample),
            request.format == OutputFormat::Dash,
        );
        self.backend.checkpoint(&[&shared.work, &work, &stage]);
        Ok(report)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        conversion::{
            CheckOutcome, DecodeEvidence, HlsInputKeys, InputKeys, ProcessingMode, SourceInventory,
            SourceQualification, ValidationDepth, VerificationChecks, VerificationReport,
            VerificationScope,
        },
        events::NoSink,
        inspection::{Evidence, InputFormat, Inspection, TargetProtection},
        toolrun::{Secrets, ToolRunner},
    };
    use std::sync::Arc;

    fn command() -> Converter {
        Converter::new(
            Arc::new(ToolRunner::new(Arc::new(Secrets::new()), Arc::new(NoSink))),
            "must-not-start-packager",
            "must-not-start-decoder",
        )
        .with_processing(ProcessingMode::Stitch)
    }

    fn source(owner: &files::Stage) -> PathBuf {
        let path = owner.path().join("source.mp4");
        owner
            .create_file(&path)
            .unwrap()
            .write_all(b"same-length clear data")
            .unwrap();
        path
    }

    #[test]
    fn shared_receipt_refuses_same_length_content_mutation_and_identical_replacement() {
        for replace in [false, true] {
            let command = command();
            let owner = workspace().unwrap();
            let path = source(&owner);
            let receipt = Receipt::capture(&path, &command.backend).unwrap();
            if replace {
                std::fs::rename(&path, owner.path().join("original.mp4")).unwrap();
                std::fs::write(&path, b"same-length clear data").unwrap();
            } else {
                std::fs::write(&path, b"same-length clear datb").unwrap();
            }
            let child = workspace().unwrap();
            let destination = child.path().join("input.mp4");
            assert_eq!(
                receipt
                    .copy(&child, &destination, &command.backend)
                    .unwrap_err(),
                CHANGED
            );
            assert_eq!(receipt.check(&command.backend).unwrap_err(), CHANGED);
            assert!(path.exists());
            if replace {
                assert!(!destination.exists());
            }
        }
    }

    #[test]
    fn packaging_copies_are_distinct_objects_and_cannot_modify_shared_or_sibling_bytes() {
        let command = command();
        let owner = workspace().unwrap();
        let path = source(&owner);
        let receipt = Receipt::capture(&path, &command.backend).unwrap();
        let children = [workspace().unwrap(), workspace().unwrap()];
        let copies = children
            .iter()
            .map(|child| {
                let output = child.path().join("input.mp4");
                receipt.copy(child, &output, &command.backend).unwrap();
                output
            })
            .collect::<Vec<_>>();
        let first = files::regular_input(&copies[0]).unwrap();
        let second = files::regular_input(&copies[1]).unwrap();
        assert!(FileIdentity::read(&first).unwrap() != FileIdentity::read(&second).unwrap());
        assert!(FileIdentity::read(&first).unwrap() != receipt.identity);
        std::fs::write(&copies[0], b"same-length clear datb").unwrap();
        receipt.check(&command.backend).unwrap();
        assert_eq!(
            std::fs::read(&copies[1]).unwrap(),
            b"same-length clear data"
        );
        assert_eq!(std::fs::read(&path).unwrap(), b"same-length clear data");
    }

    #[test]
    fn cancelled_shared_copy_opens_no_destination_and_preserves_the_source() {
        let command = command();
        let owner = workspace().unwrap();
        let path = source(&owner);
        let receipt = Receipt::capture(&path, &command.backend).unwrap();
        let child = workspace().unwrap();
        let output = child.path().join("input.mp4");
        command.backend.stop.set();
        assert_eq!(
            receipt
                .copy(&child, &output, &command.backend)
                .unwrap_err()
                .0,
            "cancelled"
        );
        assert!(!output.exists());
        assert_eq!(std::fs::read(path).unwrap(), b"same-length clear data");
    }

    #[test]
    fn bundle_admission_rejects_new_key_borrows_depth_and_input_before_any_source_read() {
        let input = Path::new("must-not-open-source.m3u8");
        let keys = InputKeys::default();
        let other_keys = InputKeys::default();
        let hls = HlsInputKeys::default();
        let other_hls = HlsInputKeys::default();
        let inventory = SourceInventory {
            schema_version: 1,
            root: PathBuf::from("unused"),
            entrypoint: PathBuf::from("unused"),
            files: vec![],
            graph_digest: String::new(),
        };
        let qualified = SourceQualification {
            layout_tracks: vec![],
            hls_timing: None,
            input: Inspection::new(
                InputFormat::Hls,
                Evidence::ContainerMetadata,
                vec![],
                vec![],
                vec![],
            ),
            actual_format: OutputFormat::HlsTs,
            track_protection: vec![],
            compatibility_notes: vec![],
            eligible_formats: vec![],
            verification: VerificationReport::new(
                ValidationDepth::Native,
                VerificationScope::SourceAdmission,
                VerificationChecks {
                    stream_metadata: CheckOutcome::Pass,
                    packet_timing_equal: true,
                    packet_payloads_equal: true,
                },
                DecodeEvidence::not_requested(),
            ),
        };
        let admitted = BundleAdmission {
            source: VerificationSource {
                input,
                input_keys: &keys,
                hls_keys: &hls,
            },
            inventory: &inventory,
            qualified: &qualified,
            processing: ProcessingMode::Stitch,
            depth: ValidationDepth::Native,
        };
        let request = |input, keys, hls| ConversionRequest {
            input,
            output: Path::new("must-not-create"),
            format: OutputFormat::Dash,
            target: TargetProtection::Clear,
            input_keys: keys,
            hls_input_keys: hls,
            output_key: None,
            hls_key_uri: None,
            hls_output_key: None,
        };
        for request in [
            request(input, &other_keys, &hls),
            request(input, &keys, &other_hls),
            request(Path::new("other.m3u8"), &keys, &hls),
        ] {
            assert_eq!(
                command()
                    .convert_bundle_admitted(request, &admitted, None)
                    .unwrap_err()
                    .0,
                "source-admission-mismatch"
            );
        }
        assert_eq!(
            command()
                .with_decode(true)
                .convert_bundle_admitted(request(input, &keys, &hls), &admitted, None)
                .unwrap_err()
                .0,
            "source-admission-mismatch"
        );
    }
}
