//! Output/key consistency is checked before URL acquisition or media work.
use super::*;
use serde_json::{Value, json};
use std::fmt::Write as _;

const KID: &str = "00000000000000000000000000000001";
const KEY: &str = "00112233445566778899aabbccddeeff";
const PRIVATE_ID: &str = "private-output-sentinel";
const PRIVATE_URI: &str = "skd://private-key-sentinel";

fn named_key() -> Value {
    json!({"kid": KID, "key": KEY})
}

fn assert_refused_before_acquisition(intent: &Value, output_keys: &Value, expected: &str) {
    assert_command_refused_before_acquisition(
        |command, root| {
            let outputs = root.join("outputs.json");
            let keys = root.join("keys.json");
            policy(
                &outputs,
                &json!({"schema_version":4,"outputs":[intent]}).to_string(),
            );
            policy(
                &keys,
                &json!({"schema_version":2,"outputs":output_keys}).to_string(),
            );
            command.arg("--output-config").arg(outputs);
            command.arg("--keys-file").arg(keys);
        },
        expected,
    );
}

fn assert_quick_refused_before_acquisition(
    format: Option<&str>,
    encryption: &str,
    output_key: Option<&Value>,
    expected: &str,
) {
    assert_command_refused_before_acquisition(
        |command, root| {
            command.args(["--encryption", encryption]);
            if let Some(format) = format {
                command.args(["--output-format", format]);
            }
            if let Some(output_key) = output_key {
                let keys = root.join("keys.json");
                policy(
                    &keys,
                    &json!({"schema_version":2,"outputs":{"default":output_key}}).to_string(),
                );
                command.arg("--keys-file").arg(keys);
            }
        },
        expected,
    );
}

fn assert_command_refused_before_acquisition(
    configure: impl FnOnce(&mut Command, &Path),
    expected: &str,
) {
    let root = drmr_core::native::private_tempdir().unwrap();
    let server = Server::new();
    let download = root.path().join("source");
    let output = root.path().join("result");
    let fetch = root.path().join("fetch.json");
    policy(
        &fetch,
        r#"{"allow_http":true,"allow_private_network":true,"attempts":1}"#,
    );
    let mut command = Command::new(env!("CARGO_BIN_EXE_drmr"));
    command
        .args(["convert", &server.url])
        .arg("--download-dir")
        .arg(&download)
        .arg("--fetch-config")
        .arg(&fetch)
        .arg("--output-dir")
        .arg(&output)
        .args(["--json", "--progress", "json"]);
    configure(&mut command, root.path());
    let result = command.output().unwrap();

    // This is an observable boundary: invalid output settings need no source bytes.
    server.assert_unused();
    assert_eq!(result.status.code(), Some(2));
    assert!(!download.exists() && !output.exists());
    let report: Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(report["status"], "failed");
    assert_eq!(report["error"]["code"], expected);
    assert_eq!(report["error"]["last_activity"], "configuration");
    let records: Vec<Value> = std::str::from_utf8(&result.stderr)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert!(records.iter().all(|record| {
        record["tools_started"].as_u64().unwrap_or(0) == 0
            && record["active_tool_count"].as_u64().unwrap_or(0) == 0
    }));
    assert_eq!(
        records.last().expect("progress must report the failure")["event"],
        "failed"
    );
    for stream in [&result.stdout, &result.stderr] {
        let text = String::from_utf8_lossy(stream);
        for private in [PRIVATE_ID, PRIVATE_URI, KID, KEY, "private-sentinel"] {
            assert!(
                !text.contains(private),
                "private configuration was disclosed"
            );
        }
    }
}

fn dash(encryption: &str) -> Value {
    json!({"id":"dash","output_format":"dash","encryption":encryption,"drms":[]})
}

fn cenc_pssh() -> String {
    let mut payload = vec![0x12, 0x10];
    for byte in KID.as_bytes().as_chunks::<2>().0 {
        payload.push(u8::from_str_radix(std::str::from_utf8(byte).unwrap(), 16).unwrap());
    }
    payload.push(0x48);
    let mut scheme = u32::from_be_bytes(*b"cenc");
    while scheme > 127 {
        payload.push(u8::try_from(scheme & 127).unwrap() | 128);
        scheme >>= 7;
    }
    payload.push(u8::try_from(scheme).unwrap());
    let bytes =
        drmr_core::playready::build_pssh_box(&drmr_core::playready::WIDEVINE_SYSTEM_ID, &payload)
            .unwrap();
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        write!(encoded, "{byte:02x}").unwrap();
    }
    encoded
}

