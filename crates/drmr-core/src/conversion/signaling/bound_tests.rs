use super::*;
use crate::{
    conversion::{Converter, key_file::OperatorKeys},
    events::Collector,
    toolrun::{Secrets, ToolRunner},
};
use std::{io::Write as _, path::PathBuf, sync::Arc};

fn command(records: Arc<Collector>) -> Converter {
    Converter::new(
        Arc::new(ToolRunner::new(Arc::new(Secrets::new()), records)),
        "must-not-start-packager",
        "must-not-start-decoder",
    )
}

fn fixture(root: &Path) -> PathBuf {
    let path = root.join("output.mp4");
    std::fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../drmr/tests/fixtures/bbb/clip.mp4"),
        &path,
    )
    .unwrap();
    path
}

fn budget_events(records: &Collector) -> usize {
    records
        .records()
        .iter()
        .filter(|record| record["stage"] == "resources" && record["message"] == "resource-budget")
        .count()
}

#[test]
fn bound_signaling_does_not_admit_another_source_copy() {
    let owner = crate::native::private_tempdir().unwrap();
    let path = fixture(owner.path());
    let records = Arc::new(Collector::default());
    let command = command(records.clone());
    let frozen = command.capture_source(&path).unwrap();
    let before = budget_events(&records);
    command
        .verify_bound_output_signaling(
            &frozen,
            OutputFormat::Mp4,
            TargetProtection::Clear,
            &OperatorKeys::default(),
        )
        .unwrap();
    assert_eq!(budget_events(&records), before);
    command
        .verify_output_signaling(
            &path,
            OutputFormat::Mp4,
            TargetProtection::Clear,
            &OperatorKeys::default(),
        )
        .unwrap();
    assert!(budget_events(&records) > before);
}

#[test]
fn bound_signaling_rejects_changes_to_the_actual_output_after_capture() {
    let owner = crate::native::private_tempdir().unwrap();
    let path = fixture(owner.path());
    let command = command(Arc::new(Collector::default()));
    let frozen = command.capture_source(&path).unwrap();
    let unexpected = playready::build_pssh_box(
        &playready::WIDEVINE_SYSTEM_ID,
        b"unexpected provider record",
    )
    .unwrap();
    std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap()
        .write_all(&unexpected)
        .unwrap();
    assert_eq!(
        command.verify_bound_output_signaling(
            &frozen,
            OutputFormat::Mp4,
            TargetProtection::Clear,
            &OperatorKeys::default(),
        ),
        Err(MISMATCH)
    );
    assert_eq!(
        command.verify_output_signaling(
            &path,
            OutputFormat::Mp4,
            TargetProtection::Clear,
            &OperatorKeys::default(),
        ),
        Err(MISMATCH)
    );
}

#[test]
fn bound_and_fresh_signaling_reject_unrequested_provider_records() {
    let owner = crate::native::private_tempdir().unwrap();
    let path = fixture(owner.path());
    let unexpected = playready::build_pssh_box(
        &playready::WIDEVINE_SYSTEM_ID,
        b"unexpected provider record",
    )
    .unwrap();
    std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap()
        .write_all(&unexpected)
        .unwrap();
    let command = command(Arc::new(Collector::default()));
    let frozen = command.capture_source(&path).unwrap();
    for target in [
        TargetProtection::Clear,
        TargetProtection::Cenc,
        TargetProtection::Cbcs,
    ] {
        let bound = command.verify_bound_output_signaling(
            &frozen,
            OutputFormat::Mp4,
            target,
            &OperatorKeys::default(),
        );
        assert_eq!(bound, Err(MISMATCH));
        assert_eq!(
            bound,
            command.verify_output_signaling(
                &path,
                OutputFormat::Mp4,
                target,
                &OperatorKeys::default()
            )
        );
    }
}
