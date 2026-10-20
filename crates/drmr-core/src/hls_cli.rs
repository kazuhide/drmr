//! Shared clear-HLS command handlers and private configuration adapters.
//!
//! Invalid configuration produces fixed error tokens without source addresses, key
//! material, configuration bodies, or parser details. Analysis and packaging emit
//! structured records scrubbed against the job's secrets.

use crate::config::{self, KeyMaterial};
use crate::events::{JsonLinesWriter, Record, Sink};
use crate::hls_pipeline::{
    self, DrmSignaling, PackageHlsReport, PackagingSelection, PipelineError,
};
use crate::hls_pipeline_deps;
use crate::playready::{self, PlayReadyObjects, WIDEVINE_SYSTEM_ID};
use crate::pycompat;
use crate::source::{self, Failure, FetchPolicy, SourceAnalysis, SourceRequest};
use crate::toolrun::{Secrets, ToolRunner};
use clap::Args;
use serde_json::{Map, Value};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{Ipv4Addr, Ipv6Addr};
use std::panic::{self, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use zeroize::Zeroize as _;

/// `private_json`'s only error.
pub const PRIVATE_CONFIG_INVALID: &str = "private-config-invalid";
/// `fetch_policy`'s only error (`source::SourceError("invalid-fetch-policy")`).
pub const INVALID_FETCH_POLICY: &str = "invalid-fetch-policy";
/// `drm_parameters`' only error.
pub const DRM_PARAMETERS_INVALID: &str = "drm-parameters-invalid";

/// One of the three tokens above (Python's `ValueError(token)`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct HlsCliError(pub &'static str);

impl HlsCliError {
    /// The token.
    pub fn token(self) -> &'static str {
        self.0
    }
}

/// Largest private config accepted (`2 * 1024 * 1024` bytes).
const PRIVATE_CONFIG_LIMIT: u64 = 2 * 1024 * 1024;

/// `private_json(path)`: a regular operator file of at most 2 MiB, readable by its owner only
/// (no group or other permission bit), holding one JSON object.
pub fn private_json(path: &Path) -> Result<Map<String, Value>, HlsCliError> {
    let invalid = HlsCliError(PRIVATE_CONFIG_INVALID);
    // Validate and read the same held object. The bounded read also covers growth
    // after metadata inspection without allocating an unbounded config body.
    let file = crate::native::open_private_read(path).map_err(|_| invalid)?;
    if file.metadata().map_err(|_| invalid)?.len() > PRIVATE_CONFIG_LIMIT {
        return Err(invalid);
    }
    let mut text = String::new();
    file.take(PRIVATE_CONFIG_LIMIT + 1)
        .read_to_string(&mut text)
        .map_err(|_| invalid)?;
    if text.len() as u64 > PRIVATE_CONFIG_LIMIT {
        return Err(invalid);
    }
    match serde_json::from_str::<Value>(&text) {
        Ok(Value::Object(map)) => Ok(map),
        _ => Err(invalid),
    }
}

// ------------------------------------------------------------------------------------------
// FetchPolicy
// ------------------------------------------------------------------------------------------

/// `fetch_policy(data)`: `dict(data)` ([`py_dict`]), then `FetchPolicy(**policy)` with
/// `allowed_origins` made a tuple and `allowed_local_roots` made paths — the source implementation's
/// [`FetchPolicy::from_json`], which carries `FetchPolicy.__post_init__`'s validation. Any
/// failure is `invalid-fetch-policy`.
pub fn fetch_policy(data: &Value) -> Result<FetchPolicy, HlsCliError> {
    let invalid = HlsCliError(INVALID_FETCH_POLICY);
    let policy = py_dict(data).ok_or(invalid)?;
    FetchPolicy::from_json(&Value::Object(policy)).map_err(|_| invalid)
}

/// `dict(value)` for a JSON value: an object as it is; a list of pairs (each a two-element list
/// or a two-character string) in order, a later key replacing the value of an earlier one; an
/// empty string is `{}`. Any other shape, or a key that is not a string (which
/// `FetchPolicy(**policy)` refuses), is `None`.
pub fn py_dict(value: &Value) -> Option<Map<String, Value>> {
    match value {
        Value::Object(map) => Some(map.clone()),
        Value::String(text) if text.is_empty() => Some(Map::new()),
        Value::Array(items) => {
            let mut map = Map::new();
            for item in items {
                let (key, value) = match item {
                    Value::Array(pair) if pair.len() == 2 => (pair[0].clone(), pair[1].clone()),
                    Value::String(text) if text.chars().count() == 2 => {
                        let mut chars = text.chars();
                        let first = chars.next()?.to_string();
                        let second = chars.next()?.to_string();
                        (Value::String(first), Value::String(second))
                    }
                    Value::Object(pair) if pair.len() == 2 => {
                        let mut keys = pair.keys();
                        let first = keys.next()?.clone();
                        let second = keys.next()?.clone();
                        (Value::String(first), Value::String(second))
                    }
                    _ => return None,
                };
                let Value::String(key) = key else {
                    return None;
                };
                map.insert(key, value);
            }
            Some(map)
        }
        _ => None,
    }
}

/// `tuple(value)`: the characters of a string, the items of a list, the keys of an object;
/// `None` (a `TypeError`) for a number, a boolean or null.
pub fn py_tuple(value: &Value) -> Option<Vec<Value>> {
    match value {
        Value::String(text) => Some(text.chars().map(|c| Value::String(c.to_string())).collect()),
        Value::Array(items) => Some(items.clone()),
        Value::Object(map) => Some(map.keys().cloned().map(Value::String).collect()),
        _ => None,
    }
}

