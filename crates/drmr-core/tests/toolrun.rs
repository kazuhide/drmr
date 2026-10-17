//! The tool runner and the secret registry against the cases of the fixture cases.
//! Secrets here are synthetic (the sample IV of the fixtures), never real key material.
//! Controlled subprocess output uses a native fixture executable on every platform.

use base64::Engine as _;
use base64::engine::general_purpose::{STANDARD, STANDARD_NO_PAD, URL_SAFE};
use drmr_core::canonical::Dumps;
use drmr_core::events::{Collector, JsonLinesWriter, NoSink, Record};
use drmr_core::toolrun::{
    REDACTED, RunOptions, SecretRegistry, Secrets, ToolError, ToolRunner, bounded, patterns_for,
};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

const IV: &str = "0f0e0d0c0b0a09080706050403020100";

fn argv(items: &[&str]) -> Vec<String> {
    items.iter().map(ToString::to_string).collect()
}

struct Response {
    _directory: tempfile::TempDir,
    arguments: Vec<String>,
}

impl std::ops::Deref for Response {
    type Target = [String];
    fn deref(&self) -> &Self::Target {
        &self.arguments
    }
}

fn fixture_response(spec: &drmr_test_support::ToolSpec, arguments: &[&str]) -> Response {
    let directory = tempfile::tempdir().unwrap();
    let tool = drmr_test_support::install(&directory.path().join("response"), spec).unwrap();
    let mut command = vec![tool.to_str().unwrap().to_string()];
    command.extend(arguments.iter().map(|arg| (*arg).to_string()));
    Response {
        _directory: directory,
        arguments: command,
    }
}

fn response(stdout: &[u8], stderr: &[u8], exit_code: i32, arguments: &[&str]) -> Response {
    fixture_response(
        &drmr_test_support::ToolSpec {
            stdout: stdout.to_vec(),
            stderr: stderr.to_vec(),
            exit_code,
            ..drmr_test_support::ToolSpec::default()
        },
        arguments,
    )
}

fn sleeping_response(sleep_ms: u64) -> Response {
    fixture_response(
        &drmr_test_support::ToolSpec {
            sleep_ms,
            ..drmr_test_support::ToolSpec::default()
        },
        &[],
    )
}

fn python() -> &'static str {
    if cfg!(windows) { "python" } else { "python3" }
}

fn runner() -> (Arc<ToolRunner>, Arc<Collector>, Arc<Secrets>) {
    let secrets = Arc::new(Secrets::new());
    let collector = Arc::new(Collector::default());
    let runner = Arc::new(ToolRunner::new(secrets.clone(), collector.clone()));
    (runner, collector, secrets)
}

fn text_of(records: &[Record]) -> String {
    serde_json::to_string(records).unwrap()
}

fn utf16le(text: &str) -> Vec<u8> {
    text.encode_utf16().flat_map(u16::to_le_bytes).collect()
}

/// The forms a tool may print a hex secret in.
fn forms(secret: &str) -> Vec<(&'static str, String)> {
    let bytes = hex::decode(secret).unwrap();
    let pairs: Vec<String> = (0..secret.len())
        .step_by(2)
        .map(|i| secret[i..i + 2].to_string())
        .collect();
    vec![
        ("lower", secret.to_string()),
        ("upper", secret.to_uppercase()),
        ("spaced", pairs.join(" ")),
        ("colon", pairs.join(":")),
        (
            "comma-upper",
            pairs
                .iter()
                .map(|p| p.to_uppercase())
                .collect::<Vec<_>>()
                .join(", "),
        ),
        (
            "0x",
            pairs
                .iter()
                .map(|p| format!("0x{p}"))
                .collect::<Vec<_>>()
                .join(" "),
        ),
        ("dashes", pairs.join("-")),
        ("base64", STANDARD.encode(&bytes)),
        ("base64-nopad", STANDARD_NO_PAD.encode(&bytes)),
        ("base64-lower-text", STANDARD.encode(secret.as_bytes())),
        (
            "urlsafe-nopad-upper-text",
            URL_SAFE
                .encode(secret.to_uppercase().as_bytes())
                .trim_end_matches('=')
                .to_string(),
        ),
    ]
}

#[test]
fn run_options_debug_prints_no_argv_text() {
    let redacted = [
        "packager".to_string(),
        "--pssh".to_string(),
        "0a0b0c".to_string(),
    ];
    let options = RunOptions {
        argv_redacted: Some(&redacted),
        secrets: &["0a0b0c"],
        ..RunOptions::default()
    };
    let text = format!("{options:?}");
    assert!(text.contains("argv_redacted: Some(3)"), "{text}");
    assert!(
        !text.contains("0a0b0c") && !text.contains("packager"),
        "{text}"
    );
}

