//! Finite playback windows survive the actual CLI and a fresh verification.
#[path = "support/presentation_cli.rs"]
mod presentation_cli;
use presentation_cli::{configuration, convert_and_verify, fixture};
use serde_json::json;

#[test]
fn finite_original_bbb_clear_cenc_and_cbcs_inputs_convert_to_all_nine_outputs() {
    let owner = tempfile::tempdir().unwrap();
    let root = owner.path().join("private");
    drmr_core::native::create_private_dir_all(&root).unwrap();
    let mut results = Vec::new();
    for (name, protection) in [
        ("clip.mp4", "clear"),
        ("cenc-iv16.mp4", "cenc"),
        ("cbcs-joint.mp4", "cbcs"),
    ] {
        let encrypted = protection != "clear";
        let source = fixture(name);
        let source_bytes = std::fs::read(&source).unwrap();
        for (format, target) in [
            ("dash", "clear"),
            ("dash", "cenc"),
            ("dash", "cbcs"),
            ("hls", "clear"),
            ("hls", "cbcs"),
            ("hls", "hls-aes128"),
            ("mp4", "clear"),
            ("mp4", "cenc"),
            ("mp4", "cbcs"),
        ] {
            let label = format!("{name}-{format}-{target}");
            let config = root.join(format!("{label}.json"));
            let config_bytes = configuration(&config, encrypted, target, format);
            let destination = root.join(if format == "mp4" {
                format!("{label}.mp4")
            } else {
                label
            });
            let report = convert_and_verify(&source, &destination, &config, format, target);
            assert_eq!(report["output"]["tracks"].as_array().unwrap().len(), 2);
            assert_eq!(std::fs::read(&config).unwrap(), config_bytes);
            results.push(json!({
                "source_protection": protection,
                "format": format,
                "target": target,
                "verification": report["verification"],
                "fresh_verification": true,
            }));
        }
        assert_eq!(std::fs::read(&source).unwrap(), source_bytes);
    }
    assert_eq!(results.len(), 27);
    println!(
        "{}",
        json!({"fixture": "original-ten-second-bbb", "routes": results})
    );
}