/// Python truthiness of a JSON value.
pub fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(flag) => *flag,
        Value::Number(number) => number.as_f64().is_some_and(|n| n != 0.0),
        Value::String(text) => !text.is_empty(),
        Value::Array(items) => !items.is_empty(),
        Value::Object(map) => !map.is_empty(),
    }
}

// ------------------------------------------------------------------------------------------
// Hex text
// ------------------------------------------------------------------------------------------

/// `bytes.fromhex(text)`: pairs of hex digits, ASCII whitespace (`isspace` in C: space, `\t`,
/// `\n`, `\v`, `\f`, `\r`) allowed between pairs, never inside one; any other character is an
/// error.
pub fn bytes_fromhex(text: &str) -> Option<Vec<u8>> {
    let bytes = text.as_bytes();
    if !text.is_ascii() {
        return None;
    }
    let mut out = Vec::with_capacity(bytes.len() / 2);
    let mut index = 0;
    while index < bytes.len() {
        if matches!(
            bytes[index],
            b' ' | b'\t' | b'\n' | b'\x0b' | b'\x0c' | b'\r'
        ) {
            index += 1;
            continue;
        }
        let high = hex_value(bytes[index])?;
        let low = hex_value(*bytes.get(index + 1)?)?;
        out.push(high << 4 | low);
        index += 2;
    }
    Some(out)
}

fn hex_value(byte: u8) -> Option<u8> {
    char::from(byte)
        .to_digit(16)
        .and_then(|digit| u8::try_from(digit).ok())
}

// ------------------------------------------------------------------------------------------
// DRM parameters
// ------------------------------------------------------------------------------------------

/// An environment lookup (`os.environ.get`-like) for [`drm_parameters`]' `env`.
pub type EnvLookup<'a> = &'a dyn Fn(&str) -> Option<String>;

/// `drm_parameters(data, env=env)`: the key material (from `env` when given, otherwise from the
/// `kid` / `key` / `iv` members of `data`) and the three signalings of `data`, validated by
/// `validate_signaling`. A raw `WidevinePsshData` payload (not a complete box) is boxed with the
/// Widevine system id. Every failure is `drm-parameters-invalid`.
pub fn drm_parameters(
    data: &Value,
    env: Option<EnvLookup<'_>>,
) -> Result<(KeyMaterial, DrmSignaling), HlsCliError> {
    parse_drm_parameters(data, env).ok_or(HlsCliError(DRM_PARAMETERS_INVALID))
}

fn parse_drm_parameters(
    data: &Value,
    env: Option<EnvLookup<'_>>,
) -> Option<(KeyMaterial, DrmSignaling)> {
    let Value::Object(data) = data else {
        return None; // `data.get` of a non-dict is an AttributeError
    };
    let keys = key_material(data, env)?;
    let Value::String(value) = data.get("widevine_pssh")? else {
        return None;
    };
    if value.chars().count() > 131_072 {
        return None;
    }
    let value = py_strip(value);
    let hex_form = !value.is_empty()
        && value.len().is_multiple_of(2)
        && value.bytes().all(|byte| byte.is_ascii_hexdigit());
    let mut pssh = if hex_form {
        bytes_fromhex(value)?
    } else {
        b64decode_validate(value)?
    };
    if !is_pssh_box(&pssh) {
        // Encoding APIs also supply WidevinePsshData rather than a complete ISO box: its bytes
        // are kept and boxed; signaling validation still checks every embedded KID.
        if pssh.is_empty()
            || pssh.len() > 65_504
            || hls_pipeline::protobuf_kids(&pssh).ok()?.is_empty()
        {
            return None;
        }
        pssh = playready::build_pssh_box(&WIDEVINE_SYSTEM_ID, &pssh).ok()?;
    }
    let text = |name: &str| data.get(name).cloned();
    let signaling = (text("playready_la_url")?, text("fairplay_uri")?);
    let (Value::String(playready_la_url), Value::String(fairplay_uri)) = signaling else {
        return None; // `validate_signaling` refuses a non-string URI
    };
    let signaling = DrmSignaling {
        widevine_pssh: pssh,
        playready_la_url,
        fairplay_uri,
    };
    validate_signaling(&signaling, &keys).ok()?;
    Some((keys, signaling))
}

/// `load_key_material(env if env is not None else {"DRM_KID": data.get("kid", ""), "DRM_KEY":
/// data.get("key", ""), "DRM_IV": data.get("iv", "")})` through [`config::load_key_material`].
/// From `data`, a member Python's `not source.get(name)` calls missing (absent or falsy) is left
/// out of the mapping, and a truthy member that is not a string is refused (`bytes.fromhex` of it
/// is a `TypeError`). The mapping's copies of the key and the IV are wiped once read.
fn key_material(data: &Map<String, Value>, env: Option<EnvLookup<'_>>) -> Option<KeyMaterial> {
    let mut source: HashMap<String, String> = HashMap::new();
    let mut untyped = false;
    for (name, member) in [("DRM_KID", "kid"), ("DRM_KEY", "key"), ("DRM_IV", "iv")] {
        let value = match env {
            Some(lookup) => lookup(name),
            None => match data.get(member) {
                Some(Value::String(text)) => Some(text.clone()),
                Some(other) => {
                    untyped |= truthy(other);
                    None
                }
                None => None,
            },
        };
        if let Some(value) = value {
            source.insert(name.to_string(), value);
        }
    }
    let keys = if untyped {
        None
    } else {
        config::load_key_material(Some(&source)).ok()
    };
    for value in source.values_mut() {
        value.zeroize();
    }
    keys
}

