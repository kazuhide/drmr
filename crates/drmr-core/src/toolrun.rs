//! Running external tools with full, scrubbed records (`toolrun`).
//!
//! Every invocation emits a `command` record before and a `tool-output` record after, sharing
//! a `call_id`, so the operator sees the exact command and what the tool printed. Secret
//! hygiene has two layers: a caller whose argv carries key material passes the
//! redacted argv and that call's secrets; and the job's registered secrets (the content key,
//! the IV, the clear-HLS `LA_URL` and the `skd://` URI) are scrubbed from every record in every
//! encoding a tool may print them in — contiguous hex in any case, bytes separated by spaces,
//! colons, commas or `0x` prefixes, base64; a text secret as written, XML-escaped,
//! percent-encoded, as UTF-8 and UTF-16LE hex and as base64 of both. The JSON-Lines writer
//! applies the same scrub to each serialized line as the last guard.
//!
//! A tool that cannot complete (spawn failure, timeout, capture limit) is a [`ToolError`] naming the tool and
//! the kind of failure only, never the argv.

use crate::canonical::python_float_repr;
use crate::concurrency::Stop;
use crate::events::{Record, Sink, now_iso, round3};
use crate::resources::{Claim, Limiter, Permit, Workload, process_limiter};
use base64::Engine as _;
use base64::engine::general_purpose::{STANDARD, URL_SAFE};
use regex::Regex;
use serde_json::Value;
use std::collections::BTreeSet;
use std::ffi::{OsStr, OsString};
use std::fmt::{self, Write as _};
use std::process::ExitStatus;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, PoisonError, RwLock};
use std::time::{Duration, Instant};

/// The placeholder every scrubbed secret becomes (the same one the implementations use).
pub const REDACTED: &str = "***REDACTED***";

#[cfg(test)]
mod native_admission_tests {
    use super::*;
    #[test]
    fn native_waiter_cancels_while_the_custom_pool_remains_saturated() {
        let runner = Arc::new(
            ToolRunner::new(Arc::new(Secrets::new()), Arc::new(crate::events::NoSink))
                .with_limiter(Arc::new(Limiter::new(1, 256))),
        );
        let claim = Claim {
            cpu_threads: 1,
            memory_bytes: 128,
        };
        let held = runner
            .native_permit(claim, &Stop::new(), Duration::from_secs(1))
            .unwrap();
        let stop = Stop::new();
        let stopped = stop.clone();
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let waiter = std::thread::spawn(move || {
            ready_tx.send(()).unwrap();
            runner.native_permit(claim, &stopped, Duration::from_secs(1))
        });
        ready_rx.recv().unwrap();
        stop.set();
        assert_eq!(
            waiter.join().unwrap().unwrap_err(),
            crate::resources::ReservationError::Cancelled
        );
        // The waiter's capacity never became available, so cancellation must be
        // observed by the shared admission wait rather than by a later tool.
        drop(held);
    }
}
/// Default bound on each of stdout and stderr in a record, in characters.
pub const DEFAULT_MAX_OUTPUT_CHARS: usize = 256 * 1024;
/// Hard default capture limits, independent of log truncation. Oversized data fails.
pub const DEFAULT_STDOUT_BYTES: usize = 16 * 1024 * 1024;
/// Diagnostic output is bounded independently from parsed stdout.
pub const DEFAULT_STDERR_BYTES: usize = 1024 * 1024;
/// Maximum configurable capture per stream.
pub const MAX_CAPTURE_BYTES: usize = 64 * 1024 * 1024;

mod capture;
mod cleanup;
#[cfg(test)]
mod cohort_tests;
pub use cleanup::CleanupOwner;
pub(crate) use cleanup::CleanupReceipt;
/// Shorter strings would redact unrelated text; keys and IVs are 32 hex characters.
const MIN_SECRET_CHARS: usize = 8;
/// Between two bytes of a hex secret, as tools print them. Python's `\s` also matches
/// U+001C–U+001F (`str.isspace`), which Rust's `\s` (Unicode `White_Space`) does not.
const BYTE_SEPARATOR: &str = r"[\s\x1c-\x1f,:\-]*(?:0[xX])?";
const MAX_EXACT_HEX_BYTES: usize = 1024;
const HEX_EDGE_BYTES: usize = 64;
const MAX_EXACT_LITERAL_CHARS: usize = 4096;
const LITERAL_EDGE_CHARS: usize = 512;

