//! Independent sample/decryption oracles cover every committed VP9 profile/depth pair.
use super::*;

fn fixtures() -> impl Iterator<Item = std::path::PathBuf> {
    [
        "vp9-p0-8.mp4",
        "vp9-p1-8.mp4",
        "vp9-p2-10.mp4",
        "vp9-p2-12.mp4",
        "vp9-p3-10.mp4",
        "vp9-p3-12.mp4",
    ]
    .into_iter()
    .map(|name| {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../drmr/tests/fixtures/bbb")
            .join(name)
    })
}

#[test]
fn real_vp9_sample_hashes_and_raw_clocks_match_independent_ffprobe() {
    for fixture in fixtures() {
        super::tests::compare(&fixture, false);
    }
}

#[test]
fn independent_shaka_vp9_cenc_resources_decrypt_and_rekey_without_losing_altrefs() {
    for fixture in fixtures() {
        // VP9's standard writer adds alignment residue before protected bytes.
        // Shaka keeps trailing residue clear, so its subsample map is not an
        // equality oracle; every independently decrypted packet and clock is.
        super::segment_tests::assert_shaka_resource_routes(
            &fixture,
            &["video"],
            &["cenc"],
            &[
                crate::inspection::TargetProtection::Clear,
                crate::inspection::TargetProtection::Cenc,
            ],
            false,
        );
    }
}