/// `pssh[4:8] == b"pssh" and int.from_bytes(pssh[:4], "big") == len(pssh)`.
fn is_pssh_box(data: &[u8]) -> bool {
    let size = data
        .iter()
        .take(4)
        .fold(0u64, |acc, byte| acc << 8 | u64::from(*byte));
    data.get(4..8) == Some(b"pssh".as_slice()) && usize::try_from(size).ok() == Some(data.len())
}

/// The token `validate_signaling` raises (`playready-invalid` stands for the `PlayReadyError`
/// that `build_playready_v43` raises in Python).
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct SignalingError(pub &'static str);

/// The tokens of [`hls_pipeline::validate_signaling`] other than a `PlayReadyError`'s message.
const SIGNALING_TOKENS: [&str; 5] = [
    "widevine-invalid-pssh",
    "widevine-invalid-payload",
    "widevine-kid-mismatch",
    "playready-invalid-uri",
    "fairplay-invalid-uri",
];

/// `hls_pipeline.validate_signaling(signaling, keys)`: a well-formed Widevine `pssh` box (v0 or
/// v1) whose KIDs (box and protobuf) are all the content KID, an `http(s)` `LA_URL` and an `skd`
/// `FairPlay` URI (no control character, no `"`, a host, no user info, as [`urlsplit`] splits
/// them), then the `PlayReady` objects for the KID and the `LA_URL`
/// (`playready::build_playready_v43`) — [`hls_pipeline::validate_signaling`] itself, its failure
/// as one of its tokens (`playready-invalid` for the `PlayReadyError`).
pub fn validate_signaling(
    signaling: &DrmSignaling,
    keys: &KeyMaterial,
) -> Result<PlayReadyObjects, SignalingError> {
    hls_pipeline::validate_signaling(signaling, keys).map_err(|error| {
        let message = error.to_string();
        SignalingError(
            SIGNALING_TOKENS
                .into_iter()
                .find(|token| *token == message)
                .unwrap_or("playready-invalid"),
        )
    })
}

/// `base64.b64decode(text, validate=True)` of Python 3.11 (`binascii.a2b_base64` in strict
/// mode): the standard alphabet only, no leading `=`, nothing after the padding, no data
/// character after a `=`; the unused bits of the last quantum are not checked.
pub fn b64decode_validate(text: &str) -> Option<Vec<u8>> {
    if !text.is_ascii() {
        return None; // `s.encode('ascii')` fails
    }
    let data = text.as_bytes();
    if data.first() == Some(&b'=') {
        return None;
    }
    let mut out = Vec::with_capacity(data.len() / 4 * 3 + 3);
    let (mut quad_pos, mut left, mut pads, mut padding_started) = (0u8, 0u8, 0u8, false);
    for (index, byte) in data.iter().enumerate() {
        if *byte == b'=' {
            padding_started = true;
            if quad_pos >= 2 {
                pads += 1;
                if quad_pos + pads >= 4 {
                    return (index + 1 == data.len()).then_some(out);
                }
            }
            continue;
        }
        let value = b64_value(*byte)?;
        if padding_started {
            return None;
        }
        pads = 0;
        match quad_pos {
            0 => {
                quad_pos = 1;
                left = value;
            }
            1 => {
                quad_pos = 2;
                out.push(left << 2 | value >> 4);
                left = value & 0x0f;
            }
            2 => {
                quad_pos = 3;
                out.push(left << 4 | value >> 2);
                left = value & 0x03;
            }
            _ => {
                quad_pos = 0;
                out.push(left << 6 | value);
                left = 0;
            }
        }
    }
    (quad_pos == 0).then_some(out)
}

fn b64_value(byte: u8) -> Option<u8> {
    match byte {
        b'A'..=b'Z' => Some(byte - b'A'),
        b'a'..=b'z' => Some(byte - b'a' + 26),
        b'0'..=b'9' => Some(byte - b'0' + 52),
        b'+' => Some(62),
        b'/' => Some(63),
        _ => None,
    }
}

/// `str.strip`: Unicode whitespace plus U+001C–U+001F, which Python's `isspace` counts and
/// Rust's `char::is_whitespace` does not.
pub fn py_strip(text: &str) -> &str {
    text.trim_matches(py_isspace)
}

/// `str.isspace` for one character.
pub fn py_isspace(c: char) -> bool {
    c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c)
}

// ------------------------------------------------------------------------------------------
// The commands: `analyze-hls` and `package-hls`
// ------------------------------------------------------------------------------------------

/// `analyze-hls`'s help line (`register_hls_commands`).
pub const ANALYZE_ABOUT: &str = "Legacy: save and inspect a clear on-demand HLS snapshot";
/// `package-hls`'s help line.
pub const PACKAGE_ABOUT: &str = "Legacy snapshot packaging for DASH and HLS";
/// `--max-workers`' help text.
pub const MAX_WORKERS_HELP: &str = "segments packaged at once across all tracks, 1-64 (default: \
                                    the CPUs the process may run on, at most 32; 1 = one at a \
                                    time on the caller's thread)";

/// `analyze-hls`'s arguments. The members carry plain comments: clap prints a doc comment as
/// help text, and Python gives these options none.
#[derive(Args, Clone, Debug)]
pub struct AnalyzeArgs {
    // `--source-config` (`type=Path`): the private source configuration (`url`, `policy`).
    #[arg(
        long = "source-config",
        required = true,
        help = "Owner-only JSON file with the playlist URL and allowed input locations"
    )]
    pub source_config: PathBuf,
    // `--asset-id`.
    #[arg(
        long = "asset-id",
        required = true,
        help = "Name for this saved media snapshot"
    )]
    pub asset_id: String,
    // `--output` (`type=Path`): the output root.
    #[arg(
        long,
        required = true,
        help = "Output root; the snapshot is saved under <asset-id>/clear/source/"
    )]
    pub output: PathBuf,
}