/// The patterns that match `secret` in every encoding (see the module documentation); empty
/// for a secret shorter than eight characters.
/// Large opaque values use bounded edges and also hide spans that share those edges.
pub fn patterns_for(secret: &str) -> Vec<Regex> {
    let secret = py_strip(secret);
    if secret.chars().count() < MIN_SECRET_CHARS {
        return Vec::new();
    }
    let even_hex = is_even_hex(secret);
    let mut patterns = vec![literal(secret, true)];
    let mut encoded: BTreeSet<String> = BTreeSet::new();
    let escaped = xml_escape(secret);
    let payloads: Vec<Vec<u8>> = if even_hex {
        let pairs: Vec<String> = (0..secret.len())
            .step_by(2)
            .map(|index| regex::escape(&secret[index..index + 2]))
            .collect();
        let body = if pairs.len() <= MAX_EXACT_HEX_BYTES {
            pairs.join(BYTE_SEPARATOR)
        } else {
            // Very large opaque metadata is identified by bounded edges. Ambiguous
            // spans between the same edges are deliberately hidden as well.
            format!(
                "{}{BYTE_SEPARATOR}(?:[0-9a-f]{{2}}{BYTE_SEPARATOR})*{}",
                pairs[..HEX_EDGE_BYTES].join(BYTE_SEPARATOR),
                pairs[pairs.len() - HEX_EDGE_BYTES..].join(BYTE_SEPARATOR),
            )
        };
        patterns.push(compile(&format!("(?i)(?:0[xX])?{body}")));
        vec![
            hex::decode(secret).expect("validated hex"),
            secret.as_bytes().to_vec(),
            secret.to_ascii_lowercase().into_bytes(),
            secret.to_ascii_uppercase().into_bytes(),
        ]
    } else {
        // As written and XML-escaped (the LA_URL sits inside the PlayReady object's XML, so an
        // `&` in it is `&amp;` there), each as UTF-8 and UTF-16LE bytes; longest form first.
        let texts: Vec<&str> = if escaped == secret {
            vec![secret]
        } else {
            vec![escaped.as_str(), secret]
        };
        let payloads: Vec<Vec<u8>> = texts
            .iter()
            .flat_map(|text| [text.as_bytes().to_vec(), utf16le(text)])
            .collect();
        for text in &texts {
            encoded.insert(percent_encode(text));
        }
        for text in &texts[..texts.len() - 1] {
            patterns.push(literal(text, true));
        }
        for data in &payloads {
            patterns.push(literal(&hex::encode(data), true));
        }
        payloads
    };
    for data in &payloads {
        for text in [STANDARD.encode(data), URL_SAFE.encode(data)] {
            encoded.insert(text.trim_end_matches('=').to_string());
            encoded.insert(text);
        }
    }
    let mut ordered: Vec<String> = encoded.into_iter().collect();
    ordered.sort_by_key(|text| std::cmp::Reverse(text.chars().count()));
    let lowered = secret.to_lowercase();
    for text in ordered {
        if text.to_lowercase() != lowered {
            patterns.push(literal(&text, !even_hex));
        }
    }
    // Legacy key material admits ASCII whitespace between complete byte pairs.
    // Keep its original text encodings above and also cover the same bytes in
    // canonical hex. Do not turn whitespace-padded short text into broad masks.
    if !even_hex && let Ok(bytes) = crate::pycompat::fromhex(secret) {
        let bytes = zeroize::Zeroizing::new(bytes);
        let canonical = zeroize::Zeroizing::new(hex::encode(bytes.as_slice()));
        if canonical.len() >= MIN_SECRET_CHARS {
            patterns.extend(patterns_for(canonical.as_str()));
        }
    }
    patterns
}

/// `text` with every match of `patterns` replaced by [`REDACTED`].
pub fn scrub_with(patterns: &[Regex], text: &str) -> String {
    let mut current = text.to_string();
    for pattern in patterns {
        if pattern.is_match(&current) {
            current = pattern.replace_all(&current, REDACTED).into_owned();
        }
    }
    current
}

/// The job's secrets as patterns for every encoding.
///
/// `Debug` prints the number of patterns only: a pattern is the escaped secret, and a
/// `{:?}` of a registry (or of anything holding one) must never reach a log or an error.
#[derive(Default)]
pub struct SecretRegistry {
    patterns: Vec<Regex>,
}

impl fmt::Debug for SecretRegistry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SecretRegistry")
            .field("patterns", &self.patterns.len())
            .finish()
    }
}

impl SecretRegistry {
    /// An empty registry.
    pub fn new() -> Self {
        SecretRegistry::default()
    }

    /// Add secrets: from now on every scrub covers them in every encoding.
    pub fn register<I, S>(&mut self, secrets: I)
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        for secret in secrets {
            self.patterns.extend(patterns_for(secret.as_ref()));
        }
    }

    /// Forget every secret.
    pub fn clear(&mut self) {
        self.patterns.clear();
    }

    /// Whether nothing is registered.
    pub fn is_empty(&self) -> bool {
        self.patterns.is_empty()
    }

    /// `text` with the registered secrets replaced in every encoding.
    pub fn scrub(&self, text: &str) -> String {
        scrub_with(&self.patterns, text)
    }

    /// [`SecretRegistry::scrub`], also covering `extra` (one call's own secrets).
    pub fn scrub_extra(&self, text: &str, extra: &[&str]) -> String {
        if extra.is_empty() {
            return self.scrub(text);
        }
        let mut patterns = self.patterns.clone();
        for secret in extra {
            patterns.extend(patterns_for(secret));
        }
        scrub_with(&patterns, text)
    }
}

/// A registry shared by a job's threads, sinks and runner.
///
/// `Debug` prints the number of patterns only (see [`SecretRegistry`]).
#[derive(Default)]
pub struct Secrets {
    registry: RwLock<SecretRegistry>,
}

