//! Compare the streaming proof with `FFmpeg`'s complete decoded SHA-256 output.
use super::*;
use crate::conversion::keys::{KeyedArguments, ProbeKeys};
use sha2::{Digest as _, Sha256};

fn backend() -> Backend {
    crate::conversion::Converter::new(
        Arc::new(ToolRunner::new(
            Arc::new(crate::toolrun::Secrets::new()),
            Arc::new(crate::events::NoSink),
        )),
        "packager",
        "ffmpeg",
    )
    .with_timeout(Duration::from_secs(30))
    .backend
}

fn input(
    backend: &Backend,
    path: &Path,
    index: usize,
    keys: Option<ProbeKeys<'_>>,
    unedited: bool,
) -> KeyedArguments {
    let mut argv = vec![backend.ffmpeg.clone()];
    if unedited {
        argv.extend(["-ignore_editlist".into(), "1".into()]);
    }
    argv.extend([
        "-v".into(),
        "error".into(),
        "-nostdin".into(),
        "-filter_threads".into(),
        "1".into(),
        "-filter_complex_threads".into(),
        "1".into(),
        "-xerror".into(),
        "-threads".into(),
        "1".into(),
        "-protocol_whitelist".into(),
        "file,crypto".into(),
        "-i".into(),
        super::super::files::tool_path(path).unwrap(),
        "-map".into(),
        format!("0:{index}"),
    ]);
    KeyedArguments::new(argv, keys)
}

fn reference(backend: &Backend, mut keyed: KeyedArguments) -> Result<String, ConversionError> {
    // Retain the previous command, including implicit rawvideo/PCM-S16 defaults.
    keyed.argv.extend(
        [
            "-threads",
            "1",
            "-fps_mode",
            "passthrough",
            "-f",
            "hash",
            "-hash",
            "sha256",
            "-",
        ]
        .map(String::from),
    );
    let output = backend.run(&keyed.argv, "ffmpeg", &keyed.secrets())?;
    Ok(String::from_utf8(output)
        .unwrap()
        .trim()
        .strip_prefix("SHA256=")
        .unwrap()
        .to_ascii_lowercase())
}

fn generate(backend: &Backend, arguments: &[&str], output: &Path) {
    let mut argv = vec![
        backend.ffmpeg.clone(),
        "-v".into(),
        "error".into(),
        "-nostdin".into(),
    ];
    argv.extend(arguments.iter().map(|value| (*value).to_owned()));
    argv.push(super::super::files::tool_path(output).unwrap());
    backend.run(&argv, "ffmpeg", &[]).unwrap();
}

#[test]
fn complete_bbb_video_audio_and_edit_list_modes_keep_the_previous_digest() {
    let backend = backend();
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../drmr/tests/fixtures/bbb/clip.mp4");
    let original = Sha256::digest(std::fs::read(&path).unwrap());
    for index in [0, 1] {
        for unedited in [false, true] {
            let expected =
                reference(&backend, input(&backend, &path, index, None, unedited)).unwrap();
            let actual = backend
                .hash_stream_options(&path, index, false, None, unedited)
                .unwrap();
            assert_eq!(actual, expected, "track {index}, unedited {unedited}");
        }
    }
    assert_eq!(Sha256::digest(std::fs::read(path).unwrap()), original);
}

#[test]
fn decoded_aac_rates_and_channel_layouts_keep_pcm_s16le_digests() {
    let backend = backend();
    let root = tempfile::tempdir().unwrap();
    for rate in [44_100, 48_000] {
        for channels in [1, 2, 6] {
            let path = root.path().join(format!("{rate}-{channels}.mp4"));
            generate(
                &backend,
                &[
                    "-f",
                    "lavfi",
                    "-i",
                    &format!("sine=frequency=440:sample_rate={rate}:duration=0.3"),
                    "-ac",
                    &channels.to_string(),
                    "-threads",
                    "1",
                    "-c:a",
                    "aac",
                ],
                &path,
            );
            assert_eq!(
                backend.hash_stream(&path, 0, false, None).unwrap(),
                reference(&backend, input(&backend, &path, 0, None, false)).unwrap(),
                "rate {rate}, channels {channels}"
            );
        }
    }
}

#[test]
fn decoded_pixels_keep_high_precision_and_rgb_formats() {
    let backend = backend();
    let root = tempfile::tempdir().unwrap();
    for pixel_format in ["yuv420p", "yuv420p10le", "rgb24"] {
        let path = root.path().join(format!("{pixel_format}.nut"));
        generate(
            &backend,
            &[
                "-f",
                "lavfi",
                "-i",
                "testsrc2=duration=1:size=32x18:rate=3",
                "-threads",
                "1",
                "-c:v",
                "rawvideo",
                "-pix_fmt",
                pixel_format,
            ],
            &path,
        );
        assert_eq!(
            backend.hash_stream(&path, 0, false, None).unwrap(),
            reference(&backend, input(&backend, &path, 0, None, false)).unwrap(),
            "pixel format {pixel_format}"
        );
    }
}

#[test]
fn hash_muxer_keeps_duplicate_timestamp_acceptance_and_decreasing_timestamp_refusal() {
    let backend = backend();
    for (expression, accepted) in [("PTS", true), ("0", true), ("-N", false)] {
        let argv = vec![
            backend.ffmpeg.clone(),
            "-v".into(),
            "error".into(),
            "-nostdin".into(),
            "-xerror".into(),
            "-threads".into(),
            "1".into(),
            "-filter_threads".into(),
            "1".into(),
            "-filter_complex_threads".into(),
            "1".into(),
            "-f".into(),
            "lavfi".into(),
            "-i".into(),
            format!("testsrc2=duration=1:size=32x18:rate=3,setpts={expression}"),
            "-map".into(),
            "0:0".into(),
        ];
        let expected = reference(&backend, KeyedArguments::new(argv.clone(), None));
        let actual = backend.hash_decoded_stream(KeyedArguments::new(argv, None), 0);
        assert_eq!(expected.is_ok(), accepted, "reference {expression}");
        assert_eq!(actual, expected, "candidate {expression}");
    }
}