/// `package-hls`'s arguments. The key material is never a flag: it comes from the
/// environment (`DRM_KID` / `DRM_KEY` / `DRM_IV`), the signaling from `--drm-config`.
#[derive(Args, Clone, Debug)]
pub struct PackageArgs {
    // `--analysis` (`type=Path`): the snapshot's `analysis.json`.
    #[arg(
        long,
        required = true,
        help = "analysis.json from a completed analyze-hls command"
    )]
    pub analysis: PathBuf,
    // `--drm-config` (`type=Path`): the private DRM signaling.
    #[arg(
        long = "drm-config",
        required = true,
        help = "Owner-only JSON file with Widevine, PlayReady, and FairPlay signaling"
    )]
    pub drm_config: PathBuf,
    // `--selection` (`type=Path`): the private audio selection.
    #[arg(long, help = "Owner-only JSON file selecting the audio source track")]
    pub selection: Option<PathBuf>,
    // `--packager-bin` (`type=Path`).
    #[arg(
        long = "packager-bin",
        help = "Shaka Packager executable path (default: packager on PATH)"
    )]
    pub packager_bin: Option<PathBuf>,
    // `--max-workers` (`type=int`; the range is `package_hls`'s check).
    #[arg(long = "max-workers", allow_negative_numbers = true, help = MAX_WORKERS_HELP)]
    pub max_workers: Option<i64>,
}

/// `{"stage": …, "level": …, "message": …}`: the command-level records carry no `ts`.
fn outcome_record(stage: &str, level: &str, message: &str) -> Record {
    let mut record = Record::new();
    record.insert("stage".into(), Value::from(stage));
    record.insert("level".into(), Value::from(level));
    record.insert("message".into(), Value::from(message));
    record
}

/// One writer (`_emit`: one lock, one scrubbed line per record) for the command's own records,
/// its progress records and every tool record of its runner; the exit code is 1 when
/// stdout failed, as Python's `print` raising ends the process.
fn with_writer<W, F>(out: W, command: F) -> u8
where
    W: Write + Send + 'static,
    F: FnOnce(&Arc<Secrets>, &Arc<JsonLinesWriter<W>>, Arc<ToolRunner>) -> u8,
{
    let secrets = Arc::new(Secrets::new());
    let writer = Arc::new(JsonLinesWriter::new(out, Arc::clone(&secrets)));
    let runner = Arc::new(ToolRunner::new(
        Arc::clone(&secrets),
        Arc::clone(&writer) as Arc<dyn Sink>,
    ));
    let code = command(&secrets, &writer, runner);
    if writer.failed() { 1 } else { code }
}

/// How `_analyze` ends when it does not return an analysis.
enum AnalyzeFailure {
    /// `SourceError`: its token, exit 1.
    Source(String),
    /// `ValueError`, `TypeError`, `OSError`: `source-config-invalid`, exit 2.
    Config,
    /// Any other exception: `source-infrastructure-error`, exit 2.
    Infrastructure,
}

/// `analyze_command(args)`: the private source config read, the analysis run with every record
/// on `out`, then `analysis-complete` (exit 0) — or the failure's record: a `SourceError`'s
/// token (exit 1), `source-config-invalid` or `source-infrastructure-error` (exit 2).
pub fn analyze_command<W: Write + Send + 'static>(args: &AnalyzeArgs, out: W) -> u8 {
    with_writer(out, |_, writer, runner| {
        let outcome = panic::catch_unwind(AssertUnwindSafe(|| {
            run_analysis(args, writer.as_ref(), &runner)
        }));
        let (message, code) = match outcome {
            Ok(Ok(analysis)) => {
                let mut record = outcome_record("analysis", "info", "analysis-complete");
                record.insert("analysis_id".into(), Value::from(analysis.analysis_id));
                record.insert("track_count".into(), Value::from(analysis.tracks.len()));
                writer.emit(&record);
                return u8::from(!analysis.admitted);
            }
            Ok(Err(AnalyzeFailure::Source(token))) => (token, 1),
            Ok(Err(AnalyzeFailure::Config)) => ("source-config-invalid".to_string(), 2),
            // `except Exception` (a panic is this implementation's unexpected exception)
            Ok(Err(AnalyzeFailure::Infrastructure)) | Err(_) => {
                ("source-infrastructure-error".to_string(), 2)
            }
        };
        writer.emit(&outcome_record("analysis", "error", &message));
        code
    })
}

/// `_analyze`'s body: `private_json`, the `url` check, `fetch_policy(config.get("policy",
/// {}))` (any failure a plain `ValueError`), then `analyze_hls`.
fn run_analysis(
    args: &AnalyzeArgs,
    emit: &dyn Sink,
    runner: &ToolRunner,
) -> Result<SourceAnalysis, AnalyzeFailure> {
    let config = private_json(&args.source_config).map_err(|_| AnalyzeFailure::Config)?;
    let Some(Value::String(url)) = config.get("url") else {
        return Err(AnalyzeFailure::Config);
    };
    let empty = Value::Object(Map::new());
    let policy =
        fetch_policy(config.get("policy").unwrap_or(&empty)).map_err(|_| AnalyzeFailure::Config)?;
    source::analyze_hls(
        &SourceRequest::new(url.clone()),
        &args.asset_id,
        &args.output,
        &policy,
        Some(emit),
        runner,
    )
    .map_err(|failure| match failure {
        Failure::Source(error) => AnalyzeFailure::Source(error.token().to_string()),
        Failure::Escaped(escaped) if escaped.is_config_class() => AnalyzeFailure::Config,
        Failure::Escaped(_) | Failure::Cancelled => AnalyzeFailure::Infrastructure,
    })
}