impl fmt::Debug for Secrets {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let patterns = self
            .registry
            .read()
            .map_or(0, |registry| registry.patterns.len());
        f.debug_struct("Secrets")
            .field("patterns", &patterns)
            .finish()
    }
}

impl Secrets {
    /// An empty, shareable registry.
    pub fn new() -> Self {
        Secrets::default()
    }

    /// A registry holding `secrets`.
    pub fn with<I, S>(secrets: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let shared = Secrets::new();
        shared.register(secrets);
        shared
    }

    /// Add secrets (callers that load them after the sink is installed, such as the clear-HLS
    /// DRM parameters).
    pub fn register<I, S>(&self, secrets: I)
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        self.registry
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .register(secrets);
    }

    /// Forget every secret (clearing the sink in Python forgets them the same way).
    pub fn clear(&self) {
        self.registry
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .clear();
    }

    /// `text` with the registered secrets replaced in every encoding.
    pub fn scrub(&self, text: &str) -> String {
        self.registry
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .scrub(text)
    }

    /// [`Secrets::scrub`], also covering `extra` — whose patterns are compiled on every call.
    /// For many texts with the same extra secrets, compile them once with [`patterns_for`]
    /// and use [`Secrets::scrub_with_patterns`], as `run_tool` does.
    pub fn scrub_extra(&self, text: &str, extra: &[&str]) -> String {
        self.registry
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .scrub_extra(text, extra)
    }

    /// [`Secrets::scrub`], then `extra` patterns the caller compiled once ([`patterns_for`]):
    /// a tool run scrubs its per-call secrets from every argv token and both outputs without
    /// recompiling them per token.
    pub fn scrub_with_patterns(&self, text: &str, extra: &[Regex]) -> String {
        scrub_with(extra, &self.scrub(text))
    }
}

/// Python's text mode (`universal_newlines`): `\r\n` and a lone `\r` become `\n`. The tool
/// records of a text-mode call carry the output this way; the caller's bytes stay raw.
pub fn universal_newlines(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}

/// `text` cut to `limit` characters, with a note of what is not shown.
pub fn bounded(text: &str, limit: usize) -> (String, bool) {
    let total = text.chars().count();
    if total <= limit {
        return (text.to_string(), false);
    }
    let mut kept: String = text.chars().take(limit).collect();
    write!(kept, "\n… ({} characters not shown)", total - limit)
        .expect("String never fails to write");
    (kept, true)
}

/// A tool could not complete, including capture, resource, timeout, and spawn failures.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{tool}: {kind}")]
pub struct ToolError {
    /// The tool's name as recorded (`packager`, `ffmpeg`, `ffprobe`, `mp4dump`).
    pub tool: String,
    /// A fixed classification, without arguments or untrusted tool output.
    pub kind: &'static str,
}

/// What a tool returned: the real, unscrubbed output for the caller's own use.
///
/// `Debug` prints the exit code and the output sizes only: the output may carry key material
/// (mp4dump prints the tenc constant IV), so a `{:?}` of a result must never reach a log.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct Completed {
    /// The exit code; a negative signal number when a signal ended the process; `None` when
    /// unknown.
    pub returncode: Option<i32>,
    /// Captured stdout.
    pub stdout: Vec<u8>,
    /// Captured stderr.
    pub stderr: Vec<u8>,
}

impl fmt::Debug for Completed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Completed")
            .field("returncode", &self.returncode)
            .field("stdout_bytes", &self.stdout.len())
            .field("stderr_bytes", &self.stderr.len())
            .finish()
    }
}

impl Completed {
    /// stdout as text (invalid UTF-8 replaced).
    pub fn stdout_text(&self) -> String {
        String::from_utf8_lossy(&self.stdout).into_owned()
    }

    /// stderr as text (invalid UTF-8 replaced).
    pub fn stderr_text(&self) -> String {
        String::from_utf8_lossy(&self.stderr).into_owned()
    }
}

/// Per-call options of [`ToolRunner::run_tool`].
///
/// `Debug` prints the number of per-call secrets and the number of redacted argv tokens,
/// never their text: a redacted argv may still carry a `PlayReady` PSSH whose hex encodes the
/// operator's `LA_URL` (R2: never logged on the clear-HLS path), which only the record writer
/// scrubs.
#[derive(Clone, Copy)]
pub struct RunOptions<'a> {
    /// What is logged instead of the argv when it carries key material.
    pub argv_redacted: Option<&'a [String]>,
    /// This call's own secrets, scrubbed from its records in addition to the job's.
    pub secrets: &'a [&'a str],
    /// Kill the tool and fail with `timeout` after this long.
    pub timeout: Option<Duration>,
    /// Whether the output is text (affects only the wording of an unlogged stdout note).
    pub text: bool,
    /// The rendition the call belongs to, recorded when given.
    pub rendition_id: Option<&'a str>,
    /// Bound on each of stdout and stderr in the record.
    pub max_output_chars: usize,
    /// Maximum captured stdout bytes, capped at [`MAX_CAPTURE_BYTES`]. Overflow fails.
    pub max_stdout_bytes: usize,
    /// Maximum captured stderr bytes, capped at [`MAX_CAPTURE_BYTES`]. Overflow fails.
    pub max_stderr_bytes: usize,
    /// `false` keeps stdout out of the record (output consumed as data): the record notes
    /// its size only.
    pub log_stdout: bool,
    /// Optional known media for this call; it can only raise an already registered job estimate.
    pub workload: Option<Workload>,
    /// Cooperative cancellation while waiting to admit a child. Running children drain normally.
    pub stop: Option<&'a Stop>,
}