#[test]
fn registered_secrets_are_scrubbed_in_every_encoding_from_every_record() {
    let (runner, collector, secrets) = runner();
    secrets.register([IV]);
    let forms = forms(IV);
    let joined = forms
        .iter()
        .map(|(_, form)| form.clone())
        .collect::<Vec<_>>()
        .join(" | ");
    let spaced = forms[2].1.clone();
    // The IV on the argv (no argv_redacted: the registry covers it), in stdout, and on stderr
    // the way mp4dump prints the tenc constant IV.
    let completed = runner
        .run_tool(
            &response(
                format!("{joined}\n").as_bytes(),
                format!("default_constant_IV: [{spaced}]\n").as_bytes(),
                0,
                &[&joined, &spaced, "--iv", IV],
            ),
            "mp4dump",
            &RunOptions::default(),
        )
        .unwrap();
    assert!(
        completed.stdout_text().contains(IV),
        "the caller's copy is real"
    );
    assert_eq!(completed.returncode, Some(0));
    runner.note_command(
        &argv(&["ffmpeg", "-iv", &IV.to_uppercase()]),
        "ffmpeg",
        None,
    );
    runner.note_result(
        "ffmpeg",
        Some(1),
        0.1,
        None,
        &format!("bad iv {}", forms[6].1),
        None,
    );

    let records = collector.records();
    let text = text_of(&records);
    for (name, form) in &forms {
        assert!(!text.contains(form.as_str()), "{name}");
    }
    assert_eq!(records.len(), 4);
    let (command, output, note, result) = (&records[0], &records[1], &records[2], &records[3]);
    assert_eq!(
        command["argv"].as_array().unwrap().last().unwrap(),
        &json!(REDACTED)
    );
    assert_eq!(command["event"], json!("command"));
    assert_eq!(output["event"], json!("tool-output"));
    assert!(
        output["stderr"]
            .as_str()
            .unwrap()
            .contains(&format!("default_constant_IV: [{REDACTED}]")),
        "{}",
        output["stderr"]
    );
    assert_eq!(output["level"], json!("info"));
    assert_eq!(note["argv"], json!(["ffmpeg", "-iv", REDACTED]));
    assert_eq!(result["stderr"], json!(format!("bad iv {REDACTED}")));
    assert_eq!(result["level"], json!("error"));
    secrets.clear();
    assert_eq!(secrets.scrub(IV), IV, "clearing forgets the job's secrets");
}

#[test]
fn scrub_covers_literal_secrets_and_ignores_short_strings() {
    let secrets = Secrets::with([
        "0f0e0d0c0b0a09080706050403020100",
        "s3cret-token-VALUE",
        "ab",
    ]);
    let line = Dumps::DEFAULT.dumps(&json!({"stage": "report", "message": "iv [0F 0E 0D 0C 0B 0A 09 08 07 06 05 04 03 02 01 00] token S3CRET-TOKEN-value ab"}));
    let scrubbed: Value = serde_json::from_str(&secrets.scrub(&line)).unwrap();
    assert_eq!(
        scrubbed["message"],
        json!(format!("iv [{REDACTED}] token {REDACTED} ab"))
    );
    assert_eq!(
        secrets.scrub("abc"),
        "abc",
        "a secret shorter than 8 characters is ignored"
    );
    assert!(patterns_for("ab").is_empty());
    assert!(patterns_for("        ").is_empty());
    assert!(
        patterns_for("1234567").is_empty(),
        "seven characters: ignored"
    );
    assert!(!patterns_for("12345678").is_empty(), "eight: the minimum");
    assert_eq!(
        Secrets::with(["1234567"]).scrub("x 1234567 y"),
        "x 1234567 y"
    );
    assert_eq!(
        Secrets::with(["12345678"]).scrub("x 12345678 y"),
        format!("x {REDACTED} y")
    );
    // Python's strip removes U+001C–U+001F too; a secret padded with them still registers.
    assert_eq!(
        Secrets::with(["\u{1c}12345678\u{1f}"]).scrub("12345678"),
        REDACTED
    );
    assert_eq!(
        secrets.scrub_extra(
            "key 00112233445566778899aabbccddeeff",
            &["00112233445566778899aabbccddeeff"]
        ),
        format!("key {REDACTED}")
    );
}

