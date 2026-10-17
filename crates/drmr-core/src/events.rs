//! Structured JSON Lines records and event sinks.
//!
//! Records preserve field insertion order. The writer scrubs every serialized line
//! against the job's registered secrets before writing it.

use crate::canonical::Dumps;
use crate::checks::CheckId;
use crate::toolrun::Secrets;
use serde_json::{Map, Value};
use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

/// One job-log record.
pub type Record = Map<String, Value>;

/// Where records go. Implementations serialize their own writes.
pub trait Sink: Send + Sync {
    /// Receive one record.
    fn emit(&self, record: &Record);
}

/// A sink that drops every record (the implementations run without a sink in the same way).
#[derive(Clone, Copy, Debug, Default)]
pub struct NoSink;

impl Sink for NoSink {
    fn emit(&self, _record: &Record) {}
}

/// A sink that keeps every record (tests, the server's in-memory tail).
#[derive(Debug, Default)]
pub struct Collector {
    records: Mutex<Vec<Record>>,
}

impl Collector {
    /// A copy of the records received so far.
    pub fn records(&self) -> Vec<Record> {
        self.records.lock().expect("collector lock").clone()
    }
}

impl Sink for Collector {
    fn emit(&self, record: &Record) {
        self.records
            .lock()
            .expect("collector lock")
            .push(record.clone());
    }
}

/// The JSON-Lines writer of the CLI entry points: one lock, one line per record, the line
/// scrubbed of the job's secrets (`CLI log output`).
pub struct JsonLinesWriter<W: Write + Send> {
    out: Mutex<W>,
    secrets: Arc<Secrets>,
    failed: AtomicBool,
}

impl<W: Write + Send> JsonLinesWriter<W> {
    /// Write to `out`, scrubbing with `secrets`.
    pub fn new(out: W, secrets: Arc<Secrets>) -> Self {
        JsonLinesWriter {
            out: Mutex::new(out),
            secrets,
            failed: AtomicBool::new(false),
        }
    }

    /// Whether a write failed (a closed pipe). The log stops there and the failure is kept
    /// for the exit code: the log is the evidence of the run, and Python's `print` raises
    /// where this writer only remembers.
    pub fn failed(&self) -> bool {
        self.failed.load(Ordering::SeqCst)
    }

    /// The serialized, scrubbed line (without the newline) a record becomes.
    pub fn line(&self, record: &Record) -> String {
        self.secrets
            .scrub(&Dumps::DEFAULT.dumps(&Value::Object(record.clone())))
    }

    /// Give the writer back.
    pub fn into_inner(self) -> W {
        self.out.into_inner().expect("writer lock")
    }
}

impl<W: Write + Send> Sink for JsonLinesWriter<W> {
    fn emit(&self, record: &Record) {
        if self.failed() {
            return;
        }
        let line = self.line(record);
        let mut out = self.out.lock().expect("writer lock");
        // A failed write (a closed pipe) does not take the pipeline down — the product is the
        // output tree — but it is remembered (`failed`) rather than ignored.
        if writeln!(out, "{line}").and_then(|()| out.flush()).is_err() {
            self.failed.store(true, Ordering::SeqCst);
        }
    }
}

/// `datetime.now(timezone.utc).isoformat`.
pub fn now_iso() -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    format_utc(
        i64::try_from(now.as_secs()).unwrap_or(i64::MAX),
        now.subsec_micros(),
    )
}

/// A UTC instant as `datetime.isoformat` writes it: `YYYY-MM-DDTHH:MM:SS[.ffffff]+00:00`
/// (Python omits the fraction when the microseconds are zero).
pub fn format_utc(unix_seconds: i64, microseconds: u32) -> String {
    let days = unix_seconds.div_euclid(86_400);
    let seconds = unix_seconds.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    let fraction = if microseconds == 0 {
        String::new()
    } else {
        format!(".{microseconds:06}")
    };
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}{fraction}+00:00",
        seconds / 3600,
        (seconds % 3600) / 60,
        seconds % 60
    )
}

/// Days since 1970-01-01 to a proleptic Gregorian date (Howard Hinnant's algorithm).
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let shifted = days + 719_468;
    let era = shifted.div_euclid(146_097);
    let day_of_era = shifted.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted_month = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * shifted_month + 2) / 5 + 1;
    let month = if shifted_month < 10 {
        shifted_month + 3
    } else {
        shifted_month - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    (
        year,
        u32::try_from(month).expect("1..=12"),
        u32::try_from(day).expect("1..=31"),
    )
}

/// `round(seconds, 3)` as the records carry durations.
pub fn round3(seconds: f64) -> f64 {
    // Python's `round(x, 3)` rounds the exact binary value to three decimals, ties to even
    // (`round(1.2345, 3)` is `1.234`: the double is just below the tie). Rust's `{:.3}` rounds
    // the same way; scaling by 1000 and rounding the product does not.
    format!("{seconds:.3}")
        .parse()
        .expect("a formatted float parses")
}

/// A pipeline record (`pipeline logging`): `ts`, `stage`, `level`, `message`, then the optional
/// `rendition_id`, `check_id` and `duration_s` in that order.
pub fn pipeline_record(
    stage: &str,
    level: &str,
    message: &str,
    rendition_id: Option<&str>,
    check_id: Option<CheckId>,
    duration_s: Option<f64>,
) -> Record {
    let mut record = Record::new();
    record.insert("ts".into(), Value::String(now_iso()));
    record.insert("stage".into(), Value::String(stage.to_string()));
    record.insert("level".into(), Value::String(level.to_string()));
    record.insert("message".into(), Value::String(message.to_string()));
    if let Some(id) = rendition_id {
        record.insert("rendition_id".into(), Value::String(id.to_string()));
    }
    if let Some(check) = check_id {
        record.insert("check_id".into(), Value::String(check.as_str().to_string()));
    }
    if let Some(seconds) = duration_s {
        record.insert("duration_s".into(), Value::from(round3(seconds)));
    }
    record
}