impl fmt::Debug for RunOptions<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RunOptions")
            .field("argv_redacted", &self.argv_redacted.map(<[String]>::len))
            .field("secrets", &self.secrets.len())
            .field("timeout", &self.timeout)
            .field("text", &self.text)
            .field("rendition_id", &self.rendition_id)
            .field("max_output_chars", &self.max_output_chars)
            .field("max_stdout_bytes", &self.max_stdout_bytes)
            .field("max_stderr_bytes", &self.max_stderr_bytes)
            .field("log_stdout", &self.log_stdout)
            .field("workload", &self.workload)
            .field("stop", &self.stop.is_some())
            .finish()
    }
}

impl Default for RunOptions<'_> {
    fn default() -> Self {
        RunOptions {
            argv_redacted: None,
            secrets: &[],
            timeout: None,
            text: true,
            rendition_id: None,
            max_output_chars: DEFAULT_MAX_OUTPUT_CHARS,
            max_stdout_bytes: DEFAULT_STDOUT_BYTES,
            max_stderr_bytes: DEFAULT_STDERR_BYTES,
            log_stdout: true,
            workload: None,
            stop: None,
        }
    }
}

/// Call ids are process-wide, as Python's `_next_call_id` counter is: every runner in the
/// process draws from one sequence, so a log never shows two calls with the same id.
static CALLS: AtomicU64 = AtomicU64::new(0);

/// `str.strip`: Unicode whitespace plus U+001C–U+001F, which Python's `isspace` counts and
/// Rust's `char::is_whitespace` does not.
fn py_strip(text: &str) -> &str {
    text.trim_matches(|c: char| c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c))
}

fn next_call_id() -> u64 {
    CALLS.fetch_add(1, Ordering::SeqCst) + 1
}

/// Runs tools and emits their records to a sink, scrubbing with a shared registry.
pub struct ToolRunner {
    secrets: Arc<Secrets>,
    sink: Arc<dyn Sink>,
    observer: Option<Arc<dyn Sink>>,
    path: Option<OsString>,
    limiter: Arc<Limiter>,
    cpu_scope: Option<Arc<crate::resources::CpuScope>>,
    workload: RwLock<Option<Workload>>,
    cleanup_owner: Option<Arc<CleanupOwner>>,
}

impl ToolRunner {
    /// A runner emitting to `sink`, scrubbing with `secrets`; its tools resolve on this
    /// process's `PATH`.
    pub fn new(secrets: Arc<Secrets>, sink: Arc<dyn Sink>) -> Self {
        ToolRunner {
            secrets,
            sink,
            observer: None,
            path: None,
            limiter: process_limiter(),
            cpu_scope: None,
            workload: RwLock::new(None),
            cleanup_owner: None,
        }
    }

    /// Preserve the original sink, admission pool, and secrets while observing this fork only.
    pub(crate) fn observed(&self, observer: Arc<dyn Sink>) -> Self {
        Self {
            secrets: Arc::clone(&self.secrets),
            sink: Arc::clone(&self.sink),
            observer: Some(observer),
            path: self.path.clone(),
            limiter: Arc::clone(&self.limiter),
            cpu_scope: self.cpu_scope.clone(),
            workload: RwLock::new(self.registered_workload()),
            cleanup_owner: self.cleanup_owner.clone(),
        }
    }

    /// Fork without changing observers or an existing serial cleanup owner.
    /// The caller keeps its workspace until an unconfirmed tree is reaped.
    pub(crate) fn ensuring_cleanup_owner(&self) -> (Self, Arc<CleanupOwner>) {
        let owner = self
            .cleanup_owner
            .clone()
            .unwrap_or_else(|| Arc::new(CleanupOwner::new()));
        let runner = Self {
            secrets: Arc::clone(&self.secrets),
            sink: Arc::clone(&self.sink),
            observer: self.observer.clone(),
            path: self.path.clone(),
            limiter: Arc::clone(&self.limiter),
            cpu_scope: self.cpu_scope.clone(),
            workload: RwLock::new(self.registered_workload()),
            cleanup_owner: Some(Arc::clone(&owner)),
        };
        (runner, owner)
    }

    /// Explicitly retain one unconfirmed child tree instead of dropping its owner.
    /// Keep the supplied owner alive and retry cleanup before permitting host exit.
    #[must_use]
    pub fn with_cleanup_owner(mut self, owner: Arc<CleanupOwner>) -> Self {
        self.cleanup_owner = Some(owner);
        self
    }

    fn emit(&self, record: &Record) {
        if let Some(observer) = &self.observer {
            observer.emit(record);
        }
        self.sink.emit(record);
    }

    /// Every tool of this runner looked up on and run with `path` as its `PATH` — what a
    /// Python job sees after `cli`'s `--packager-bin` prepends a directory to
    /// `os.environ["PATH"]` (a Rust process does not change its own environment).
    #[must_use]
    pub fn with_path(mut self, path: impl Into<OsString>) -> Self {
        self.path = Some(path.into());
        self
    }

