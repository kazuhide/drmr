//! The admitted metadata bound must also be safe for diagnostic redaction.
use drmr_core::{
    conversion::OutputSignaling,
    events::Collector,
    toolrun::{REDACTED, RunOptions, Secrets, ToolRunner},
};
use std::sync::Arc;

fn largest_pssh() -> Vec<u8> {
    let mut payload = hex::decode("1210000102030405060708090a0b0c0d0e0f").unwrap();
    payload.push(0x1a);
    let mut size = 65_482u32;
    while size >= 128 {
        payload.push(u8::try_from(size & 127).unwrap() | 128);
        size >>= 7;
    }
    payload.push(u8::try_from(size).unwrap());
    payload.resize(65_504, b'a');
    let bytes =
        drmr_core::playready::build_pssh_box(&drmr_core::playready::WIDEVINE_SYSTEM_ID, &payload)
            .unwrap();
    assert_eq!(bytes.len(), 65_536);
    bytes
}
#[test]
fn maximum_admitted_pssh_has_bounded_panic_free_private_redaction() {
    let bytes = largest_pssh();
    let secret = hex::encode(&bytes);
    OutputSignaling::multi_drm(Some(&secret), None, false).unwrap();
    let secrets = Secrets::with([secret.as_str()]);
    assert_eq!(
        secrets.scrub("ordinary unrelated diagnostic"),
        "ordinary unrelated diagnostic"
    );
    assert_eq!(secrets.scrub(&secret), REDACTED);
    assert_eq!(secrets.scrub(&secret.to_uppercase()), REDACTED);
    let mut different = secret.clone();
    different.replace_range(80..81, if &secret[80..81] == "0" { "1" } else { "0" });
    assert_eq!(secrets.scrub(&different), different);
    assert_eq!(
        secrets.scrub(&format!("before {secret} after")),
        format!("before {REDACTED} after")
    );
    let separated = secret
        .as_bytes()
        .chunks(2)
        .map(|pair| std::str::from_utf8(pair).unwrap())
        .collect::<Vec<_>>()
        .join(" : 0x");
    assert_eq!(secrets.scrub(&separated), REDACTED);
    let encoded = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, bytes);
    assert_eq!(secrets.scrub(&encoded), REDACTED);
}

#[test]
fn maximum_pssh_per_call_masks_tool_output_without_a_registry_or_provider_request() {
    let owner = drmr_core::native::private_tempdir().unwrap();
    let secret = hex::encode(largest_pssh());
    let tool = drmr_test_support::install(
        &owner.path().join("response"),
        &drmr_test_support::ToolSpec {
            stdout: secret.as_bytes().to_vec(),
            stderr: secret.to_uppercase().into_bytes(),
            ..drmr_test_support::ToolSpec::default()
        },
    )
    .unwrap();
    let events = Arc::new(Collector::default());
    let runner = ToolRunner::new(Arc::new(Secrets::new()), events.clone());
    let result = runner
        .run_tool(
            &[tool.to_str().unwrap().to_owned()],
            "synthetic-metadata-response",
            &RunOptions {
                secrets: &[&secret],
                ..RunOptions::default()
            },
        )
        .unwrap();
    assert_eq!(result.returncode, Some(0));
    let rendered = serde_json::to_string(&events.records()).unwrap();
    assert!(rendered.contains(REDACTED));
    assert!(!rendered.contains(&secret));
    assert!(!rendered.contains(&secret.to_uppercase()));
}