#[test]
fn text_secrets_are_scrubbed_as_written_percent_encoded_hex_utf16_and_base64() {
    // The clear-HLS job's LA_URL sits inside the --pssh argument as UTF-16LE hex; the FairPlay
    // URI appears as written (: config values are never echoed).
    let la_url = "https://license.example.invalid/playready/rightsmanager.asmx";
    let skd = "skd://content-id-42";
    let utf16_hex = hex::encode(utf16le(la_url));
    let (runner, collector, secrets) = runner();
    secrets.register([la_url, skd]);
    runner.note_command(
        &argv(&[
            "packager",
            "--pssh",
            &format!("0000003a70737368{}0000", utf16_hex.to_uppercase()),
            "--hls_key_uri",
            skd,
            "--note",
            &hex::encode(la_url),
            &STANDARD.encode(la_url),
            URL_SAFE.encode(utf16le(skd)).trim_end_matches('='),
            "https%3A%2F%2Flicense.example.invalid%2Fplayready%2Frightsmanager.asmx",
        ]),
        "packager",
        None,
    );
    let records = collector.records();
    let text = text_of(&records);
    for form in [
        la_url,
        skd,
        utf16_hex.as_str(),
        &utf16_hex.to_uppercase(),
        &hex::encode(la_url),
        &STANDARD.encode(la_url),
        "rightsmanager",
        "content-id-42",
    ] {
        assert!(!text.contains(form), "{form}");
    }
    let argv = records[0]["argv"].as_array().unwrap();
    assert_eq!(
        argv[2],
        json!(format!("0000003a70737368{REDACTED}0000")),
        "the rest of the PSSH hex stays"
    );
    assert_eq!(argv[4], json!(REDACTED));
    assert_eq!(argv.last().unwrap(), &json!(REDACTED));
}

#[test]
fn a_text_secret_with_an_ampersand_is_scrubbed_in_its_xml_escaped_utf16_form() {
    let la_url = "https://license.example.invalid/rm.asmx?a=1&b=2";
    let escaped = la_url.replace('&', "&amp;");
    let escaped_utf16_hex = hex::encode(utf16le(&escaped));
    let (runner, collector, secrets) = runner();
    secrets.register([la_url]);
    runner.note_command(
        &argv(&[
            "packager",
            "--pssh",
            &format!("00{}ff", escaped_utf16_hex.to_uppercase()),
            &escaped,
            la_url,
        ]),
        "packager",
        Some("v000"),
    );
    let records = collector.records();
    assert_eq!(
        records[0]["argv"],
        json!([
            "packager",
            "--pssh",
            format!("00{REDACTED}ff"),
            REDACTED,
            REDACTED
        ])
    );
    assert_eq!(records[0]["rendition_id"], json!("v000"));
}

#[test]
fn output_is_bounded_and_marked_truncated() {
    let (runner, collector, _) = runner();
    let completed = runner
        .run_tool(
            &response(&vec![b'x'; 300], b"", 0, &[]),
            "demo",
            &RunOptions {
                max_output_chars: 100,
                ..RunOptions::default()
            },
        )
        .unwrap();
    assert_eq!(completed.stdout.len(), 300, "the caller's copy is complete");
    let output = &collector.records()[1];
    assert_eq!(output["truncated"], json!(true));
    assert_eq!(
        output["stdout"],
        json!(format!("{}\n… (200 characters not shown)", "x".repeat(100)))
    );
    assert_eq!(bounded("abc", 3), ("abc".to_string(), false));
    assert_eq!(
        bounded("日本語です", 2),
        ("日本\n… (3 characters not shown)".to_string(), true)
    );
}

#[test]
fn output_consumed_as_data_is_not_logged() {
    let (runner, collector, _) = runner();
    let completed = runner
        .run_tool(
            &response(b"payload-bytes".repeat(10).as_slice(), b"", 0, &[]),
            "ffprobe",
            &RunOptions {
                text: false,
                log_stdout: false,
                ..RunOptions::default()
            },
        )
        .unwrap();
    assert!(
        completed.stdout.starts_with(b"payload-bytes"),
        "the caller's copy is complete"
    );
    let records = collector.records();
    assert!(!text_of(&records).contains("payload-bytes"));
    let stdout = records[1]["stdout"].as_str().unwrap();
    assert!(
        stdout.starts_with("(stdout not logged: ") && stdout.contains("130 bytes consumed as data"),
        "{stdout}"
    );
    assert_eq!(records[1]["truncated"], json!(false));
}