    /// The `PATH` this runner's tools resolve on, when it is not this process's.
    pub fn path(&self) -> Option<&OsStr> {
        self.path.as_deref()
    }

    /// Use an enclosing application's allocation. Default runners share the process limiter.
    #[must_use]
    pub fn with_limiter(mut self, limiter: Arc<Limiter>) -> Self {
        self.limiter = limiter;
        self
    }

    pub(crate) fn cohort(&self, maximum_workers: usize) -> Self {
        let scope = self
            .cpu_scope
            .as_ref()
            .filter(|scope| scope.limit() <= maximum_workers)
            .cloned()
            .unwrap_or_else(|| {
                crate::resources::CpuScope::new(maximum_workers, self.cpu_scope.clone())
            });
        Self {
            secrets: Arc::clone(&self.secrets),
            sink: Arc::clone(&self.sink),
            observer: self.observer.clone(),
            path: self.path.clone(),
            limiter: Arc::clone(&self.limiter),
            cpu_scope: Some(scope),
            workload: RwLock::new(self.registered_workload()),
            cleanup_owner: self.cleanup_owner.clone(),
        }
    }
    /// The enclosing allocation's ceiling for bounded in-process scheduling.
    pub(crate) fn native_capacity(&self) -> (usize, u64) {
        let (cpu, memory) = self.limiter.capacity();
        (
            self.cpu_scope
                .as_ref()
                .map_or(cpu, |scope| cpu.min(scope.limit())),
            memory,
        )
    }

    /// Account for bounded in-process media work in this runner's admission pool.
    /// Callers release this permit before invoking a tool that acquires its own.
    pub(crate) fn native_permit(
        &self,
        claim: Claim,
        stop: &Stop,
        timeout: Duration,
    ) -> Result<Permit, crate::resources::ReservationError> {
        self.reserve_claim(claim, Some(stop), Some(timeout))
    }

    fn reserve_claim(
        &self,
        claim: Claim,
        stop: Option<&Stop>,
        timeout: Option<Duration>,
    ) -> Result<Permit, crate::resources::ReservationError> {
        let capacity = self.native_capacity();
        if claim.cpu_threads == 0
            || claim.cpu_threads > capacity.0
            || claim.memory_bytes > capacity.1
        {
            return Err(crate::resources::ReservationError::Insufficient);
        }
        let started = Instant::now();
        // Wait for the cohort before taking a process-wide token. A blocked
        // command cannot prevent another cohort from using the shared capacity.
        let cpu = self
            .cpu_scope
            .as_ref()
            .map(|scope| scope.acquire(claim.cpu_threads, stop, timeout))
            .transpose()?;
        self.limiter
            .acquire(
                claim,
                stop,
                timeout.map(|limit| limit.saturating_sub(started.elapsed())),
            )
            .map(|permit| permit.with_cpu_scope(cpu))
    }

    /// Reserve enclosing metadata without consuming a child's CPU token.
    pub(crate) fn retain_context(
        &self,
        bytes: u64,
        leaf_floor: u64,
        stop: &Stop,
        timeout: Duration,
    ) -> Result<crate::resources::ContextPermit, crate::resources::ReservationError> {
        self.limiter.retain(bytes, leaf_floor, stop, timeout)
    }

    /// The conservative observation currently registered for this runner.
    pub(crate) fn registered_workload(&self) -> Option<Workload> {
        *self.workload.read().unwrap_or_else(PoisonError::into_inner)
    }

    /// Register known source metadata before scheduling its tools. The first observation
    /// replaces the unknown fallback; later observations can only raise the estimate.
    pub fn register_workload(&self, workload: Workload) {
        let mut current = self
            .workload
            .write()
            .unwrap_or_else(PoisonError::into_inner);
        *current = Some(current.map_or(workload, |previous| previous.merge(workload)));
    }

    /// Record numeric admission choices without source paths, arguments, or machine identifiers.
    pub fn note_resource_budget(&self, budget: &crate::resources::Budget) {
        let mut record = Record::new();
        put(&mut record, "ts", now_iso());
        put(&mut record, "stage", "resources");
        put(&mut record, "level", "info");
        put(&mut record, "message", "resource-budget");
        put(&mut record, "workers", budget.workers);
        put(&mut record, "child_threads", budget.child_threads);
        put(&mut record, "cpu_tokens", budget.cpu_tokens);
        put(
            &mut record,
            "memory_tokens_bytes",
            budget.memory_tokens_bytes,
        );
        put(
            &mut record,
            "estimated_child_bytes",
            budget.estimated_child_bytes,
        );
        put(
            &mut record,
            "memory_source",
            match budget.memory_source {
                crate::resources::MemorySource::Host => "host",
                crate::resources::MemorySource::ProcessCgroup => "process-cgroup",
                crate::resources::MemorySource::Unknown => "unknown",
            },
        );
        self.emit(&record);
    }

