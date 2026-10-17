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
//! A tool that cannot be run (spawn failure, timeout) is a [`ToolError`] naming the tool and
//! the kind of failure only, never the argv.

use crate::canonical::python_float_repr;
use crate::events::{Record, Sink, now_iso, round3};
use base64::Engine as _;
use base64::engine::general_purpose::{STANDARD, URL_SAFE};
use regex::Regex;
use serde_json::Value;
use std::collections::BTreeSet;
use std::ffi::{OsStr, OsString};
use std::fmt::{self, Write as _};
use std::io::Read;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, PoisonError, RwLock, mpsc};
use std::time::{Duration, Instant};

/// The placeholder every scrubbed secret becomes (the same one the implementations use).
pub const REDACTED: &str = "***REDACTED***";
/// Default bound on each of stdout and stderr in a record, in characters.
pub const DEFAULT_MAX_OUTPUT_CHARS: usize = 256 * 1024;
/// Shorter strings would redact unrelated text; keys and IVs are 32 hex characters.
const MIN_SECRET_CHARS: usize = 8;
/// Between two bytes of a hex secret, as tools print them. Python's `\s` also matches
/// U+001C–U+001F (`str.isspace`), which Rust's `\s` (Unicode `White_Space`) does not.
const BYTE_SEPARATOR: &str = r"[\s\x1c-\x1f,:\-]*(?:0[xX])?";

/// The patterns that match `secret` in every encoding (see the module documentation); empty
/// for a secret shorter than eight characters.
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
        patterns.push(compile(&format!(
            "(?i)(?:0[xX])?{}",
            pairs.join(BYTE_SEPARATOR)
        )));
        vec![
            hex::decode(secret).expect("validated hex"),
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

/// A tool could not be run to completion: a spawn failure (`unavailable`) or a `timeout`.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{tool}: {kind}")]
pub struct ToolError {
    /// The tool's name as recorded (`packager`, `ffmpeg`, `ffprobe`, `mp4dump`).
    pub tool: String,
    /// `timeout` or `unavailable`.
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
    /// `false` keeps stdout out of the record (output consumed as data): the record notes
    /// its size only.
    pub log_stdout: bool,
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
            .field("log_stdout", &self.log_stdout)
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
            log_stdout: true,
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
    path: Option<OsString>,
}

impl ToolRunner {
    /// A runner emitting to `sink`, scrubbing with `secrets`; its tools resolve on this
    /// process's `PATH`.
    pub fn new(secrets: Arc<Secrets>, sink: Arc<dyn Sink>) -> Self {
        ToolRunner {
            secrets,
            sink,
            path: None,
        }
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
        self.sink
            .emit(&self.command_record(&base, options.argv_redacted.unwrap_or(argv), &extra));
        let start = Instant::now();
        match execute(argv, options.timeout, self.path.as_deref()) {
            Ok(completed) => {
                let record = self.output_record(
                    &base,
                    options,
                    &extra,
                    &completed,
                    start.elapsed().as_secs_f64(),
                );
                self.sink.emit(&record);
                Ok(completed)
            }
            Err(failure) => {
                let record =
                    self.failure_record(&base, &extra, &failure, start.elapsed().as_secs_f64());
                self.sink.emit(&record);
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
        self.sink.emit(&record);
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
        self.sink.emit(&record);
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

fn execute(
    argv: &[String],
    timeout: Option<Duration>,
    path: Option<&OsStr>,
) -> Result<Completed, Failure> {
    let Some((program, arguments)) = argv.split_first() else {
        return Err(Failure {
            kind: "unavailable",
            text: "could not run: empty command".to_string(),
        });
    };
    let mut command = Command::new(program);
    if let Some(path) = path {
        // A bare program name is then looked up on this PATH (execvp over the child's
        // environment), as Python's subprocess looks it up on the modified os.environ.
        command.env("PATH", path);
    }
    let mut child = command
        .args(arguments)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| Failure {
            kind: "unavailable",
            text: format!("could not run: {error}"),
        })?;
    let stdout = child.stdout.take().expect("stdout is piped");
    let stderr = child.stderr.take().expect("stderr is piped");
    let deadline = timeout.map(|limit| Instant::now() + limit);
    // The readers hand their bytes over channels: a pipe can stay open after the child exits
    // (a grandchild inherited it), and like Python's `communicate(timeout)` the call gives up
    // on it at the deadline. A reader that outlives the deadline ends when its pipe closes.
    let (out_tx, out_rx) = mpsc::channel();
    let (err_tx, err_rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = out_tx.send(read_all(stdout));
    });
    std::thread::spawn(move || {
        let _ = err_tx.send(read_all(stderr));
    });
    let status = match wait_until(&mut child, deadline) {
        Ok(Some(status)) => status,
        Ok(None) => return Err(gave_up(timeout)),
        Err(error) => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(Failure {
                kind: "unavailable",
                text: format!("could not run: {error}"),
            });
        }
    };
    let (Some(stdout), Some(stderr)) =
        (recv_until(&out_rx, deadline), recv_until(&err_rx, deadline))
    else {
        return Err(gave_up(timeout));
    };
    Ok(Completed {
        returncode: exit_code(status),
        stdout,
        stderr,
    })
}

/// Giving up on the child: the timeout when one was set. With no deadline, a reader that
/// ended without its bytes (its thread panicked) is not a timeout but an unavailable tool.
fn gave_up(timeout: Option<Duration>) -> Failure {
    match timeout {
        Some(limit) => Failure {
            kind: "timeout",
            text: format!("timed out after {}s", seconds_text(limit)),
        },
        None => Failure {
            kind: "unavailable",
            text: "could not read the output".to_string(),
        },
    }
}

/// The child's exit status, or `None` once `deadline` passed first: the child is then killed
/// and reaped. An error from the wait also kills and reaps it (the caller).
fn wait_until(child: &mut Child, deadline: Option<Instant>) -> std::io::Result<Option<ExitStatus>> {
    let Some(deadline) = deadline else {
        return child.wait().map(Some);
    };
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(Some(status));
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Ok(None);
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// A reader's bytes, or `None` when `deadline` passed before its pipe closed.
fn recv_until(rx: &mpsc::Receiver<Vec<u8>>, deadline: Option<Instant>) -> Option<Vec<u8>> {
    match deadline {
        None => rx.recv().ok(),
        Some(deadline) => rx
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .ok(),
    }
}

fn read_all(mut reader: impl Read) -> Vec<u8> {
    let mut buffer = Vec::new();
    let _ = reader.read_to_end(&mut buffer);
    buffer
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
    compile(&format!(
        "{}{}",
        if ignore_case { "(?i)" } else { "" },
        regex::escape(text)
    ))
}

fn compile(pattern: &str) -> Regex {
    Regex::new(pattern).expect("escaped literals and a fixed separator always compile")
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