/// `package_command(args)`: the analysis read back and verified, the DRM parameters (the key
/// material from `env`), the job's secrets registered — the key, the IV, the `LA_URL` and the
/// `FairPlay` URI are scrubbed from every record from then on — the selection, then
/// `package_hls` with every record on `out` and `packaging-complete` (exit 0) or
/// `packaging-failed` (exit 1); a `SourceError`'s token (exit 1), `package-config-invalid` or
/// `package-infrastructure-error` (exit 2) otherwise.
pub fn package_command<W: Write + Send + 'static>(
    args: &PackageArgs,
    env: EnvLookup<'_>,
    out: W,
) -> u8 {
    with_writer(out, |secrets, writer, runner| {
        let outcome = panic::catch_unwind(AssertUnwindSafe(|| {
            run_packaging(args, env, secrets, writer.as_ref(), runner)
        }));
        let (message, code) = match outcome {
            Ok(Ok(report)) => {
                let (level, message) = if report.success {
                    ("info", "packaging-complete")
                } else {
                    ("error", "packaging-failed")
                };
                writer.emit(&outcome_record("report", level, message));
                return u8::from(!report.success);
            }
            Ok(Err(PipelineError::Source(token))) => (token, 1),
            Ok(Err(PipelineError::Value(_))) => ("package-config-invalid".to_string(), 2),
            Ok(Err(PipelineError::Infrastructure(_))) | Err(_) => {
                ("package-infrastructure-error".to_string(), 2)
            }
        };
        writer.emit(&outcome_record("package-hls", "error", &message));
        code
    })
}

/// `_package`'s body.
fn run_packaging(
    args: &PackageArgs,
    env: EnvLookup<'_>,
    secrets: &Secrets,
    emit: &dyn Sink,
    runner: Arc<ToolRunner>,
) -> Result<PackageHlsReport, PipelineError> {
    let config_invalid = |token: &str| PipelineError::Value(token.to_string());
    let analysis = source::read_analysis(&args.analysis).map_err(hls_pipeline::source_failure)?;
    let drm = private_json(&args.drm_config).map_err(|error| config_invalid(error.token()))?;
    let (keys, signaling) = drm_parameters(&Value::Object(drm), Some(env))
        .map_err(|error| config_invalid(error.token()))?;
    secrets.register([
        keys.key_hex(),
        keys.iv_hex(),
        signaling.playready_la_url.as_str(),
        signaling.fairplay_uri.as_str(),
    ]);
    let choices = match &args.selection {
        Some(path) => private_json(path).map_err(|error| config_invalid(error.token()))?,
        None => Map::new(),
    };
    // `choices.get(name, default)`: a member present with any value is that value
    let choice = |name: &str, default: &str| {
        choices
            .get(name)
            .cloned()
            .unwrap_or_else(|| Value::from(default))
    };
    let selection = PackagingSelection {
        analysis_id: choice("analysis_id", &analysis.analysis_id),
        snapshot_digest: choice("snapshot_digest", &analysis.snapshot_digest),
        audio_source_id: choices
            .get("audio_source_id")
            .cloned()
            .unwrap_or(Value::Null),
    };
    // `str(args.packager_bin or "packager")`: a `Path` prints normalised
    let packager_bin = args
        .packager_bin
        .as_deref()
        .map_or_else(|| "packager".to_string(), pycompat::path_str);
    let deps = hls_pipeline_deps::Ports::new(runner);
    let on_log = |record: Record| emit.emit(&record);
    hls_pipeline::package_hls(
        &analysis,
        &keys,
        &signaling,
        Some(&selection),
        Some(&on_log),
        &packager_bin,
        args.max_workers,
        &deps,
    )
}

// ------------------------------------------------------------------------------------------
// urllib.parse.urlsplit (CPython 3.11)
// ------------------------------------------------------------------------------------------

/// `urllib.parse.SplitResult`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SplitResult {
    /// Lower-cased scheme, or empty.
    pub scheme: String,
    /// The authority as written, or empty.
    pub netloc: String,
    /// The path.
    pub path: String,
    /// The query, without `?`.
    pub query: String,
    /// The fragment, without `#`.
    pub fragment: String,
}

impl SplitResult {
    fn userinfo(&self) -> Option<&str> {
        self.netloc.rsplit_once('@').map(|(info, _)| info)
    }

    /// `.username`: `None` without an `@`, possibly empty.
    pub fn username(&self) -> Option<&str> {
        self.userinfo()
            .map(|info| info.split_once(':').map_or(info, |(name, _)| name))
    }

    /// `.password`: `None` without an `@` or without a `:` in the user info.
    pub fn password(&self) -> Option<&str> {
        self.userinfo()
            .and_then(|info| info.split_once(':').map(|(_, word)| word))
    }
}

/// `urlsplit`'s `ValueError` (an unbalanced or invalid bracketed host, or a netloc that NFKC
/// normalisation would turn into a delimiter).
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("invalid URL")]
pub struct UrlError;

