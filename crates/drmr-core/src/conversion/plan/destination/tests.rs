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
    command.publish(&stage, &candidate, &intermediate).unwrap();
    assert!(!output.exists());
    command.publish(&stage, &intermediate, &output).unwrap();
    assert_eq!(std::fs::read(&output).unwrap(), b"verified result");
    assert!(command.publish(&stage, &candidate, &output).is_err());
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
    let (result, _) = command.measure(|measured| measured.publish(&stage, &candidate, &output));
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
    let (result, _) = command.measure(|measured| measured.publish(&stage, &candidate, &redirected));
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
        &stage.directory_source().unwrap(),
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
#[cfg(unix)]
fn source_parent_alias_cannot_publish_a_foreign_file_after_the_final_destination_check() {
    let root = crate::native::private_tempdir().unwrap();
    let base = root.path().canonicalize().unwrap();
    let work = base.join("work");
    std::fs::create_dir(&work).unwrap();
    std::fs::create_dir(work.join("out")).unwrap();
    let output = work.join("out/result");
    let guard = OutputGuard::new(&output).unwrap();
    let stage = files::Stage::new(Some(&work)).unwrap();
    let candidate = stage.path().join("verified-output");
    let mut writer = files::create_new(&candidate).unwrap();
    writer.write_all(b"verified owned bytes").unwrap();
    let source = stage.created_file_source(&candidate, &writer).unwrap();
    drop(writer);
    let relative = candidate.strip_prefix(&work).unwrap().to_path_buf();
    let displaced = base.join("displaced-work");
    let foreign = base.join("foreign-work");
    let result = guard.commit_observed(&source, || {
        std::fs::rename(&work, &displaced).unwrap();
        std::fs::create_dir(&work).unwrap();
        std::fs::create_dir_all(candidate.parent().unwrap()).unwrap();
        std::fs::write(&candidate, b"foreign bytes must remain").unwrap();
    });
    // Restore the original namespace before Stage cleanup, so a final pathname
    // check cannot distinguish this substitution from an untouched candidate.
    std::fs::rename(&work, &foreign).unwrap();
    std::fs::rename(&displaced, &work).unwrap();
    drop(source);
    drop(stage);
    result.unwrap();
    assert_eq!(std::fs::read(&output).unwrap(), b"verified owned bytes");
    assert_eq!(
        std::fs::read(foreign.join(relative)).unwrap(),
        b"foreign bytes must remain"
    );
}

#[test]
#[cfg(unix)]
fn source_parent_alias_cannot_publish_a_foreign_directory_after_the_final_destination_check() {
    let root = crate::native::private_tempdir().unwrap();
    let base = root.path().canonicalize().unwrap();
    let work = base.join("work");
    std::fs::create_dir(&work).unwrap();
    std::fs::create_dir(work.join("out")).unwrap();
    let output = work.join("out/result");
    let guard = OutputGuard::new(&output).unwrap();
    let stage = files::Stage::new(Some(&work)).unwrap();
    files::create_new(&stage.path().join("verified"))
        .unwrap()
        .write_all(b"verified owned bytes")
        .unwrap();
    let relative = stage.path().strip_prefix(&work).unwrap().to_path_buf();
    let displaced = base.join("displaced-work");
    let foreign = base.join("foreign-work");
    let source = stage.directory_source().unwrap();
    let result = guard.commit_observed(&source, || {
        std::fs::rename(&work, &displaced).unwrap();
        std::fs::create_dir(&work).unwrap();
        std::fs::create_dir_all(stage.path()).unwrap();
        std::fs::write(
            stage.path().join("foreign-sentinel"),
            b"foreign bytes must remain",
        )
        .unwrap();
    });
    std::fs::rename(&work, &foreign).unwrap();
    std::fs::rename(&displaced, &work).unwrap();
    drop(source);
    drop(stage);
    result.unwrap();
    assert_eq!(
        std::fs::read(output.join("verified")).unwrap(),
        b"verified owned bytes"
    );
    assert!(!output.join("foreign-sentinel").exists());
    assert_eq!(
        std::fs::read(foreign.join(relative).join("foreign-sentinel")).unwrap(),
        b"foreign bytes must remain"
    );
}

#[test]
#[cfg(unix)]
fn a_directory_source_uses_the_media_identity_from_stage_creation() {
    let root = crate::native::private_tempdir().unwrap();
    let stage = files::Stage::new(Some(root.path())).unwrap();
    let original = root.path().join("original-media");
    let foreign = root.path().join("foreign-media");
    std::fs::rename(stage.path(), &original).unwrap();
    files::create_dir(stage.path()).unwrap();
    std::fs::write(stage.path().join("sentinel"), b"foreign directory").unwrap();
    let error = stage.directory_source().err().unwrap();
    std::fs::rename(stage.path(), &foreign).unwrap();
    std::fs::rename(&original, stage.path()).unwrap();
    drop(stage);
    assert_eq!(error.0, "output-exists-or-publish-failed");
    assert_eq!(
        std::fs::read(foreign.join("sentinel")).unwrap(),
        b"foreign directory"
    );
}

#[test]
#[cfg(unix)]
fn a_created_file_source_uses_the_original_writer_identity() {
    let root = crate::native::private_tempdir().unwrap();
    let stage = files::Stage::new(Some(root.path())).unwrap();
    let path = stage.path().join("verified-output");
    let original = root.path().join("original-file");
    let foreign = root.path().join("foreign-file");
    let mut writer = stage.create_file(&path).unwrap();
    writer.write_all(b"verified owned bytes").unwrap();
    std::fs::rename(&path, &original).unwrap();
    files::create_new(&path)
        .unwrap()
        .write_all(b"foreign bytes")
        .unwrap();
    let error = stage.created_file_source(&path, &writer).err().unwrap();
    std::fs::rename(&path, &foreign).unwrap();
    std::fs::rename(&original, &path).unwrap();
    drop(writer);
    drop(stage);
    assert_eq!(error.0, "output-exists-or-publish-failed");
    assert_eq!(std::fs::read(foreign).unwrap(), b"foreign bytes");
}

#[test]
#[cfg(unix)]
fn a_retained_file_receipt_refuses_a_replacement_entry_at_commit() {
    let root = crate::native::private_tempdir().unwrap();
    let base = root.path().canonicalize().unwrap();
    let output = base.join("result");
    let guard = OutputGuard::new(&output).unwrap();
    let stage = files::Stage::new(Some(&base)).unwrap();
    let path = stage.path().join("verified-output");
    let original = base.join("original-file");
    let foreign = base.join("foreign-file");
    let mut writer = stage.create_file(&path).unwrap();
    writer.write_all(b"verified owned bytes").unwrap();
    let source = stage.created_file_source(&path, &writer).unwrap();
    drop(writer);
    std::fs::rename(&path, &original).unwrap();
    files::create_new(&path)
        .unwrap()
        .write_all(b"foreign bytes")
        .unwrap();
    let error = guard.commit(&source).unwrap_err();
    std::fs::rename(&path, &foreign).unwrap();
    std::fs::rename(&original, &path).unwrap();
    drop(source);
    drop(stage);
    assert_eq!(error.0, "output-exists-or-publish-failed");
    assert!(!output.exists());
    assert_eq!(std::fs::read(foreign).unwrap(), b"foreign bytes");
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
