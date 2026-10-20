use super::*;
use crate::{
    events::NoSink,
    toolrun::{Secrets, ToolRunner},
};
use std::io::Write as _;

fn converter() -> Converter {
    Converter::new(
        Arc::new(ToolRunner::new(Arc::new(Secrets::new()), Arc::new(NoSink))),
        "unused",
        "unused",
    )
}

#[test]
fn planned_publication_keeps_no_replace_and_allows_private_intermediate_outputs() {
    let root = crate::native::private_tempdir().unwrap();
    let base = root.path().canonicalize().unwrap();
    let output = base.as_path().join("result");
    let command = converter().using_plan_destination(OutputGuard::new(&output).unwrap());
    let stage = files::Stage::new(Some(base.as_path())).unwrap();
    let candidate = stage.path().join("candidate");
    files::create_new(&candidate)
        .unwrap()
        .write_all(b"verified result")
        .unwrap();
    let intermediate = stage.path().join("intermediate.mp4");
    command.publish(&candidate, &intermediate).unwrap();
    assert!(!output.exists());
    command.publish(&intermediate, &output).unwrap();
    assert_eq!(std::fs::read(&output).unwrap(), b"verified result");
    assert!(command.publish(&candidate, &output).is_err());
    assert_eq!(std::fs::read(&output).unwrap(), b"verified result");
}

#[test]
fn measurement_clones_keep_the_original_destination_and_cancellation_guards() {
    let root = crate::native::private_tempdir().unwrap();
    let base = root.path().canonicalize().unwrap();
    let output = base.as_path().join("result");
    let stop = crate::concurrency::Stop::new();
    let command = converter()
        .with_stop(stop.clone())
        .using_plan_destination(OutputGuard::new(&output).unwrap());
    let stage = files::Stage::new(Some(base.as_path())).unwrap();
    let candidate = stage.path().join("candidate");
    files::create_new(&candidate)
        .unwrap()
        .write_all(b"verified result")
        .unwrap();
    stop.set();
    let (result, _) = command.measure(|measured| measured.publish(&candidate, &output));
    assert_eq!(result.unwrap_err().0, "cancelled");
    assert!(!output.exists());
}

#[test]
#[cfg(unix)]
fn redirected_final_destination_cannot_be_treated_as_a_private_intermediate() {
    use std::os::unix::fs::symlink;
    let root = crate::native::private_tempdir().unwrap();
    let base = root.path().canonicalize().unwrap();
    let source = base.as_path().join("source");
    let work = base.as_path().join("work");
    std::fs::create_dir(&source).unwrap();
    std::fs::create_dir(source.join("out")).unwrap();
    std::fs::create_dir(&work).unwrap();
    std::fs::create_dir(work.join("out")).unwrap();
    std::fs::write(source.join("original.ts"), b"original media").unwrap();
    let output = work.join("out/result");
    let command = converter().using_plan_destination(OutputGuard::new(&output).unwrap());
    let stage = files::Stage::new(Some(base.as_path())).unwrap();
    let candidate = stage.path().join("candidate");
    files::create_new(&candidate)
        .unwrap()
        .write_all(b"verified result")
        .unwrap();
    std::fs::rename(&work, base.as_path().join("previous-work")).unwrap();
    symlink(&source, &work).unwrap();
    let redirected = source.join("out/result");
    let (result, _) = command.measure(|measured| measured.publish(&candidate, &redirected));
    assert_eq!(result.unwrap_err(), UNSAFE);
    assert!(!redirected.exists());
    assert_eq!(
        std::fs::read(source.join("original.ts")).unwrap(),
        b"original media"
    );
    assert_eq!(std::fs::read(&candidate).unwrap(), b"verified result");
}

#[test]
#[cfg(unix)]
fn late_parent_substitution_is_refused_after_directory_preparation() {
    use std::os::unix::fs::symlink;
    let root = crate::native::private_tempdir().unwrap();
    let base = root.path().canonicalize().unwrap();
    let source = base.as_path().join("source");
    let work = base.as_path().join("work");
    std::fs::create_dir(&source).unwrap();
    std::fs::create_dir(source.join("out")).unwrap();
    std::fs::create_dir(&work).unwrap();
    std::fs::create_dir(work.join("out")).unwrap();
    let output = work.join("out/result");
    let command = converter().using_plan_destination(OutputGuard::new(&output).unwrap());
    let stage = files::Stage::new(Some(base.as_path())).unwrap();
    files::create_new(&stage.path().join("media"))
        .unwrap()
        .write_all(b"verified result")
        .unwrap();
    let result = files::publish_directory_committing(
        stage.path(),
        &output,
        None,
        || {
            std::fs::rename(&work, base.as_path().join("previous-work")).unwrap();
            symlink(&source, &work).unwrap();
            Ok(())
        },
        |source, destination| command.commit_publication(source, destination),
    );
    assert_eq!(result.unwrap_err(), UNSAFE);
    assert!(!source.join("out/result").exists());
    assert!(stage.path().join("media").exists());
}

#[test]
#[cfg(windows)]
fn destination_ancestors_are_pinned_against_rename_until_the_guard_is_released() {
    let root = crate::native::private_tempdir().unwrap();
    let base = root.path().canonicalize().unwrap();
    let work = base.as_path().join("work");
    crate::native::create_private_dir_new(&work).unwrap();
    let guard = OutputGuard::new(&work.join("result")).unwrap();
    assert!(std::fs::rename(&work, base.as_path().join("renamed")).is_err());
    guard.check().unwrap();
    drop(guard);
    std::fs::rename(&work, base.as_path().join("renamed")).unwrap();
}