/// `urllib.parse.urlsplit(url)` of Python 3.11: leading C0 controls and spaces removed, tabs
/// and line breaks removed, then scheme, netloc, path, query and fragment.
pub fn urlsplit(url: &str) -> Result<SplitResult, UrlError> {
    let url = url.trim_start_matches(|c: char| c <= ' ');
    let mut url: String = url
        .chars()
        .filter(|c| !matches!(c, '\t' | '\r' | '\n'))
        .collect();
    let mut result = SplitResult::default();
    if let Some(colon) = url.find(':') {
        let candidate = &url[..colon];
        let starts_alpha = candidate
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic());
        if colon > 0
            && starts_alpha
            && candidate
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
        {
            result.scheme = candidate.to_ascii_lowercase();
            url = url[colon + 1..].to_string();
        }
    }
    if url.starts_with("//") {
        let rest = &url[2..];
        let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
        result.netloc = rest[..end].to_string();
        url = rest[end..].to_string();
        let (open, close) = (result.netloc.contains('['), result.netloc.contains(']'));
        if open != close {
            return Err(UrlError);
        }
        if open && close {
            check_bracketed_netloc(&result.netloc)?;
        }
    }
    if let Some((before, fragment)) = url.split_once('#') {
        result.fragment = fragment.to_string();
        url = before.to_string();
    }
    if let Some((before, query)) = url.split_once('?') {
        result.query = query.to_string();
        url = before.to_string();
    }
    check_netloc(&result.netloc)?;
    result.path = url;
    Ok(result)
}

/// The non-ASCII code points whose NFKC form contains `/`, `?`, `#`, `@` or `:` (Unicode 14.0,
/// Python 3.11's `unicodedata`): `_checknetloc` refuses a netloc that contains one.
const NFKC_DELIMITERS: [u32; 19] = [
    0x2047, 0x2048, 0x2049, 0x2100, 0x2101, 0x2105, 0x2106, 0x2A74, 0xFE13, 0xFE16, 0xFE55, 0xFE56,
    0xFE5F, 0xFE6B, 0xFF03, 0xFF0F, 0xFF1A, 0xFF1F, 0xFF20,
];

fn check_netloc(netloc: &str) -> Result<(), UrlError> {
    if netloc
        .chars()
        .any(|c| NFKC_DELIMITERS.contains(&u32::from(c)))
    {
        return Err(UrlError);
    }
    Ok(())
}

fn check_bracketed_netloc(netloc: &str) -> Result<(), UrlError> {
    let host_and_port = netloc.rsplit_once('@').map_or(netloc, |(_, host)| host);
    let hostname = match host_and_port.split_once('[') {
        Some((before, bracketed)) => {
            if !before.is_empty() {
                return Err(UrlError);
            }
            let (hostname, port) = bracketed.split_once(']').unwrap_or((bracketed, ""));
            if !port.is_empty() && !port.starts_with(':') {
                return Err(UrlError);
            }
            hostname
        }
        None => host_and_port
            .split_once(':')
            .map_or(host_and_port, |(host, _)| host),
    };
    check_bracketed_host(hostname)
}