    /// Reserve exactly once for a child that streams its output outside `run_tool`.
    /// The caller keeps this guard through child cleanup and never calls `run_tool` inside it.
    pub fn reserve(&self, tool: &str, options: &RunOptions<'_>) -> Result<Permit, ToolError> {
        let current = *self.workload.read().unwrap_or_else(PoisonError::into_inner);
        let workload = match (current, options.workload) {
            (Some(current), Some(call)) => current.merge(call),
            (Some(current), None) => current,
            (None, call) => call.unwrap_or_default(),
        };
        self.reserve_claim(
            Claim::for_workload(workload, options.max_stdout_bytes, options.max_stderr_bytes),
            options.stop,
            options.timeout,
        )
        .map_err(|error| ToolError {
            tool: tool.to_owned(),
            kind: match error {
                crate::resources::ReservationError::Insufficient => "resource-budget",
                crate::resources::ReservationError::Cancelled => "cancelled",
                crate::resources::ReservationError::Timeout => "timeout",
            },
        })
    }

    /// The shared registry.
    pub fn secrets(&self) -> &Arc<Secrets> {
        &self.secrets
    }

    /// Run `argv` with captured output; emit a `command` record before and a `tool-output`
    /// record after; return the real output. A non-zero exit is the caller's to judge.
    pub fn run_tool(
        &self,
        argv: &[String],
        tool: &str,
        options: &RunOptions<'_>,
    ) -> Result<Completed, ToolError> {
        self.run_tool_with(argv, tool, options, false, None, |bounded, lease| {
            capture::execute(argv, bounded, self.path.as_deref(), lease)
        })
    }

    /// Publish invocation-specific cleanup authority immediately after admission.
    /// The caller starts with an empty receipt and retains it through its workspace's Drop.
    pub(crate) fn run_tool_with_cleanup_receipt(
        &self,
        argv: &[String],
        tool: &str,
        options: &RunOptions<'_>,
        receipt: &mut Option<CleanupReceipt>,
    ) -> Result<Completed, ToolError> {
        if receipt.is_some() {
            return Err(ToolError {
                tool: tool.to_owned(),
                kind: "cleanup-unconfirmed",
            });
        }
        self.run_tool_with(
            argv,
            tool,
            options,
            false,
            Some(receipt),
            |bounded, lease| capture::execute(argv, bounded, self.path.as_deref(), lease),
        )
    }

    #[cfg(test)]
    pub(crate) fn run_tool_injected_cleanup(
        &self,
        argv: &[String],
        options: &RunOptions<'_>,
        cleanup: impl FnMut(&mut crate::native::ManagedChild) -> std::io::Result<()>,
    ) -> Result<Completed, ToolError> {
        self.run_tool_with(argv, "fixture", options, false, None, |bounded, lease| {
            capture::execute_injected_cleanup(argv, bounded, self.path.as_deref(), lease, cleanup)
        })
    }

    #[cfg(test)]
    pub(crate) fn run_tool_injected_cleanup_with_receipt(
        &self,
        argv: &[String],
        options: &RunOptions<'_>,
        receipt: &mut Option<CleanupReceipt>,
        cleanup: impl FnMut(&mut crate::native::ManagedChild) -> std::io::Result<()>,
    ) -> Result<Completed, ToolError> {
        self.run_tool_with(
            argv,
            "fixture",
            options,
            false,
            Some(receipt),
            |bounded, lease| {
                capture::execute_injected_cleanup(
                    argv,
                    bounded,
                    self.path.as_deref(),
                    lease,
                    cleanup,
                )
            },
        )
    }

    /// Stream stdout into a bounded parser without retaining or logging its payload.
    /// The caller charges its parser and read buffer through `max_stdout_bytes`.
    pub(crate) fn run_tool_streamed(
        &self,
        argv: &[String],
        tool: &str,
        options: &RunOptions<'_>,
        consume: impl FnMut(&[u8]) -> Result<(), ()> + Send,
    ) -> Result<Completed, ToolError> {
        self.run_tool_with(argv, tool, options, true, None, |bounded, lease| {
            capture::execute_streamed(argv, bounded, self.path.as_deref(), lease, consume)
        })
    }

