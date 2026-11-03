//! Complete transport renditions are inspected on an existing acquisition worker.
use super::{CHANGED, Expected, GRAPH_BYTES, IO, OwnedDownload, Resource, digest};
use crate::{
    conversion::{
        ConversionError, Converter, SourceInventory, SourceQualification, ValidationDepth,
        VerificationSource, backend::Backend, files, presentation,
    },
    resources::ContextPermit,
};
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    path::PathBuf,
    sync::{Mutex, TryLockError},
    time::Duration,
};

pub(super) struct Plan {
    pub playlist: PathBuf,
    pub resources: Vec<PathBuf>,
}

struct Rendition {
    path: PathBuf,
    originals: Expected,
    copied: BTreeMap<PathBuf, (crate::native::FileIdentity, ([u8; 32], u64))>,
    prepared: presentation::acquired::AcquiredRendition,
}

pub(in crate::conversion) struct AcquiredNative {
    work: files::Stage,
    root: PathBuf,
    renditions: Vec<Rendition>,
    retained_bytes: u64,
    _retained: ContextPermit,
}

impl AcquiredNative {
    pub(super) fn retained_bytes(&self) -> u64 {
        self.retained_bytes
    }
    pub(in crate::conversion) fn qualify(
        &self,
        command: &Converter,
        source: VerificationSource<'_>,
        inventory: &SourceInventory,
    ) -> Result<Option<SourceQualification>, ConversionError> {
        if command.backend.validation_depth != ValidationDepth::Native
            || command.processing_mode == crate::conversion::ProcessingMode::Segments
            || inventory.root != self.root
            || !source.input_keys.keys.is_empty()
            || !source.hls_keys.0.is_empty()
        {
            return Ok(None);
        }
        let files = inventory
            .files
            .iter()
            .map(|file| (file.path.as_path(), file))
            .collect::<BTreeMap<_, _>>();
        for rendition in &self.renditions {
            for (path, (hash, bytes)) in &rendition.originals {
                let Some(file) = files.get(path.as_path()) else {
                    return Ok(None);
                };
                if file.bytes != *bytes || file.sha256 != hex::encode(hash) {
                    return Err(CHANGED);
                }
            }
        }
        self.check(&command.backend)?;
        let root = source.input.parent().ok_or(CHANGED)?;
        let tracks = self
            .renditions
            .iter()
            .map(|rendition| {
                Ok((
                    root.join(&rendition.path)
                        .canonicalize()
                        .map_err(|_| CHANGED)?,
                    rendition.prepared.tracks.clone(),
                ))
            })
            .collect::<Result<Vec<_>, ConversionError>>()?;
        let qualified = command.qualify_acquired_hls(source, tracks)?;
        self.check(&command.backend)?;
        if let Some(sink) = &command.backend.activity {
            sink.emit(
                serde_json::json!({"stage":"acquisition", "event":"native-admission-reused", "renditions":self.renditions.len()})
                    .as_object().unwrap(),
            );
        }
        Ok(Some(qualified))
    }

    fn check(&self, backend: &Backend) -> Result<(), ConversionError> {
        let _permit = backend
            .runner
            .native_permit(
                crate::resources::Claim {
                    cpu_threads: 1,
                    memory_bytes: 128 * 1024,
                },
                &backend.stop,
                backend.timeout,
            )
            .map_err(|error| match error {
                crate::resources::ReservationError::Cancelled => ConversionError("cancelled"),
                crate::resources::ReservationError::Insufficient => {
                    ConversionError("resource-budget")
                }
                crate::resources::ReservationError::Timeout => {
                    ConversionError("media-tool-timeout")
                }
            })?;
        self.work.directory_source()?;
        for rendition in &self.renditions {
            for (path, (identity, expected)) in &rendition.copied {
                let held = files::regular_input(path).map_err(|_| CHANGED)?;
                if crate::native::FileIdentity::read(&held).map_err(|_| CHANGED)? != *identity
                    || digest(path, &backend.stop, expected.1)? != *expected
                {
                    return Err(CHANGED);
                }
                let current = files::regular_input(path).map_err(|_| CHANGED)?;
                if crate::native::FileIdentity::read(&current).map_err(|_| CHANGED)? != *identity {
                    return Err(CHANGED);
                }
            }
        }
        self.work.directory_source()?;
        backend.check_cancelled()
    }
}

