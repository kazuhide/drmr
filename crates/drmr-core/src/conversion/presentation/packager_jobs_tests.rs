use super::*;
use crate::{
    events::NoSink,
    resources::{Limiter, Workload},
    toolrun::{DEFAULT_STDERR_BYTES, DEFAULT_STDOUT_BYTES, Secrets, ToolRunner},
};
use std::sync::Arc;

fn fixture(parent: &Path, maximum: usize) -> (Converter, PathBuf, PathBuf) {
    let arguments = parent.join("arguments.txt");
    let tool = drmr_test_support::install(
        &parent.join("packager"),
        &drmr_test_support::ToolSpec::default().with("argv_file", arguments.to_str().unwrap()),
    )
    .unwrap();
    let workload = Workload {
        max_pixels: Some(0),
        input_buffer_bytes: Some(0),
        tracks: 3,
    };
    let runner = ToolRunner::new(Arc::new(Secrets::new()), Arc::new(NoSink)).with_limiter(
        Arc::new(Limiter::new(
            4,
            4 * workload.estimated_child_bytes(DEFAULT_STDOUT_BYTES, DEFAULT_STDERR_BYTES),
        )),
    );
    runner.register_workload(workload);
    let converter = Converter::new(Arc::new(runner), tool.to_str().unwrap(), "ffmpeg")
        .with_max_workers(Some(maximum))
        .unwrap();
    (converter, tool, arguments)
}

#[test]
fn final_joint_call_uses_the_serial_job_manager_at_every_worker_limit() {
    for (maximum, paths, parallel) in [
        (4, vec!["video.mp4", "audio.mp4"], false),
        (4, vec!["video.mp4", "./video.mp4"], false),
        (4, vec!["video.mp4", "video.mp4"], false),
        (1, vec!["video.mp4", "audio.mp4"], false),
        (1, vec!["video.mp4"], false),
    ] {
        let parent = crate::native::private_tempdir().unwrap();
        let (converter, tool, arguments) = fixture(parent.path(), maximum);
        let owner = super::super::files::Stage::new(Some(parent.path())).unwrap();
        let mut argv = vec![tool.to_str().unwrap().into()];
        argv.extend(
            paths
                .into_iter()
                .map(|path| format!("input={},stream=0", parent.path().join(path).display())),
        );
        converter
            .run_joint_packager(&argv, None, &[], &owner, None)
            .unwrap();
        let actual = fs::read_to_string(&arguments).unwrap();
        assert!(actual.lines().any(|value| value == "--io_cache_size=0"));
        assert_eq!(
            actual
                .lines()
                .any(|value| value == "--single_threaded=false"),
            parallel
        );
        assert_eq!(
            actual.lines().any(|value| value == "--single_threaded"),
            !parallel
        );
        converter.backend.run(&argv, "packager", &[]).unwrap();
        let ordinary = fs::read_to_string(&arguments).unwrap();
        assert!(ordinary.lines().any(|value| value == "--single_threaded"));
        assert!(
            !ordinary
                .lines()
                .any(|value| value == "--single_threaded=false")
        );
    }
}

#[test]
fn private_pssh_call_uses_the_serial_job_manager() {
    let parent = crate::native::private_tempdir().unwrap();
    let (converter, tool, arguments) = fixture(parent.path(), 4);
    let owner = super::super::files::Stage::new(Some(parent.path())).unwrap();
    converter
        .run_joint_packager(
            &[tool.to_str().unwrap().into()],
            Some("0000000870737368"),
            &[],
            &owner,
            None,
        )
        .unwrap();
    let actual = fs::read_to_string(arguments).unwrap();
    assert!(actual.lines().any(|value| value == "--single_threaded"));
    assert!(actual.lines().any(|value| value.starts_with("--flagfile=")));
}