    fn run_tool_with(
        &self,
        argv: &[String],
        tool: &str,
        options: &RunOptions<'_>,
        streamed: bool,
        receipt: Option<&mut Option<CleanupReceipt>>,
        execute: impl FnOnce(&RunOptions<'_>, Option<&cleanup::Lease<'_>>) -> Result<Completed, Failure>,
    ) -> Result<Completed, ToolError> {
        let base = CallBase {
            tool,
            call_id: next_call_id(),
            rendition_id: options.rendition_id,
        };
        // The per-call secrets' patterns, compiled once per call rather than once per token
        // (about two dozen patterns per secret; a packaging makes thousands of calls).
        let extra: Vec<Regex> = options
            .secrets
            .iter()
            .flat_map(|secret| patterns_for(secret))
            .collect();
        self.emit(&self.command_record(&base, options.argv_redacted.unwrap_or(argv), &extra));
        let start = Instant::now();
        // Hold admission through capture, record construction, and emission, where output
        // may have temporary scrubbed copies. Waiting consumes the same call deadline.
        // Reserve the caller's single ownership slot before waiting for shared
        // resources. A retained child must not deadlock a later call on its permit.
        let lease = self
            .cleanup_owner
            .as_deref()
            .map(CleanupOwner::claim)
            .transpose();
        if let (Some(receipt), Ok(Some(lease)), Some(owner)) =
            (receipt, &lease, &self.cleanup_owner)
        {
            *receipt = Some(lease.receipt(owner));
        }
        let reservation = if lease.is_ok() {
            self.reserve(tool, options)
        } else {
            Err(ToolError {
                tool: tool.to_owned(),
                kind: "cleanup-unconfirmed",
            })
        };
        let result = match &reservation {
            Ok(_) => {
                let mut bounded = *options;
                bounded.timeout = options
                    .timeout
                    .map(|limit| limit.saturating_sub(start.elapsed()));
                execute(&bounded, lease.as_ref().ok().and_then(Option::as_ref))
            }
            Err(error) => Err(Failure {
                kind: error.kind,
                text: format!("tool admission failed: {}", error.kind),
            }),
        }
        .map_err(|mut failure| {
            if failure.kind == "timeout"
                && let Some(limit) = options.timeout
            {
                failure.text = format!("timed out after {}s", seconds_text(limit));
            }
            failure
        });
        let mut permit = reservation.ok();
        if result
            .as_ref()
            .is_err_and(|error| error.kind == "cleanup-unconfirmed")
            && let Ok(Some(lease)) = &lease
            && let Some(permit) = permit.take()
        {
            lease.retain_permit(permit);
        }
        match result {
            Ok(completed) => {
                let mut record = self.output_record(
                    &base,
                    options,
                    &extra,
                    &completed,
                    start.elapsed().as_secs_f64(),
                );
                if streamed {
                    put(
                        &mut record,
                        "stdout",
                        "stdout streamed without retaining payload",
                    );
                }
                self.emit(&record);
                Ok(completed)
            }
            Err(failure) => {
                let record =
                    self.failure_record(&base, &extra, &failure, start.elapsed().as_secs_f64());
                self.emit(&record);
                Err(ToolError {
                    tool: tool.to_string(),
                    kind: failure.kind,
                })
            }
        }
    }

    fn command_record(&self, base: &CallBase<'_>, shown: &[String], extra: &[Regex]) -> Record {
        let mut record = Record::new();
        put(&mut record, "ts", now_iso());
        put(&mut record, "level", "info");
        put(&mut record, "message", "command");
        put(&mut record, "event", "command");
        put(
            &mut record,
            "argv",
            Value::Array(
                shown
                    .iter()
                    .map(|token| Value::String(self.secrets.scrub_with_patterns(token, extra)))
                    .collect(),
            ),
        );
        base.apply(&mut record);
        record
    }

    fn failure_record(
        &self,
        base: &CallBase<'_>,
        extra: &[Regex],
        failure: &Failure,
        elapsed: f64,
    ) -> Record {
        let mut record = Record::new();
        put(&mut record, "ts", now_iso());
        put(&mut record, "level", "error");
        put(&mut record, "message", "tool-output");
        put(&mut record, "event", "tool-output");
        put(&mut record, "returncode", Value::Null);
        put(&mut record, "duration_s", round3(elapsed));
        put(&mut record, "stdout", "");
        put(
            &mut record,
            "stderr",
            self.secrets.scrub_with_patterns(&failure.text, extra),
        );
        put(&mut record, "truncated", false);
        base.apply(&mut record);
        record
    }

    fn output_record(
        &self,
        base: &CallBase<'_>,
        options: &RunOptions<'_>,
        extra: &[Regex],
        completed: &Completed,
        elapsed: f64,
    ) -> Record {
        // Text mode decodes like Python's `text=True`: universal newlines. Bytes mode decodes
        // for the record only, newlines as printed.
        let (stdout_text, stderr_text) = if options.text {
            (
                universal_newlines(&completed.stdout_text()),
                universal_newlines(&completed.stderr_text()),
            )
        } else {
            (completed.stdout_text(), completed.stderr_text())
        };
        let (out, out_cut) = if options.log_stdout {
            bounded(
                &self.secrets.scrub_with_patterns(&stdout_text, extra),
                options.max_output_chars,
            )
        } else {
            let (size, unit) = if options.text {
                (stdout_text.chars().count(), "characters")
            } else {
                (completed.stdout.len(), "bytes")
            };
            (
                format!("(stdout not logged: {size} {unit} consumed as data)"),
                false,
            )
        };
        let (err, err_cut) = bounded(
            &self.secrets.scrub_with_patterns(&stderr_text, extra),
            options.max_output_chars,
        );
        let mut record = Record::new();
        put(&mut record, "ts", now_iso());
        put(
            &mut record,
            "level",
            if completed.returncode == Some(0) {
                "info"
            } else {
                "error"
            },
        );
        put(&mut record, "message", "tool-output");
        put(&mut record, "event", "tool-output");
        put(
            &mut record,
            "returncode",
            completed.returncode.map_or(Value::Null, Value::from),
        );
        put(&mut record, "duration_s", round3(elapsed));
        put(&mut record, "stdout", out);
        put(&mut record, "stderr", err);
        put(&mut record, "truncated", out_cut || err_cut);
        base.apply(&mut record);
        record
    }

