//! TS DASH candidates remain conditional metadata plans until original media proof completes.
use drmr_core::inspection::{self, OutputFormat, TargetProtection};
#[test]
fn declared_ts_protection_offers_dash_with_original_clock_revalidation() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("input.m3u8");
    for method in [
        None,
        Some("AES-128"),
        Some("SAMPLE-AES"),
        Some("SAMPLE-AES-CTR"),
    ] {
        let key = method.map_or_else(String::new, |method| {
            format!("#EXT-X-KEY:METHOD={method},URI=\"not-read\"\n")
        });
        std::fs::write(
            &path,
            format!("#EXTM3U\n{key}#EXTINF:2,\nnot-read.ts\n#EXT-X-ENDLIST\n"),
        )
        .unwrap();
        for target in [
            TargetProtection::Clear,
            TargetProtection::Cenc,
            TargetProtection::Cbcs,
        ] {
            let plan = inspection::plan(
                inspection::inspect(&path).unwrap(),
                target,
                vec![OutputFormat::Dash],
            );
            if method == Some("SAMPLE-AES-CTR") {
                assert!(!plan.compatible);
                continue;
            }
            assert!(plan.compatible && plan.execution_available);
            assert!(!plan.input.media_verified);
            let processing = plan.processing.unwrap();
            assert_eq!(processing.status, inspection::PlanningStatus::MetadataOnly);
            let candidate = processing
                .candidates
                .iter()
                .find(|c| c.route.as_str() == "mpeg-ts-dash")
                .unwrap();
            assert_eq!(candidate.output_format, OutputFormat::Dash);
            assert!(candidate.revalidation_required);
            assert_eq!(
                candidate.condition_codes,
                [
                    "local-transport-stream-profile-and-absolute-timeline-must-pass",
                    "additional-complete-decoding-requires-ffmpeg-and-ffprobe"
                ]
            );
            assert_eq!(
                candidate.required_tools,
                [inspection::RequiredTool::ShakaPackager]
            );
        }
    }
    assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 1);
}