#[test]
fn timeout_and_spawn_failures_raise_tool_errors_without_the_argv() {
    let (runner, collector, _) = runner();
    let start = Instant::now();
    let error = runner
        .run_tool(
            &sleeping_response(5_000),
            "packager",
            &RunOptions {
                timeout: Some(Duration::from_millis(200)),
                ..RunOptions::default()
            },
        )
        .unwrap_err();
    assert!(
        start.elapsed() < Duration::from_secs(2),
        "the child is killed at the deadline, not waited for ({:?})",
        start.elapsed()
    );
    assert_eq!(
        error,
        ToolError {
            tool: "packager".to_string(),
            kind: "timeout"
        }
    );
    assert_eq!(error.to_string(), "packager: timeout");
    let records = collector.records();
    assert_eq!(records[1]["returncode"], Value::Null);
    assert_eq!(records[1]["stderr"], json!("timed out after 0.2s"));
    assert_eq!(records[1]["level"], json!("error"));
    assert_eq!(records[1]["call_id"], records[0]["call_id"]);
    let error = runner
        .run_tool(
            &sleeping_response(5_000),
            "packager",
            &RunOptions {
                timeout: Some(Duration::from_secs(1)),
                ..RunOptions::default()
            },
        )
        .unwrap_err();
    assert_eq!(error.kind, "timeout");
    assert_eq!(
        collector.records()[3]["stderr"],
        json!("timed out after 1s"),
        "an integral timeout prints like Python's int"
    );

    let error = runner
        .run_tool(
            &argv(&["drmr-no-such-tool-0123", "--flag"]),
            "ffmpeg",
            &RunOptions::default(),
        )
        .unwrap_err();
    assert_eq!(error.kind, "unavailable");
    assert_eq!(error.to_string(), "ffmpeg: unavailable");
    let records = collector.records();
    assert!(
        records[5]["stderr"]
            .as_str()
            .unwrap()
            .starts_with("could not run: ")
    );
    assert_eq!(records[5]["returncode"], Value::Null);

    let exit = runner
        .run_tool(&response(b"", b"", 3, &[]), "demo", &RunOptions::default())
        .unwrap();
    assert_eq!(exit.returncode, Some(3));
    assert_eq!(
        collector.records()[7]["level"],
        json!("error"),
        "a non-zero exit is recorded as an error, not raised"
    );
}

#[test]
fn a_grandchild_holding_the_pipes_cannot_outlive_the_timeout() {
    // The direct child exits at once; a background `sleep` inherited both pipes and holds
    // them for 3 s. Python's `communicate(timeout)` raises at the deadline; so does the runner.
    let (runner, collector, _) = runner();
    let start = Instant::now();
    let error = runner
        .run_tool(
            &argv(&[python(), "-c", "import subprocess,sys; subprocess.Popen([sys.executable,'-c','import time;time.sleep(3)'])"]),
            "packager",
            &RunOptions {
                timeout: Some(Duration::from_millis(500)),
                ..RunOptions::default()
            },
        )
        .unwrap_err();
    assert_eq!(error.kind, "timeout");
    assert!(
        start.elapsed() < Duration::from_secs(2),
        "returned after {:?}, not at the deadline",
        start.elapsed()
    );
    let records = collector.records();
    assert_eq!(records[1]["stderr"], json!("timed out after 0.5s"));
    assert_eq!(records[1]["returncode"], Value::Null);
}

#[test]
fn text_mode_records_universal_newlines_and_bytes_mode_keeps_them() {
    let (runner, collector, _) = runner();
    let completed = runner
        .run_tool(
            &response(b"a\r\nb\rc", b"x\r\n", 0, &[]),
            "demo",
            &RunOptions::default(),
        )
        .unwrap();
    assert_eq!(completed.stdout, b"a\r\nb\rc", "the caller's bytes are raw");
    let records = collector.records();
    assert_eq!(
        records[1]["stdout"],
        json!("a\nb\nc"),
        "text mode: Python's universal newlines"
    );
    assert_eq!(records[1]["stderr"], json!("x\n"));
    runner
        .run_tool(
            &response(b"a\r\nb\rc", b"x\r\n", 0, &[]),
            "demo",
            &RunOptions {
                text: false,
                ..RunOptions::default()
            },
        )
        .unwrap();
    assert_eq!(
        collector.records()[3]["stdout"],
        json!("a\r\nb\rc"),
        "bytes mode: as printed"
    );
}

#[test]
fn a_per_call_secret_on_the_shown_argv_is_scrubbed_without_argv_redacted() {
    let secret = "cafebabecafebabecafebabecafebabe";
    let (runner, collector, _) = runner();
    let command = response(b"", b"", 0, &["--key", secret]);
    runner
        .run_tool(
            &command,
            "packager",
            &RunOptions {
                secrets: &[secret],
                ..RunOptions::default()
            },
        )
        .unwrap();
    assert_eq!(
        collector.records()[0]["argv"],
        json!([command.arguments[0].as_str(), "--key", REDACTED])
    );
}

#[cfg(unix)]
#[test]
fn a_signal_is_recorded_as_a_negative_return_code() {
    let (runner, collector, _) = runner();
    let completed = runner
        .run_tool(
            &argv(&["sh", "-c", "kill -TERM $$"]),
            "demo",
            &RunOptions::default(),
        )
        .unwrap();
    assert_eq!(completed.returncode, Some(-15), "SIGTERM, as Python's -15");
    let output = &collector.records()[1];
    assert_eq!(output["returncode"], json!(-15));
    assert_eq!(output["level"], json!("error"));
}