/// `_check_bracketed_host`: an `IPvFuture` literal, or an IPv6 address (`ipaddress.ip_address`);
/// an IPv4 address in brackets is refused. A zone id (`%…`) is accepted as Python accepts it.
fn check_bracketed_host(hostname: &str) -> Result<(), UrlError> {
    if let Some(rest) = hostname.strip_prefix('v') {
        let (version, address) = rest.split_once('.').ok_or(UrlError)?;
        let valid = !version.is_empty()
            && version.bytes().all(|byte| byte.is_ascii_hexdigit())
            && !address.is_empty()
            && !address.contains('\n');
        return if valid { Ok(()) } else { Err(UrlError) };
    }
    if hostname.parse::<Ipv4Addr>().is_ok() {
        return Err(UrlError);
    }
    let (address, zone) = match hostname.split_once('%') {
        Some((address, zone)) => (address, Some(zone)),
        None => (hostname, None),
    };
    if zone.is_some_and(|zone| zone.is_empty() || zone.contains('%')) {
        return Err(UrlError);
    }
    address
        .parse::<Ipv6Addr>()
        .map(|_| ())
        .map_err(|_| UrlError)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const KID: &str = "000102030405060708090a0b0c0d0e0f";
    const KEY: &str = "10101010101010101010101010101010";
    const IV: &str = "20202020202020202020202020202020";
    const LA_URL: &str = "https://license.example.invalid/rightsmanager.asmx";
    const FAIRPLAY: &str = "skd://sample-content-id";

    fn widevine_payload(kid: &str) -> Vec<u8> {
        let mut payload = vec![0x12, 0x10];
        payload.extend(bytes_fromhex(kid).unwrap());
        payload
    }

    fn widevine_box(kid: &str) -> Vec<u8> {
        playready::build_pssh_box(&WIDEVINE_SYSTEM_ID, &widevine_payload(kid)).unwrap()
    }

    fn sample() -> Value {
        json!({"kid": KID, "key": KEY, "iv": IV, "widevine_pssh": base64_encode(&widevine_box(KID)),
               "playready_la_url": LA_URL, "fairplay_uri": FAIRPLAY})
    }

    fn base64_encode(data: &[u8]) -> String {
        use base64::Engine as _;
        base64::engine::general_purpose::STANDARD.encode(data)
    }

    #[test]
    fn a_complete_box_and_a_raw_payload_are_both_accepted() {
        let (keys, signaling) = drm_parameters(&sample(), None).unwrap();
        assert_eq!(keys.key_id_hex(), KID);
        assert_eq!(signaling.playready_la_url, LA_URL);
        let mut raw = sample();
        raw["widevine_pssh"] = json!(base64_encode(&widevine_payload(KID)));
        let (_, boxed) = drm_parameters(&raw, None).unwrap();
        assert_eq!(boxed.widevine_pssh, widevine_box(KID));
    }

    #[test]
    fn a_foreign_kid_an_empty_payload_and_a_missing_field_are_drm_parameters_invalid() {
        let mut foreign = sample();
        foreign["widevine_pssh"] = json!(base64_encode(&widevine_payload(
            "0f0e0d0c0b0a09080706050403020100"
        )));
        for broken in [foreign, json!({}), json!([1]), json!(null)] {
            assert_eq!(
                drm_parameters(&broken, None).unwrap_err().token(),
                DRM_PARAMETERS_INVALID
            );
        }
        for field in [
            "kid",
            "key",
            "iv",
            "widevine_pssh",
            "playready_la_url",
            "fairplay_uri",
        ] {
            let mut data = sample();
            data.as_object_mut().unwrap().remove(field);
            assert!(drm_parameters(&data, None).is_err(), "{field}");
        }
        for value in ["AA==", "bm90LXByb3RvYnVm", "CAESAQ=="] {
            let mut data = sample();
            data["widevine_pssh"] = json!(value);
            assert!(drm_parameters(&data, None).is_err(), "{value}");
        }
    }

    #[test]
    fn the_uris_are_checked_like_validate_signaling() {
        for (field, value) in [
            ("playready_la_url", "ftp://license.example.invalid/x"),
            ("playready_la_url", "https://user@license.example.invalid/x"),
            ("playready_la_url", "https:///no-host"),
            ("playready_la_url", "https://license.example.invalid/\"x"),
            ("fairplay_uri", "https://sample-content-id"),
            ("fairplay_uri", "skd://[::1"),
            // CPython 3.11.15's urlsplit raises on these three (`_checknetloc`'s NFKC test and
            // `_check_bracketed_netloc`); the private splitter of `hls_pipeline` accepts them,
            // one reason `validate_signaling` is not delegated there yet.
            ("fairplay_uri", "skd://a\u{ff0f}b"),
            ("fairplay_uri", "skd://x[::1]"),
            ("fairplay_uri", "skd://[::1]x"),
        ] {
            let mut data = sample();
            data[field] = json!(value);
            assert!(drm_parameters(&data, None).is_err(), "{field}={value}");
        }
        // The PRO record's 16-bit length bounds the LA_URL: 32 532 UTF-16 units fit, 32 533 do not
        // (the boundary `playready.build_playready_v43` has).
        let prefix = "https://license.example.invalid/";
        for (units, fits) in [(32_532, true), (32_533, false)] {
            let mut long = sample();
            long["playready_la_url"] =
                json!(format!("{prefix}{}", "a".repeat(units - prefix.len())));
            assert_eq!(drm_parameters(&long, None).is_ok(), fits, "{units}");
        }
    }

    #[test]
    fn the_key_material_comes_from_env_when_given() {
        let env = |name: &str| match name {
            "DRM_KID" => Some(KID.to_string()),
            "DRM_KEY" => Some(KEY.to_string()),
            "DRM_IV" => Some(IV.to_string()),
            _ => None,
        };
        let mut data = sample();
        data["kid"] = json!("not hex");
        let (keys, _) = drm_parameters(&data, Some(&env)).unwrap();
        assert_eq!((keys.key_id_hex(), keys.iv_hex()), (KID, IV));
        let without_iv = |name: &str| (name != "DRM_IV").then(|| env(name)).flatten();
        assert!(drm_parameters(&sample(), Some(&without_iv)).is_err());
        let empty_iv = |name: &str| {
            if name == "DRM_IV" {
                Some(String::new())
            } else {
                env(name)
            }
        };
        assert!(drm_parameters(&sample(), Some(&empty_iv)).is_err());
    }

    #[test]
    fn the_data_members_are_read_as_load_key_material_reads_its_mapping() {
        // `not source.get(name)` is "missing" (an absent or falsy member); a truthy member that is
        // not a string reaches `bytes.fromhex` (a TypeError): both are drm-parameters-invalid.
        for value in [
            json!(""),
            json!(0),
            json!(false),
            json!(null),
            json!(5),
            json!([KID]),
        ] {
            let mut data = sample();
            data["iv"] = value.clone();
            assert!(drm_parameters(&data, None).is_err(), "{value}");
        }
        // A KID with whitespace between its pairs is valid key material (`bytes.fromhex`) and
        // matches the Widevine KID, but it is not a UUID: `build_playready_v43` refuses it.
        let spaced = KID
            .as_bytes()
            .chunks(2)
            .map(|pair| std::str::from_utf8(pair).unwrap())
            .collect::<Vec<_>>()
            .join(" ");
        assert!(KeyMaterial::new(spaced.as_str(), KEY, IV).is_ok());
        let mut data = sample();
        data["kid"] = json!(spaced);
        assert_eq!(
            drm_parameters(&data, None).unwrap_err().token(),
            DRM_PARAMETERS_INVALID
        );
    }

    #[test]
    fn fromhex_and_strict_base64_follow_python() {
        assert_eq!(bytes_fromhex(" 00 01\x0b"), Some(vec![0, 1]));
        assert_eq!(bytes_fromhex("0 001"), None);
        assert_eq!(bytes_fromhex("00\x1c01"), None);
        assert_eq!(bytes_fromhex("abc"), None);
        assert_eq!(b64decode_validate("AB=="), Some(vec![0]));
        assert_eq!(b64decode_validate("AAAA===="), Some(vec![0, 0, 0]));
        assert_eq!(b64decode_validate(""), Some(vec![]));
        for bad in [
            "A===",
            "AA==A",
            "AA=A",
            "=AA",
            "AAA",
            "AAAA AAAA",
            "AA=",
            "AAA==",
        ] {
            assert_eq!(b64decode_validate(bad), None, "{bad}");
        }
    }

    #[test]
    fn fetch_policy_validates_like_the_dataclass() {
        let policy = fetch_policy(
            &json!({"allowed_origins": ["https://a.example"], "connections": 3,
                                          "probe_workers": 2, "allow_http": "yes"}),
        )
        .unwrap();
        assert!(policy.allowed_origins == ["https://a.example"] && policy.allow_http);
        assert_eq!((policy.connections, policy.probe_workers), (3, Some(2)));
        for bad in [
            json!({"unknown": 1}),
            json!({"connections": 65}),
            json!({"attempts": 6}),
            json!({"timeout": true}),
            json!({"timeout": 0}),
            json!({"max_playlists": 1.5}),
            json!({"probe_workers": 0}),
            json!({"allowed_origins": 5}),
            json!({"headers_by_origin": {"https://b.example": {"X": "y"}}}),
            json!({"allowed_origins": ["https://a"], "headers_by_origin": {"https://a": {"Host": "y"}}}),
            json!({"allowed_origins": ["https://a"], "headers_by_origin": {"https://a": {"X": "a\r\nb"}}}),
            json!("ab"),
            json!(5),
        ] {
            assert_eq!(
                fetch_policy(&bad).unwrap_err().token(),
                INVALID_FETCH_POLICY,
                "{bad}"
            );
        }
        // `dict(data)` as CPython 3.11.15 builds it: pairs, an empty string (`{}`), and a
        // two-key object standing for the pair of its keys.
        let pairs = fetch_policy(&json!([["connections", 2]])).unwrap();
        assert_eq!(pairs.connections, 2, "dict() of pairs");
        assert!(fetch_policy(&json!("")).is_ok(), "dict('') is {{}}");
        let keys = fetch_policy(&json!([{"allow_http": 0, "z": 0}])).unwrap();
        assert!(
            keys.allow_http,
            "the pair ('allow_http', 'z'): a truthy 'z'"
        );
        let origins = fetch_policy(&json!({"allowed_origins": "ab"}))
            .unwrap()
            .allowed_origins;
        assert_eq!(origins, ["a", "b"], "tuple() of a string is its characters");
    }

    #[test]
    fn urlsplit_matches_cpython() {
        let parsed = urlsplit(" HTTPS://user:pw@Host:8443/a/b;p?q=1#f").unwrap();
        assert_eq!(parsed.scheme, "https");
        assert_eq!(parsed.netloc, "user:pw@Host:8443");
        assert_eq!(
            (
                parsed.path.as_str(),
                parsed.query.as_str(),
                parsed.fragment.as_str()
            ),
            ("/a/b;p", "q=1", "f")
        );
        assert_eq!(
            (parsed.username(), parsed.password()),
            (Some("user"), Some("pw"))
        );
        assert_eq!(urlsplit("https://@host/").unwrap().username(), Some(""));
        assert_eq!(
            urlsplit("skd://sample-content-id").unwrap().netloc,
            "sample-content-id"
        );
        assert!(urlsplit("https://[::1]:8080/").is_ok());
        assert!(urlsplit("https://[127.0.0.1]/").is_err());
        assert!(urlsplit("https://[::1/").is_err());
        assert!(urlsplit("https://a\u{ff0f}b/").is_err());
        assert_eq!(
            urlsplit("1http://x").unwrap().scheme,
            "",
            "a scheme starts with a letter"
        );
        assert_eq!(
            urlsplit("ht\ttp://x/").unwrap().scheme,
            "http",
            "tabs are removed first"
        );
    }

    #[cfg(unix)]
    #[test]
    fn private_json_requires_a_private_object() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        std::fs::write(&path, br#"{"a": 1}"#).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(private_json(&path).unwrap()["a"], json!(1));
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o640)).unwrap();
        assert_eq!(
            private_json(&path).unwrap_err().token(),
            PRIVATE_CONFIG_INVALID
        );
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        std::fs::write(&path, b"[1]").unwrap();
        assert!(private_json(&path).is_err());
        assert!(private_json(&dir.path().join("missing.json")).is_err());
    }

    #[test]
    fn the_pages_sample_values_are_a_valid_drm_configuration() {
        // The console's
        // synthetic values the console's "fill sample values" shows must pass as a submission.
        let console = std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../drmr-server/static/js/console.js"),
        )
        .unwrap();
        let start = console.find("SAMPLE_DRM = {").unwrap() + "SAMPLE_DRM = {".len();
        let block = &console[start..start + console[start..].find("};").unwrap()];
        let pattern = regex::Regex::new(r"(\w+): '([^']*)'").unwrap();
        let sample: Map<String, Value> = pattern
            .captures_iter(block)
            .map(|c| (c[1].to_string(), Value::String(c[2].to_string())))
            .collect();
        assert_eq!(sample.len(), 6);
        let (keys, signaling) = drm_parameters(&Value::Object(sample.clone()), None).unwrap();
        assert_eq!(Some(keys.key_id_hex()), sample["kid"].as_str());
        assert_eq!(
            Some(signaling.fairplay_uri.as_str()),
            sample["fairplay_uri"].as_str()
        );
    }
}
