//! Strict, private operator configuration for independent output domains.
use drmr_core::conversion::{DrmSystem, OutputSignaling, key_file};
use std::{io::Write as _, path::PathBuf};

const KID: &str = "000102030405060708090a0b0c0d0e0f";
const KEY: &str = "101112131415161718191a1b1c1d1e1f";
const LICENSE: &str = "https://example.invalid/license?tenant=SYNTHETIC-PRIVATE";
const URI: &str = "skd://synthetic-content";

fn pssh() -> String {
    let payload = hex::decode(format!("1210{KID}")).unwrap();
    hex::encode(
        drmr_core::playready::build_pssh_box(&drmr_core::playready::WIDEVINE_SYSTEM_ID, &payload)
            .unwrap(),
    )
}
fn config(output: &serde_json::Value) -> serde_json::Value {
    serde_json::json!({"schema_version":2,"input":{"keys":[]},"outputs":output})
}
fn write(root: &std::path::Path, text: &str) -> PathBuf {
    let path = root.join("operator.json");
    drmr_core::native::create_private_new(&path)
        .unwrap()
        .write_all(text.as_bytes())
        .unwrap();
    path
}
#[test]
fn drm_systems_have_stable_nonsecret_identifiers() {
    assert_eq!(
        serde_json::to_value([
            DrmSystem::Widevine,
            DrmSystem::PlayReady,
            DrmSystem::FairPlay
        ])
        .unwrap(),
        serde_json::json!(["widevine", "playready", "fairplay"])
    );
    assert!(serde_json::from_str::<DrmSystem>("\"unknown\"").is_err());
    let value = OutputSignaling::multi_drm(Some(&pssh()), Some(LICENSE), true).unwrap();
    assert_eq!(
        value.systems(),
        [
            DrmSystem::Widevine,
            DrmSystem::PlayReady,
            DrmSystem::FairPlay
        ]
    );
    let debug = format!("{value:?}");
    for private in [LICENSE, pssh().as_str(), URI] {
        assert!(!debug.contains(private));
    }
    assert_eq!(
        OutputSignaling::playready(LICENSE).unwrap().systems(),
        [DrmSystem::PlayReady]
    );
}
#[test]
fn multi_drm_requires_a_selected_system_and_a_full_widevine_box() {
    assert!(OutputSignaling::multi_drm(None, None, false).is_err());
    assert!(OutputSignaling::multi_drm(Some(&format!("1210{KID}")), None, false).is_err());
    for mutation in [0, 4, 8, 9, 12, 28] {
        let mut bytes = hex::decode(pssh()).unwrap();
        bytes[mutation] ^= 0x7f;
        assert!(OutputSignaling::multi_drm(Some(&hex::encode(bytes)), None, false).is_err());
    }
    assert!(OutputSignaling::multi_drm(None, Some("file:///private"), false).is_err());
}
#[test]
fn bundle_keys_keep_each_output_domain_separate() {
    let owner = drmr_core::native::private_tempdir().unwrap();
    let path = write(
        owner.path(),
        &config(&serde_json::json!({
            "dash": {"kid":KID,"key":KEY,"signaling":{
                "system":"multi-drm","widevine_pssh":pssh(),"playready_license_url":LICENSE}},
            "fairplay": {"key":KEY,"iv":"202122232425262728292a2b2c2d2e2f","hls_key_uri":URI,
                "signaling":{"system":"multi-drm","fairplay":true}}
        }))
        .to_string(),
    );
    let keys = key_file::read_bundle(&path).unwrap();
    assert_eq!(keys.outputs.len(), 2);
    assert!(keys.outputs["dash"].output.is_some());
    assert!(keys.outputs["dash"].hls_output.is_none());
    assert!(keys.outputs["fairplay"].output.is_none());
    assert_eq!(keys.outputs["fairplay"].hls_output_uri(), Some(URI));
    assert_eq!(
        keys.outputs["fairplay"]
            .signaling
            .as_ref()
            .unwrap()
            .systems(),
        [DrmSystem::FairPlay]
    );
    let debug = format!("{:?}", keys.outputs);
    for private in [KEY, LICENSE, URI, pssh().as_str()] {
        assert!(!debug.contains(private));
    }
    assert!(key_file::read(&path).is_err());
}
#[test]
fn bundle_schema_rejects_duplicate_outputs_and_unknown_fields() {
    for value in [
        format!("{{\"schema_version\":2,\"outputs\":{{\"x\":{{\"kid\":\"{KID}\",\"key\":\"{KEY}\"}},\"x\":{{\"kid\":\"{KID}\",\"key\":\"{KEY}\"}}}}}}"),
        config(&serde_json::json!({"dash":{"kid":KID,"key":KEY,"unexpected":true}})).to_string(),
        config(&serde_json::json!({"dash":{"kid":KID,"key":KEY,"signaling":{"system":"multi-drm","fairplay":true,"license_url":LICENSE}}})).to_string(),
        config(&serde_json::json!({"../dash":{"kid":KID,"key":KEY}})).to_string(),
        serde_json::json!({"schema_version":1,"outputs":{}}).to_string(),
    ] {
        let owner = drmr_core::native::private_tempdir().unwrap();
        let path = write(owner.path(), &value);
        assert_eq!(key_file::read_bundle(&path).unwrap_err().0, "key-file-invalid");
    }
}
#[test]
fn legacy_key_files_retain_the_original_profile() {
    let owner = drmr_core::native::private_tempdir().unwrap();
    let path = write(
        owner.path(),
        &serde_json::json!({"schema_version":1,"output":{
        "kid":KID,"key":KEY,"signaling":{"system":"playready","license_url":LICENSE}}})
        .to_string(),
    );
    let keys = key_file::read(&path).unwrap();
    assert_eq!(keys.signaling.unwrap().systems(), [DrmSystem::PlayReady]);
    assert!(key_file::read_bundle(&path).is_err());
}