#[test]
fn run_tool_records_carry_the_rendition_id() {
    let (runner, collector, _) = runner();
    runner
        .run_tool(
            &response(b"", b"", 0, &[]),
            "ffmpeg",
            &RunOptions {
                rendition_id: Some("v001"),
                ..RunOptions::default()
            },
        )
        .unwrap();
    let records = collector.records();
    assert_eq!(records[0]["rendition_id"], json!("v001"));
    assert_eq!(records[1]["rendition_id"], json!("v001"));
    assert_eq!(records[0]["tool"], json!("ffmpeg"));
    assert_eq!(records[1]["tool"], json!("ffmpeg"));
}

#[test]
fn text_output_consumed_as_data_is_noted_in_characters() {
    let (runner, collector, _) = runner();
    runner
        .run_tool(
            &response("café ok".as_bytes(), b"", 0, &[]),
            "ffprobe",
            &RunOptions {
                log_stdout: false,
                ..RunOptions::default()
            },
        )
        .unwrap();
    assert_eq!(
        collector.records()[1]["stdout"],
        json!("(stdout not logged: 7 characters consumed as data)"),
        "characters, not bytes (8), in text mode"
    );
}

#[test]
fn a_text_secret_with_angle_brackets_is_scrubbed_in_its_xml_escaped_forms() {
    let la_url = "https://license.example.invalid/rm.asmx?t=<tok>&u=1";
    let escaped = "https://license.example.invalid/rm.asmx?t=&lt;tok&gt;&amp;u=1";
    let escaped_utf16_hex = hex::encode(utf16le(escaped));
    let (runner, collector, secrets) = runner();
    secrets.register([la_url]);
    runner.note_command(
        &argv(&["packager", escaped, &escaped_utf16_hex, la_url]),
        "packager",
        None,
    );
    assert_eq!(
        collector.records()[0]["argv"],
        json!(["packager", REDACTED, REDACTED, REDACTED])
    );
}

#[test]
fn bytes_separated_by_pythons_whitespace_controls_are_scrubbed() {
    // Python's `\s` matches U+001C–U+001F (`str.isspace`); Rust's does not, so the byte
    // separator names them.
    let iv = "0f0e0d0c0b0a09080706050403020100";
    for separator in ['\u{1c}', '\u{1d}', '\u{1e}', '\u{1f}', '\u{85}', '\u{a0}'] {
        let pairs: Vec<&str> = (0..iv.len()).step_by(2).map(|i| &iv[i..i + 2]).collect();
        let separated = pairs.join(&separator.to_string());
        assert_eq!(
            Secrets::with([iv]).scrub(&format!("iv {separated} end")),
            format!("iv {REDACTED} end"),
            "separator U+{:04X}",
            u32::from(separator)
        );
    }
}

#[test]
fn call_ids_are_process_wide_across_runners() {
    // Python's `_next_call_id` is one counter per process: two runners never reuse an id.
    let (first, first_records, _) = runner();
    let (second, second_records, _) = runner();
    let a = first.note_command(&argv(&["true"]), "demo", None);
    let b = second.note_command(&argv(&["true"]), "demo", None);
    let c = first.note_command(&argv(&["true"]), "demo", None);
    assert!(a < b && b < c, "{a} {b} {c}");
    assert_eq!(first_records.records()[0]["call_id"], json!(a));
    assert_eq!(second_records.records()[0]["call_id"], json!(b));
}

#[test]
fn each_command_and_its_output_share_a_call_id() {
    let (runner, collector, _) = runner();
    runner
        .run_tool(
            &response(b"1\n", b"", 0, &[]),
            "demo",
            &RunOptions::default(),
        )
        .unwrap();
    runner
        .run_tool(
            &response(b"2\n", b"", 0, &[]),
            "demo",
            &RunOptions::default(),
        )
        .unwrap();
    let streamed = runner.note_command(&argv(&["ffmpeg"]), "ffmpeg", None);
    runner.note_result("ffmpeg", Some(0), 0.1, None, "", Some(streamed));
    let ids: Vec<(String, u64)> = collector
        .records()
        .iter()
        .map(|r| {
            (
                r["event"].as_str().unwrap().to_string(),
                r["call_id"].as_u64().unwrap(),
            )
        })
        .collect();
    assert_eq!(ids.len(), 6);
    assert!(
        ids[0].1 == ids[1].1
            && ids[2].1 == ids[3].1
            && ids[4].1 == ids[5].1
            && ids[4].1 == streamed
    );
    assert_eq!(
        [ids[0].1, ids[2].1, ids[4].1]
            .iter()
            .collect::<std::collections::BTreeSet<_>>()
            .len(),
        3,
        "distinct per invocation"
    );
    assert_eq!(
        ids.iter()
            .map(|(event, _)| event.as_str())
            .collect::<Vec<_>>(),
        [
            "command",
            "tool-output",
            "command",
            "tool-output",
            "command",
            "tool-output"
        ]
    );
}