struct State {
    fetched: Expected,
    pending: Vec<Option<Plan>>,
    dependents: BTreeMap<PathBuf, Vec<usize>>,
    remaining: Vec<usize>,
    ready: VecDeque<usize>,
    renditions: Vec<Rendition>,
    failed: bool,
}

pub(super) struct Preparation<'a> {
    backend: Backend,
    observer: Option<std::sync::Arc<dyn crate::events::Sink>>,
    owned: &'a OwnedDownload,
    work: Mutex<Option<files::Stage>>,
    state: Mutex<State>,
    running: Mutex<()>,
    retained: Mutex<Option<ContextPermit>>,
}

impl<'a> Preparation<'a> {
    pub(super) fn new(
        command: &Converter,
        owned: &'a OwnedDownload,
        plans: Vec<Plan>,
        resources: &[Resource],
        manifests: &Expected,
        workers: usize,
        abort: &crate::concurrency::Stop,
    ) -> Result<Option<Self>, ConversionError> {
        let capacity = command.backend.runner.native_capacity();
        if workers < 2
            || command.backend.validation_depth != ValidationDepth::Native
            || command.processing_mode == crate::conversion::ProcessingMode::Segments
            || capacity.0 < 2
            || capacity.1 < 2 * 1024 * 1024 * 1024
            || plans.is_empty()
            || plans.len() > 64
            || plans
                .iter()
                .any(|plan| plan.resources.is_empty() || plan.resources.len() > 4096)
            || plans.iter().map(|plan| plan.resources.len()).sum::<usize>() > 100_000
        {
            return Ok(None);
        }
        // The graph already bounds all paths and manifest text. This additional
        // charge covers a second bounded graph, compact per-segment native spans,
        // at most two native track configurations per rendition, and receipts.
        let retained = match command.backend.runner.retain_context(
            GRAPH_BYTES as u64,
            GRAPH_BYTES as u64,
            &command.backend.stop,
            Duration::from_millis(1),
        ) {
            Ok(retained) => retained,
            Err(crate::resources::ReservationError::Cancelled) => {
                return Err(ConversionError("cancelled"));
            }
            Err(_) => return Ok(None),
        };
        let Ok(work) = super::super::workspace() else {
            return Ok(None);
        };
        let planned = resources
            .iter()
            .map(|resource| PathBuf::from(&resource.path))
            .collect::<BTreeSet<_>>();
        if plans.iter().any(|plan| {
            !manifests.contains_key(&plan.playlist)
                || plan.resources.iter().any(|path| !planned.contains(path))
        }) {
            return Ok(None);
        }
        let mut backend = command.backend.clone();
        backend.stop = crate::concurrency::Stop::linked(&backend.stop, abort);
        backend.activity = None;
        backend.preparation = None;
        let mut dependents: BTreeMap<PathBuf, Vec<usize>> = BTreeMap::new();
        let mut remaining = Vec::with_capacity(plans.len());
        for (ordinal, plan) in plans.iter().enumerate() {
            let unique = plan.resources.iter().collect::<BTreeSet<_>>();
            remaining.push(unique.len());
            for path in unique {
                dependents.entry(path.clone()).or_default().push(ordinal);
            }
        }
        Ok(Some(Self {
            backend,
            observer: command.backend.activity.clone(),
            owned,
            work: Mutex::new(Some(work)),
            state: Mutex::new(State {
                fetched: manifests.clone(),
                pending: plans.into_iter().map(Some).collect(),
                dependents,
                remaining,
                ready: VecDeque::new(),
                renditions: Vec::new(),
                failed: false,
            }),
            running: Mutex::new(()),
            retained: Mutex::new(Some(retained)),
        }))
    }

