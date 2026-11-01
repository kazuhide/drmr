//! Reuse independent packet/crypto oracles for real Main and Main 10 bitstreams.
use super::*;

fn fixtures() -> impl Iterator<Item = std::path::PathBuf> {
    ["hevc-main.mp4", "hevc-main10.mp4"]
        .into_iter()
        .map(|name| {
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../drmr/tests/fixtures/bbb")
                .join(name)
        })
}

#[test]
fn real_hevc_progressive_sample_hashes_and_raw_clocks_match_independent_ffprobe() {
    for fixture in fixtures() {
        super::tests::compare(&fixture, false);
    }
}

#[test]
fn independent_shaka_hevc_cenc_and_cbcs_reencrypt_to_every_supported_scheme() {
    for fixture in fixtures() {
        super::segment_tests::assert_shaka_encrypted_resources(&fixture, &["video"]);
    }
}