#[test]
fn call_ids_pair_commands_with_their_outputs_across_threads() {
    let (runner, collector, _) = runner();
    std::thread::scope(|scope| {
        for n in 0..8 {
            let runner = runner.clone();
            scope.spawn(move || {
                for k in 0..3 {
                    // The marker travels on the argv and comes back on stdout: a cross-wired
                    // output would pair with the wrong command.
                    let marker = format!("marker-{n}-{k}");
                    runner
                        .run_tool(
                            &response(format!("{marker}\n").as_bytes(), b"", 0, &[&marker]),
                            "demo",
                            &RunOptions::default(),
                        )
                        .unwrap();
                }
            });
        }
    });
    let records = collector.records();
    assert_eq!(records.len(), 48);
    let mut by_call: BTreeMap<u64, Vec<&Record>> = BTreeMap::new();
    for record in &records {
        by_call
            .entry(record["call_id"].as_u64().unwrap())
            .or_default()
            .push(record);
    }
    assert_eq!(by_call.len(), 24);
    let mut markers = std::collections::BTreeSet::new();
    for pair in by_call.values() {
        let [command, output] = pair.as_slice() else {
            panic!("{pair:?}");
        };
        assert_eq!(command["event"], json!("command"));
        assert_eq!(output["event"], json!("tool-output"));
        let marker = command["argv"]
            .as_array()
            .unwrap()
            .last()
            .unwrap()
            .as_str()
            .unwrap();
        assert_eq!(
            output["stdout"],
            json!(format!("{marker}\n")),
            "each output pairs with its own command"
        );
        markers.insert(marker.to_string());
    }
    assert_eq!(markers.len(), 24, "every call carried its own marker");
}

#[test]
fn command_and_output_records_carry_the_redacted_argv_and_the_per_call_secrets_scrubbed() {
    // The Shaka call: the key material is on the argv, `argv_redacted` replaces it in the
    // record and the per-call `secrets` are scrubbed from the output without being registered.
    let secret = "feedfacefeedfacefeedfacefeedface";
    let (runner, collector, secrets) = runner();
    assert_eq!(
        secrets.scrub(secret),
        secret,
        "not registered: the per-call list does it"
    );
    let shown = argv(&["sh", "-c", "<script>"]);
    let completed = runner
        .run_tool(
            &response(
                format!("out {secret} ok\n").as_bytes(),
                format!("err {}\n", secret.to_uppercase()).as_bytes(),
                3,
                &[secret],
            ),
            "demo",
            &RunOptions {
                argv_redacted: Some(&shown),
                secrets: &[secret],
                ..RunOptions::default()
            },
        )
        .unwrap();
    assert_eq!(completed.returncode, Some(3));
    assert!(
        completed.stdout_text().contains(secret),
        "the caller still gets the real result"
    );
    let records = collector.records();
    let events: Vec<&str> = records
        .iter()
        .map(|r| r["event"].as_str().unwrap())
        .collect();
    assert_eq!(events, ["command", "tool-output"]);
    let (command, output) = (&records[0], &records[1]);
    assert_eq!(command["argv"], json!(["sh", "-c", "<script>"]));
    assert_eq!(command["tool"], json!("demo"));
    assert_eq!(command["stage"], json!("tool"));
    assert_eq!(command["message"], json!("command"));
    assert_eq!(output["returncode"], json!(3));
    assert_eq!(output["level"], json!("error"));
    assert_eq!(output["truncated"], json!(false));
    assert!(output["duration_s"].as_f64().unwrap() >= 0.0);
    assert_eq!(output["stdout"], json!(format!("out {REDACTED} ok\n")));
    assert_eq!(output["stderr"], json!(format!("err {REDACTED}\n")));
    let text = text_of(&records);
    assert!(!text.contains(secret) && !text.contains(&secret.to_uppercase()));
    assert!(records.iter().all(|r| r.contains_key("ts")));
}