    /// Record a command whose output the caller consumes as a stream (no capture); returns the
    /// `call_id` to pass to [`ToolRunner::note_result`].
    pub fn note_command(&self, argv: &[String], tool: &str, rendition_id: Option<&str>) -> u64 {
        let call_id = next_call_id();
        let mut record = Record::new();
        put(&mut record, "ts", now_iso());
        put(&mut record, "stage", "tool");
        put(&mut record, "level", "info");
        put(&mut record, "message", "command");
        put(&mut record, "event", "command");
        put(&mut record, "tool", tool);
        put(&mut record, "call_id", call_id);
        put(
            &mut record,
            "argv",
            Value::Array(
                argv.iter()
                    .map(|token| Value::String(self.secrets.scrub(token)))
                    .collect(),
            ),
        );
        if let Some(id) = rendition_id {
            put(&mut record, "rendition_id", id);
        }
        self.emit(&record);
        call_id
    }

    /// Record the completion of a streamed command (see [`ToolRunner::note_command`]).
    pub fn note_result(
        &self,
        tool: &str,
        returncode: Option<i32>,
        duration_s: f64,
        rendition_id: Option<&str>,
        stderr: &str,
        call_id: Option<u64>,
    ) {
        let mut record = Record::new();
        put(&mut record, "ts", now_iso());
        put(&mut record, "stage", "tool");
        put(
            &mut record,
            "level",
            if returncode == Some(0) {
                "info"
            } else {
                "error"
            },
        );
        put(&mut record, "message", "tool-output");
        put(&mut record, "event", "tool-output");
        put(&mut record, "tool", tool);
        put(
            &mut record,
            "returncode",
            returncode.map_or(Value::Null, Value::from),
        );
        put(&mut record, "duration_s", round3(duration_s));
        put(&mut record, "stdout", "");
        put(&mut record, "stderr", self.secrets.scrub(stderr));
        put(&mut record, "truncated", false);
        if let Some(id) = call_id {
            put(&mut record, "call_id", id);
        }
        if let Some(id) = rendition_id {
            put(&mut record, "rendition_id", id);
        }
        self.emit(&record);
    }
}

fn put(record: &mut Record, key: &str, value: impl Into<Value>) {
    record.insert(key.to_string(), value.into());
}

/// What every record of one invocation carries: `stage`, `tool`, `call_id` and, when the
/// call belongs to a rendition, `rendition_id`.
struct CallBase<'a> {
    tool: &'a str,
    call_id: u64,
    rendition_id: Option<&'a str>,
}

impl CallBase<'_> {
    fn apply(&self, record: &mut Record) {
        put(record, "stage", "tool");
        put(record, "tool", self.tool);
        put(record, "call_id", self.call_id);
        if let Some(id) = self.rendition_id {
            put(record, "rendition_id", id);
        }
    }
}

struct Failure {
    kind: &'static str,
    text: String,
}

/// The timeout as Python prints the configured number (`f"{timeout}s"`): the config's integer
/// without a fraction (`30s`), otherwise the float's repr (`0.2s`).
fn seconds_text(limit: Duration) -> String {
    if limit.subsec_nanos() == 0 {
        limit.as_secs().to_string()
    } else {
        python_float_repr(limit.as_secs_f64())
    }
}

#[cfg(unix)]
fn exit_code(status: ExitStatus) -> Option<i32> {
    use std::os::unix::process::ExitStatusExt;
    status
        .code()
        .or_else(|| status.signal().map(|number| -number))
}

#[cfg(not(unix))]
fn exit_code(status: ExitStatus) -> Option<i32> {
    status.code()
}

fn is_even_hex(text: &str) -> bool {
    !text.is_empty()
        && text.len().is_multiple_of(2)
        && text.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn literal(text: &str, ignore_case: bool) -> Regex {
    let length = text.chars().count();
    let body = if length <= MAX_EXACT_LITERAL_CHARS {
        regex::escape(text)
    } else {
        let head: String = text.chars().take(LITERAL_EDGE_CHARS).collect();
        let tail: String = text.chars().skip(length - LITERAL_EDGE_CHARS).collect();
        format!("{}(?s:.*){}", regex::escape(&head), regex::escape(&tail))
    };
    compile(&format!(
        "{}{}",
        if ignore_case { "(?i)" } else { "" },
        body,
    ))
}

fn compile(pattern: &str) -> Regex {
    Regex::new(pattern).unwrap_or_else(|_| {
        // If a future regex implementation rejects a bounded escaped expression,
        // suppress the diagnostic rather than expose secret-bearing text or panic.
        Regex::new("(?s:.*)").expect("fixed redaction expression")
    })
}

/// `xml.sax.saxutils.escape`.
fn xml_escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// `urllib.parse.quote(text, safe="")`.
fn percent_encode(text: &str) -> String {
    let mut out = String::with_capacity(text.len() * 3);
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || b"-._~".contains(&byte) {
            out.push(char::from(byte));
        } else {
            write!(out, "%{byte:02X}").expect("String never fails to write");
        }
    }
    out
}

fn utf16le(text: &str) -> Vec<u8> {
    text.encode_utf16().flat_map(u16::to_le_bytes).collect()
}
