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
fn final_joint_call_uses_unique_input_jobs_and_keeps_other_packager_calls_serial() {
    for (maximum, paths, parallel) in [
        (4, vec!["video.mp4", "audio.mp4"], true),
        (4, vec!["video.mp4", "./video.mp4"], true),
        (4, vec!["video.mp4", "video.mp4"], false),
        (1, vec!["video.mp4", "audio.mp4"], false),
        (1, vec!["video.mp4"], false),
    ] {
        let parent = crate::native::private_tempdir().unwrap();
        let (converter, tool, arguments) = fixture(parent.path(), maximum);
        let owner = super::super::files::Stage::new(Some(parent.path())).unwrap();
        let clear: Vec<_> = paths
            .into_iter()
            .map(|path| parent.path().join(path))
            .collect();
        let argv = [tool.to_str().unwrap().into()];
        converter
            .run_joint_packager(&clear, &argv, None, &[], &owner, None)
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
fn private_pssh_call_keeps_the_same_parallel_resource_scope() {
    let parent = crate::native::private_tempdir().unwrap();
    let (converter, tool, arguments) = fixture(parent.path(), 4);
    let owner = super::super::files::Stage::new(Some(parent.path())).unwrap();
    let clear = [
        parent.path().join("video.mp4"),
        parent.path().join("audio.mp4"),
    ];
    converter
        .run_joint_packager(
            &clear,
            &[tool.to_str().unwrap().into()],
            Some("0000000870737368"),
            &[],
            &owner,
            None,
        )
        .unwrap();
    let actual = fs::read_to_string(arguments).unwrap();
    assert!(
        actual
            .lines()
            .any(|value| value == "--single_threaded=false")
    );
    assert!(actual.lines().any(|value| value.starts_with("--flagfile=")));
}