#[test]
fn a_spawn_failure_naming_a_secret_bearing_binary_leaves_no_trace_in_the_records() {
    let secret = "0123456789abcdef0123456789abcdef";
    let (runner, collector, _) = runner();
    let shown = argv(&["<redacted argv>"]);
    let error = runner
        .run_tool(
            &argv(&[&format!("/nonexistent/drmr-tool-{secret}"), "x"]),
            "gone",
            &RunOptions {
                argv_redacted: Some(&shown),
                secrets: &[secret],
                ..RunOptions::default()
            },
        )
        .unwrap_err();
    assert_eq!(error.kind, "unavailable");
    assert_eq!(error.to_string(), "gone: unavailable");
    assert!(!format!("{error:?}").contains(secret));
    let records = collector.records();
    assert_eq!(records.len(), 2);
    assert_eq!(records[0]["argv"], json!(["<redacted argv>"]));
    assert_eq!(records[1]["event"], json!("tool-output"));
    assert_eq!(records[1]["returncode"], Value::Null);
    assert!(
        records[1]["stderr"]
            .as_str()
            .unwrap()
            .starts_with("could not run: "),
        "{}",
        records[1]["stderr"]
    );
    assert!(
        !text_of(&records).contains(secret),
        "a spawn error message naming the binary is scrubbed"
    );
}

#[test]
fn bytes_mode_keeps_the_raw_output_and_decodes_for_the_record_only() {
    let (runner, collector, _) = runner();
    let completed = runner
        .run_tool(
            &response(b"\xff\xfeabc", b"", 0, &[]),
            "demo",
            &RunOptions {
                text: false,
                ..RunOptions::default()
            },
        )
        .unwrap();
    assert_eq!(
        completed.stdout, b"\xff\xfeabc",
        "the caller's copy is the raw bytes"
    );
    let stdout = collector.records()[1]["stdout"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(stdout.ends_with("abc"), "{stdout:?}");
    assert_eq!(
        stdout.chars().count(),
        5,
        "two replacement characters, then the text"
    );
}

#[test]
fn without_a_sink_nothing_is_emitted_and_tools_run_as_before() {
    let runner = ToolRunner::new(Arc::new(Secrets::new()), Arc::new(NoSink));
    let completed = runner
        .run_tool(
            &response(b"quiet\n", b"", 0, &[]),
            "demo",
            &RunOptions::default(),
        )
        .unwrap();
    assert_eq!(completed.returncode, Some(0));
    assert_eq!(completed.stdout_text(), "quiet\n");
}

#[test]
fn the_json_lines_writer_serializes_emission_across_threads_and_scrubs_every_line() {
    // Eight threads, three calls each, through the CLI's writer: 48 whole lines, every one a
    // record, each command paired with its output, and the registered IV on none of them.
    let secrets = Arc::new(Secrets::with([IV]));
    let writer = Arc::new(JsonLinesWriter::new(Vec::new(), secrets.clone()));
    let sink: Arc<dyn drmr_core::events::Sink> = writer.clone();
    let runner = Arc::new(ToolRunner::new(secrets, sink));
    std::thread::scope(|scope| {
        for _ in 0..8 {
            let runner = runner.clone();
            scope.spawn(move || {
                for _ in 0..3 {
                    runner
                        .run_tool(
                            &response(
                                format!("{IV}\n").as_bytes(),
                                format!("{IV}\n").as_bytes(),
                                0,
                                &[IV],
                            ),
                            "demo",
                            &RunOptions::default(),
                        )
                        .unwrap();
                }
            });
        }
    });
    drop(runner);
    let bytes = Arc::try_unwrap(writer).ok().unwrap().into_inner();
    let text = String::from_utf8(bytes).unwrap();
    assert!(!text.contains(IV));
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 48);
    let mut by_call: BTreeMap<u64, Vec<String>> = BTreeMap::new();
    for line in &lines {
        let record: Value = serde_json::from_str(line).unwrap_or_else(|e| panic!("{e}: {line}"));
        by_call
            .entry(record["call_id"].as_u64().unwrap())
            .or_default()
            .push(record["event"].as_str().unwrap().to_string());
        if record["event"] == json!("tool-output") {
            assert_eq!(record["stdout"], json!(format!("{REDACTED}\n")));
            assert_eq!(record["stderr"], json!(format!("{REDACTED}\n")));
        }
    }
    assert_eq!(by_call.len(), 24);
    assert!(
        by_call
            .values()
            .all(|events| events == &["command", "tool-output"])
    );
}