#[test]
fn missing_encrypted_output_key_is_refused_before_url_acquisition() {
    assert_refused_before_acquisition(
        &dash("cenc"),
        &json!({}),
        "execution-plan-output-keys-mismatch",
    );
}

#[test]
fn unused_output_key_id_is_refused_before_url_acquisition() {
    assert_refused_before_acquisition(
        &dash("cenc"),
        &json!({"dash":named_key(),PRIVATE_ID:named_key()}),
        "execution-plan-output-keys-mismatch",
    );
}

#[test]
fn clear_output_key_is_refused_before_url_acquisition() {
    assert_refused_before_acquisition(
        &dash("clear"),
        &json!({"dash":named_key()}),
        "execution-plan-output-keys-mismatch",
    );
}

#[test]
fn output_drm_mismatch_is_refused_before_url_acquisition() {
    let mut key = named_key();
    key["hls_key_uri"] = PRIVATE_URI.into();
    assert_refused_before_acquisition(
        &json!({"id":"hls","output_format":"hls-fmp4","encryption":"cbcs","drms":["fairplay"]}),
        &json!({"hls":key}),
        "execution-plan-drm-mismatch",
    );
}

#[test]
fn output_key_role_mismatch_is_refused_before_url_acquisition() {
    assert_refused_before_acquisition(
        &dash("cenc"),
        &json!({"dash":{"key":KEY,"hls_key_uri":PRIVATE_URI}}),
        "output-key-role-mismatch",
    );
}

#[test]
fn sample_aes_output_without_iv_is_refused_before_url_acquisition() {
    assert_refused_before_acquisition(
        &json!({"id":"hls","output_format":"hls-ts","encryption":"hls-sample-aes","drms":["fairplay"]}),
        &json!({"hls":{
            "key":KEY,
            "hls_key_uri":PRIVATE_URI,
            "signaling":{"system":"multi-drm","fairplay":true}
        }}),
        "hls-sample-output-iv-required",
    );
}

#[test]
fn aes128_output_with_unused_iv_is_refused_before_url_acquisition() {
    assert_refused_before_acquisition(
        &json!({"id":"hls","output_format":"hls-ts","encryption":"hls-aes128","drms":[]}),
        &json!({"hls":{
            "key":KEY,
            "hls_key_uri":PRIVATE_URI,
            "iv":"0123456789abcdeffedcba9876543210"
        }}),
        "output-iv-not-used",
    );
}

#[test]
fn sample_encrypted_hls_without_uri_is_refused_before_url_acquisition() {
    assert_refused_before_acquisition(
        &json!({"id":"hls","output_format":"hls-fmp4","encryption":"cenc","drms":[]}),
        &json!({"hls":named_key()}),
        "hls-key-uri-required",
    );
}

#[test]
fn dash_only_signaling_for_hls_is_refused_before_url_acquisition() {
    let mut key = named_key();
    key["signaling"] = json!({
        "system":"playready",
        "license_url":"https://license.example.test/private-sentinel"
    });
    assert_refused_before_acquisition(
        &json!({"id":"hls","output_format":"hls-fmp4","encryption":"cenc","drms":["playready"]}),
        &json!({"hls":key}),
        "output-signaling-profile-not-supported",
    );
}

#[test]
fn widevine_pssh_scheme_mismatch_is_refused_before_url_acquisition() {
    let mut key = named_key();
    key["signaling"] = json!({"system":"multi-drm","widevine_pssh":cenc_pssh()});
    assert_refused_before_acquisition(
        &json!({"id":"dash","output_format":"dash","encryption":"cbcs","drms":["widevine"]}),
        &json!({"dash":&key}),
        "output-signaling-invalid",
    );
    for format in [Some("dash"), None] {
        assert_quick_refused_before_acquisition(
            format,
            "cbcs",
            Some(&key),
            "output-signaling-invalid",
        );
    }
}

#[test]
fn quick_known_format_without_output_key_is_refused_before_url_acquisition() {
    assert_quick_refused_before_acquisition(Some("dash"), "cbcs", None, "output-key-role-mismatch");
}

#[test]
fn quick_inferred_format_without_output_key_is_refused_before_url_acquisition() {
    assert_quick_refused_before_acquisition(None, "cbcs", None, "output-key-role-mismatch");
}

#[test]
fn quick_aes128_output_with_unused_iv_is_refused_before_url_acquisition() {
    assert_quick_refused_before_acquisition(
        Some("hls-ts"),
        "hls-aes128",
        Some(&json!({
            "key":KEY,
            "hls_key_uri":PRIVATE_URI,
            "iv":"0123456789abcdeffedcba9876543210"
        })),
        "output-iv-not-used",
    );
}