    pub(super) fn completed(
        &self,
        path: &str,
        fetched: &crate::source::Fetched,
    ) -> Result<(), ConversionError> {
        let hash = hex::decode(&fetched.sha256)
            .map_err(|_| IO)?
            .try_into()
            .map_err(|_| IO)?;
        {
            let mut state = self.state.lock().map_err(|_| IO)?;
            if state.failed {
                return Ok(());
            }
            let path = PathBuf::from(path);
            if state
                .fetched
                .insert(path.clone(), (hash, fetched.size))
                .is_some()
            {
                return Err(CHANGED);
            }
            if let Some(dependents) = state.dependents.remove(&path) {
                for ordinal in dependents {
                    state.remaining[ordinal] -= 1;
                    if state.remaining[ordinal] == 0 {
                        state.ready.push_back(ordinal);
                    }
                }
            }
        }
        self.drain()
    }

    fn drain(&self) -> Result<(), ConversionError> {
        let _running = match self.running.try_lock() {
            Ok(guard) => guard,
            Err(TryLockError::WouldBlock) => return Ok(()),
            Err(TryLockError::Poisoned(_)) => return Err(IO),
        };
        loop {
            self.backend.check_cancelled()?;
            let job = {
                let mut state = self.state.lock().map_err(|_| IO)?;
                if state.failed {
                    return Ok(());
                }
                let index = state.ready.pop_front();
                index.map(|index| {
                    let plan = state.pending[index].take().unwrap();
                    let expected = std::iter::once(&plan.playlist)
                        .chain(&plan.resources)
                        .map(|path| (path.clone(), state.fetched[path]))
                        .collect();
                    (index, plan, expected)
                })
            };
            let Some((index, plan, expected)) = job else {
                return Ok(());
            };
            let result = self.prepare(index, plan, expected);
            if self.backend.stop.is_set() {
                return Err(ConversionError("cancelled"));
            }
            let mut state = self.state.lock().map_err(|_| IO)?;
            if let Ok(rendition) = result {
                state.renditions.push(rendition);
                if let Some(sink) = &self.observer {
                    sink.emit(
                            serde_json::json!({"stage":"acquisition", "event":"native-rendition-prepared", "completed":state.renditions.len(), "total":state.pending.len()})
                                .as_object().unwrap(),
                        );
                }
            } else {
                // Speculative preparation grants no admission. Unsupported or
                // damaged input keeps the complete download and ordinary errors.
                state.failed = true;
                state.renditions.clear();
                state.pending.clear();
                state.dependents.clear();
                state.remaining.clear();
                state.ready.clear();
                state.fetched.clear();
                // This worker owns the running gate, and native helpers have
                // returned and closed their files. Release only this private
                // speculative stage before returning its memory reservation.
                drop(self.work.lock().map_err(|_| IO)?.take());
                drop(self.retained.lock().map_err(|_| IO)?.take());
            }
        }
    }

    fn prepare(
        &self,
        ordinal: usize,
        plan: Plan,
        originals: Expected,
    ) -> Result<Rendition, ConversionError> {
        let directory = self
            .work
            .lock()
            .map_err(|_| IO)?
            .as_ref()
            .ok_or(IO)?
            .path()
            .join(format!("rendition-{ordinal:04}"));
        files::create_dir(&directory)?;
        let resources = plan
            .resources
            .iter()
            .map(|path| {
                Ok((
                    self.owned
                        .path()
                        .join(path)
                        .canonicalize()
                        .map_err(|_| CHANGED)?,
                    originals[path],
                ))
            })
            .collect::<Result<BTreeMap<_, _>, ConversionError>>()?;
        let prepared = presentation::acquired::prepare(
            &self.owned.path().join(&plan.playlist),
            &directory,
            &self.backend,
            originals[&plan.playlist],
            &resources,
        )?;
        if prepared.resources.len() != plan.resources.len() {
            return Err(CHANGED);
        }
        let mut copied = BTreeMap::new();
        for (path, original) in prepared.resources.iter().zip(&plan.resources) {
            if *path
                != self
                    .owned
                    .path()
                    .join(original)
                    .canonicalize()
                    .map_err(|_| CHANGED)?
            {
                return Err(CHANGED);
            }
        }
        for path in [
            directory.join("reference.m3u8"),
            directory.join("original.m3u8"),
        ] {
            if path.is_file() {
                let bytes = path.metadata().map_err(|_| CHANGED)?.len();
                let expected = digest(&path, &self.backend.stop, bytes)?;
                let held = files::regular_input(&path).map_err(|_| CHANGED)?;
                copied.insert(
                    path.clone(),
                    (
                        crate::native::FileIdentity::read(&held).map_err(|_| CHANGED)?,
                        expected,
                    ),
                );
            }
        }
        Ok(Rendition {
            path: plan.playlist,
            originals,
            copied,
            prepared,
        })
    }