#[test]
fn debug_output_of_the_secret_holding_types_never_prints_a_secret() {
    // `{:?}` of a registry, of the per-call options or of a result (a `dbg!`, an
    // `unwrap_err`, a `#[derive(Debug)]` on a future holder) prints counts and sizes only.
    let mut registry = SecretRegistry::new();
    assert_eq!(format!("{registry:?}"), "SecretRegistry { patterns: 0 }");
    registry.register([IV]);
    let shown = format!("{registry:?}");
    assert!(
        !shown.contains(IV) && !shown.contains("0f0e") && !shown.contains("0F0E"),
        "{shown}"
    );
    assert!(shown.starts_with("SecretRegistry { patterns: "), "{shown}");

    let secrets = Secrets::with([IV, "https://license.example.invalid/rightsmanager.asmx"]);
    let shown = format!("{secrets:?}");
    assert!(
        !shown.contains(IV) && !shown.contains("example.invalid") && !shown.contains("0f0e"),
        "{shown}"
    );
    assert!(shown.starts_with("Secrets { patterns: "), "{shown}");

    let options = RunOptions {
        secrets: &[IV, "s3cret-token-VALUE"],
        rendition_id: Some("v000"),
        ..RunOptions::default()
    };
    let shown = format!("{options:?}");
    assert!(!shown.contains(IV) && !shown.contains("s3cret"), "{shown}");
    assert!(
        shown.contains("secrets: 2") && shown.contains("v000"),
        "{shown}"
    );

    let (runner, _, _) = runner();
    let completed = runner
        .run_tool(
            &response(
                format!("{IV}\n").as_bytes(),
                format!("{IV}\n").as_bytes(),
                0,
                &[IV],
            ),
            "mp4dump",
            &RunOptions::default(),
        )
        .unwrap();
    let shown = format!("{completed:?}");
    assert!(!shown.contains(IV), "{shown}");
    assert_eq!(
        shown,
        "Completed { returncode: Some(0), stdout_bytes: 33, stderr_bytes: 33 }"
    );
    let failed: Result<drmr_core::toolrun::Completed, ToolError> = Ok(completed);
    assert!(!format!("{failed:?}").contains(IV));
}

#[cfg(unix)]
#[test]
fn a_runner_with_its_own_path_looks_bare_names_up_there_and_records_them_bare() {
    // `cli`'s --packager-bin prepends a directory to os.environ["PATH"]: every tool the job
    // runs by a bare name resolves there first, and the records show the bare name.
    let dir = tempfile::tempdir().unwrap();
    let tool = dir.path().join("drmr-path-probe");
    // Executing a freshly written script can race sibling process spawns on Linux:
    // an inherited writable descriptor remains open until exec, causing ETXTBSY.
    // Alias an existing executable so PATH discovery does not depend on that race.
    std::os::unix::fs::symlink("/bin/sh", &tool).unwrap();
    let path = format!("{}:/usr/bin:/bin", dir.path().display());
    let collector = Arc::new(Collector::default());
    let runner = ToolRunner::new(Arc::new(Secrets::new()), collector.clone()).with_path(&path);
    assert_eq!(runner.path(), Some(std::ffi::OsStr::new(&path)));

    let arguments = argv(&["drmr-path-probe", "-c", "echo probed \"$PATH\""]);
    let completed = runner
        .run_tool(&arguments, "probe", &RunOptions::default())
        .unwrap();

    assert_eq!(completed.returncode, Some(0));
    assert_eq!(completed.stdout_text(), format!("probed {path}\n"));
    let records = collector.records();
    assert_eq!(records[0]["argv"], json!(arguments));

    // This process's PATH does not have it: a runner without its own PATH cannot run it.
    let (plain, _, _) = runner_without_path();
    let error = plain
        .run_tool(&arguments, "probe", &RunOptions::default())
        .unwrap_err();
    assert_eq!((error.tool.as_str(), error.kind), ("probe", "unavailable"));
}

fn runner_without_path() -> (Arc<ToolRunner>, Arc<Collector>, Arc<Secrets>) {
    let (runner, collector, secrets) = runner();
    assert_eq!(runner.path(), None);
    (runner, collector, secrets)
}

#[cfg(windows)]
#[test]
fn a_runner_with_its_own_path_looks_bare_windows_names_up_and_preserves_child_path() {
    let directory = tempfile::tempdir().unwrap();
    drmr_test_support::install(
        &directory.path().join("drmr-path-probe"),
        &drmr_test_support::ToolSpec::default()
            .with("stdout_env", "PATH")
            .with("stdout_env_prefix", "probed ")
            .with("stdout_env_suffix", "\n"),
    )
    .unwrap();
    let path = directory.path().to_str().unwrap();
    let collector = Arc::new(Collector::default());
    let runner = ToolRunner::new(Arc::new(Secrets::new()), collector.clone()).with_path(path);
    assert_eq!(runner.path(), Some(std::ffi::OsStr::new(path)));
    let arguments = argv(&["drmr-path-probe"]);
    let completed = runner
        .run_tool(&arguments, "probe", &RunOptions::default())
        .unwrap();
    assert_eq!(completed.returncode, Some(0));
    assert_eq!(completed.stdout_text(), format!("probed {path}\n"));
    assert_eq!(collector.records()[0]["argv"], json!(arguments));
    let (plain, _, _) = runner_without_path();
    let error = plain
        .run_tool(&arguments, "probe", &RunOptions::default())
        .unwrap_err();
    assert_eq!((error.tool.as_str(), error.kind), ("probe", "unavailable"));
}