    pub(super) fn finish(self) -> Result<Option<AcquiredNative>, ConversionError> {
        self.drain()?;
        let state = self.state.into_inner().map_err(|_| IO)?;
        if state.failed || state.pending.iter().any(Option::is_some) {
            return Ok(None);
        }
        let mut retained = self.retained.into_inner().map_err(|_| IO)?.ok_or(IO)?;
        let mut retained_bytes = 0_u64;
        for rendition in &state.renditions {
            // Native configurations and at most 4096 compact segment spans have
            // a two-MiB ceiling per track; this does not retain sample payloads.
            retained_bytes = retained_bytes
                .saturating_add(rendition.prepared.tracks.len() as u64 * 2 * 1024 * 1024);
            retained_bytes = retained_bytes
                .saturating_add(rendition.originals[&rendition.path].1.saturating_mul(4));
            for path in rendition.originals.keys().chain(rendition.copied.keys()) {
                retained_bytes = retained_bytes
                    .saturating_add((path.as_os_str().len() as u64 + 512).saturating_mul(4));
            }
        }
        if retained_bytes > GRAPH_BYTES as u64 {
            return Ok(None);
        }
        retained.shrink(retained_bytes).map_err(|_| IO)?;
        Ok(Some(AcquiredNative {
            work: self.work.into_inner().map_err(|_| IO)?.ok_or(IO)?,
            root: self.owned.path().canonicalize().map_err(|_| CHANGED)?,
            renditions: state.renditions,
            retained_bytes,
            _retained: retained,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        events::NoSink,
        native::FileIdentity,
        resources::{Claim, Limiter},
        toolrun::{Secrets, ToolRunner},
    };
    use std::{io::Write as _, sync::Arc};

    fn command() -> Converter {
        Converter::new(
            Arc::new(ToolRunner::new(
                Arc::new(Secrets::default()),
                Arc::new(NoSink),
            )),
            "must-not-start-packager",
            "must-not-start-decoder",
        )
    }

    #[test]
    fn acquired_manifest_metadata_is_bound_to_the_exact_creation_bytes() {
        let owner = super::super::super::workspace().unwrap();
        let playlist = owner.path().join("media.m3u8");
        let original =
            b"#EXTM3U\n#EXT-X-TARGETDURATION:6\n#EXTINF:6,\nmissing.ts\n#EXT-X-ENDLIST\n";
        let mut changed = original.to_vec();
        let digit = changed.iter().position(|byte| *byte == b'6').unwrap();
        changed[digit] = b'8';
        owner
            .create_file(&playlist)
            .unwrap()
            .write_all(&changed)
            .unwrap();
        let directory = owner.path().join("prepared");
        files::create_dir(&directory).unwrap();
        let result = presentation::acquired::prepare(
            &playlist,
            &directory,
            &command().backend,
            super::super::bytes_digest(original),
            &BTreeMap::new(),
        );
        assert_eq!(result.err(), Some(CHANGED));
    }

    #[test]
    fn private_preparation_refuses_changed_bytes_and_identical_file_replacement() {
        for replace in [false, true] {
            let command = command();
            let work = super::super::super::workspace().unwrap();
            let path = work.path().join("copy.ts");
            let bytes = b"private byte-bound transport";
            let mut writer = work.create_file(&path).unwrap();
            writer.write_all(bytes).unwrap();
            let identity = FileIdentity::read(&writer).unwrap();
            drop(writer);
            let copied =
                BTreeMap::from([(path.clone(), (identity, super::super::bytes_digest(bytes)))]);
            let retained = command
                .backend
                .runner
                .retain_context(1024, 1024, &command.backend.stop, Duration::from_secs(1))
                .unwrap();
            let cache = AcquiredNative {
                root: work.path().into(),
                work,
                renditions: vec![Rendition {
                    path: PathBuf::from("media.m3u8"),
                    originals: Expected::new(),
                    copied,
                    prepared: presentation::acquired::AcquiredRendition {
                        resources: Vec::new(),
                        tracks: Vec::new(),
                    },
                }],
                retained_bytes: 1024,
                _retained: retained,
            };
            cache.check(&command.backend).unwrap();
            if replace {
                std::fs::rename(&path, path.with_extension("previous")).unwrap();
                crate::native::create_private_new(&path)
                    .unwrap()
                    .write_all(bytes)
                    .unwrap();
            } else {
                let mut changed = bytes.to_vec();
                changed[0] ^= 1;
                std::fs::write(&path, changed).unwrap();
            }
            assert_eq!(cache.check(&command.backend), Err(CHANGED));
        }
    }

    #[test]
    fn optional_preparation_does_not_wait_for_contended_shared_memory() {
        let limiter = Arc::new(Limiter::new(4, 4_u64 << 30));
        let command = Converter::new(
            Arc::new(
                ToolRunner::new(Arc::new(Secrets::default()), Arc::new(NoSink))
                    .with_limiter(limiter.clone()),
            ),
            "unused",
            "unused",
        );
        let held = limiter
            .acquire(
                Claim {
                    cpu_threads: 1,
                    memory_bytes: (4_u64 << 30) - (64_u64 << 20),
                },
                None,
                Some(Duration::from_secs(1)),
            )
            .unwrap();
        let root = crate::native::private_tempdir().unwrap();
        let owned = OwnedDownload::reserve_source(&root.path().join("source")).unwrap();
        let start = std::time::Instant::now();
        let result = Preparation::new(
            &command,
            &owned,
            vec![Plan {
                playlist: "media.m3u8".into(),
                resources: vec!["segment.ts".into()],
            }],
            &[],
            &Expected::new(),
            2,
            &crate::concurrency::Stop::new(),
        )
        .unwrap();
        assert!(result.is_none());
        assert!(
            start.elapsed() < Duration::from_secs(1),
            "optional preparation waited for the ordinary media timeout"
        );
        drop(held);
        drop(
            limiter
                .acquire(
                    Claim {
                        cpu_threads: 4,
                        memory_bytes: 4_u64 << 30,
                    },
                    None,
                    Some(Duration::from_secs(1)),
                )
                .unwrap(),
        );
    }

    fn failing_transport_inputs(owned: &OwnedDownload) -> (Vec<Plan>, Vec<Resource>, Expected) {
        let manifest = b"#EXTM3U\n#EXT-X-TARGETDURATION:2\n#EXTINF:2,\nfirst.ts\n#EXT-X-ENDLIST\n";
        let later = b"#EXTM3U\n#EXT-X-TARGETDURATION:2\n#EXTINF:2,\nlater.ts\n#EXT-X-ENDLIST\n";
        let invalid = [0x47; 188];
        for (name, bytes) in [
            ("track/first.m3u8", manifest.as_slice()),
            ("track/later.m3u8", later.as_slice()),
            ("track/first.ts", invalid.as_slice()),
        ] {
            super::super::write_registered(owned, std::path::Path::new(name), bytes, |_| {})
                .unwrap();
        }
        let manifests = Expected::from([
            (
                "track/first.m3u8".into(),
                super::super::bytes_digest(manifest),
            ),
            ("track/later.m3u8".into(), super::super::bytes_digest(later)),
        ]);
        let plans = ["first", "later"].map(|name| Plan {
            playlist: format!("track/{name}.m3u8").into(),
            resources: vec![format!("track/{name}.ts").into()],
        });
        let resources = ["first", "later"].map(|name| Resource {
            url: format!("https://fixture.example.invalid/{name}.ts"),
            path: format!("track/{name}.ts"),
        });
        (plans.into(), resources.into(), manifests)
    }

    #[test]
    fn failed_preparation_removes_all_speculative_copies_before_later_fetches_continue() {
        let root = crate::native::private_tempdir().unwrap();
        let owned = OwnedDownload::reserve_source(&root.path().join("source")).unwrap();
        owned.create_dir(std::path::Path::new("track")).unwrap();
        let (plans, resources, manifests) = failing_transport_inputs(&owned);
        let command = Converter::new(
            Arc::new(
                ToolRunner::new(Arc::new(Secrets::default()), Arc::new(NoSink))
                    .with_limiter(Arc::new(Limiter::new(2, 4_u64 << 30))),
            ),
            "must-not-start-packager",
            "must-not-start-decoder",
        );
        let preparation = Preparation::new(
            &command,
            &owned,
            plans,
            &resources,
            &manifests,
            2,
            &crate::concurrency::Stop::new(),
        )
        .unwrap()
        .unwrap();
        let scratch = {
            let work = preparation.work.lock().unwrap();
            let work = work.as_ref().unwrap();
            work.create_file(&work.path().join("prior-success.ts"))
                .unwrap()
                .write_all(b"prior speculative copy")
                .unwrap();
            files::create_dir(&work.path().join("partial")).unwrap();
            files::create_new(&work.path().join("partial/copy.ts"))
                .unwrap()
                .write_all(b"partial speculative copy")
                .unwrap();
            work.private_root().to_path_buf()
        };
        let fetched = |bytes: &[u8]| crate::source::Fetched {
            effective: "https://fixture.example.invalid/media.ts".into(),
            sha256: hex::encode(super::super::bytes_digest(bytes).0),
            size: bytes.len() as u64,
        };
        preparation
            .completed("track/first.ts", &fetched(&[0x47; 188]))
            .unwrap();
        assert!(
            !scratch.exists(),
            "failed optional work retained speculative disk usage"
        );
        assert!(preparation.work.lock().unwrap().is_none());
        assert!(preparation.retained.lock().unwrap().is_none());
        assert_eq!(
            std::fs::read(owned.path().join("track/first.ts")).unwrap(),
            [0x47; 188]
        );
        let remainder = b"later original resource";
        super::super::write_registered(
            &owned,
            std::path::Path::new("track/later.ts"),
            remainder,
            |_| {},
        )
        .unwrap();
        preparation
            .completed("track/later.ts", &fetched(remainder))
            .unwrap();
        owned.check_registered().unwrap();
        assert_eq!(
            std::fs::read(owned.path().join("track/later.ts")).unwrap(),
            remainder
        );
        assert!(preparation.finish().unwrap().is_none());
    }

    #[test]
    fn linked_native_stop_observes_caller_or_sibling_without_poisoning_parent_flags() {
        for caller_cancelled in [false, true] {
            let caller = crate::concurrency::Stop::new();
            let abort = crate::concurrency::Stop::new();
            let linked = crate::concurrency::Stop::linked(&caller, &abort);
            std::thread::scope(|scope| {
                let waiting = scope.spawn(|| linked.wait(Some(Duration::from_secs(1))));
                if caller_cancelled {
                    caller.set();
                } else {
                    abort.set();
                }
                assert!(waiting.join().unwrap());
            });
            assert_eq!(caller.is_set(), caller_cancelled);
            assert_eq!(abort.is_set(), !caller_cancelled);
        }
        let caller = crate::concurrency::Stop::new();
        let abort = crate::concurrency::Stop::new();
        let linked = crate::concurrency::Stop::linked(&caller, &abort);
        assert!(!linked.wait(Some(Duration::from_millis(1))));
        linked.set();
        assert!(!caller.is_set() && !abort.is_set());
    }
}
