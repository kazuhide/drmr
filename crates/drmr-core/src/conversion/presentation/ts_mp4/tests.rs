use super::*;
use crate::{
    conversion::{finite, native_media},
    events::NoSink,
    toolrun::{Secrets, ToolRunner},
};
use sha2::{Digest as _, Sha256};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

fn fixture(root: &Path, tracks: usize) -> PathBuf {
    let clip = Path::new(env!("CARGO_MANIFEST_DIR")).join("../drmr/tests/fixtures/bbb/clip.mp4");
    let kinds: &[&str] = if tracks == 1 {
        &["audio"]
    } else {
        &["video", "audio"]
    };
    let parts: Vec<_> = (0..tracks)
        .map(|i| root.join(format!("part-{i}.mp4")))
        .collect();
    let mut command = std::process::Command::new("packager");
    command.args([
        "--single_threaded",
        "--mp4_reset_initial_composition_offset_to_zero=false",
    ]);
    for (kind, part) in kinds.iter().zip(&parts) {
        command.arg(format!(
            "input={},stream={kind},output={}",
            clip.display(),
            part.display()
        ));
    }
    command.args(["--clear_lead", "0", "--fragment_duration", "2"]);
    assert!(
        command.output().unwrap().status.success(),
        "licensed fragmented fixture generation failed"
    );
    if tracks == 1 {
        return parts[0].clone();
    }
    let source = root.join("source.mp4");
    finite::combine_parts(&parts, &source, &crate::concurrency::Stop::new()).unwrap();
    source
}

fn rebind_ids(path: &Path, ids: &[u32]) -> Vec<u8> {
    let mut bytes = std::fs::read(path).unwrap();
    for node in crate::mp4::walk(&crate::mp4::parse_boxes(&bytes).unwrap()) {
        let payload = node.header.payload(&bytes);
        let field = match node.kind() {
            kind if kind == *b"tkhd" => {
                if payload[0] == 0 {
                    12
                } else {
                    20
                }
            }
            kind if kind == *b"trex" || kind == *b"tfhd" => 4,
            _ => continue,
        };
        let old = u32::from_be_bytes(payload[field..field + 4].try_into().unwrap());
        let at = node.header.offset + node.header.header_size + field;
        bytes[at..at + 4].copy_from_slice(&ids[old as usize - 1].to_be_bytes());
    }
    std::fs::write(path, &bytes).unwrap();
    bytes
}

type SampleChunks = Vec<Vec<(native_media::SampleInfo, [u8; 32])>>;

fn sample_hashes(path: &Path) -> SampleChunks {
    let mut samples = vec![Vec::new(), Vec::new()];
    native_media::scan(
        path,
        &InputKeys::default(),
        &crate::concurrency::Stop::new(),
        &mut |track, info, bytes| {
            let mut info = info.clone();
            info.range = 0..0;
            samples[track.index].push((info, Sha256::digest(bytes).into()));
            Ok(())
        },
    )
    .unwrap();
    samples
}

fn converter() -> Converter {
    Converter::new(
        Arc::new(ToolRunner::new(Arc::new(Secrets::new()), Arc::new(NoSink))),
        "unused-packager",
        "unused-ffmpeg",
    )
}

#[test]
fn singleton_id_one_keeps_the_fast_path_and_other_ids_get_private_views() {
    let owner = crate::native::private_tempdir().unwrap();
    let source = fixture(owner.path(), 1);
    let directory = owner.path().join("views");
    crate::native::create_media_dir_all(&directory).unwrap();
    let command = converter();
    assert!(
        complete_track_views(
            &source,
            &directory,
            &command.backend,
            inspection::inspect(&source).unwrap().tracks.len()
        )
        .unwrap()
        .is_none()
    );
    assert_eq!(std::fs::read_dir(&directory).unwrap().count(), 0);
    let original = rebind_ids(&source, &[2]);
    let samples = sample_hashes(&source);
    let views = complete_track_views(
        &source,
        &directory,
        &command.backend,
        inspection::inspect(&source).unwrap().tracks.len(),
    )
    .unwrap()
    .expect("an admitted fragmented singleton with ID2 needs a private ID1 view");
    assert_eq!(views.len(), 1);
    let movie = native_media::inspect(&views[0], &command.backend.stop).unwrap();
    assert_eq!(movie.tracks[0].track_id, 1);
    assert_eq!(sample_hashes(&views[0]), samples);
    assert_eq!(std::fs::read(&source).unwrap(), original);
}

#[test]
fn nondense_joint_ids_get_independently_bound_private_views() {
    let owner = crate::native::private_tempdir().unwrap();
    let source = fixture(owner.path(), 2);
    let original = rebind_ids(&source, &[7, 42]);
    let samples = sample_hashes(&source);
    let directory = owner.path().join("views");
    crate::native::create_media_dir_all(&directory).unwrap();
    let command = converter();
    let views = complete_track_views(
        &source,
        &directory,
        &command.backend,
        inspection::inspect(&source).unwrap().tracks.len(),
    )
    .unwrap()
    .expect("nondense joint IDs need private views");
    assert_eq!(views.len(), 2);
    for (index, view) in views.iter().enumerate() {
        let movie = native_media::inspect(view, &command.backend.stop).unwrap();
        assert_eq!(movie.tracks.len(), 1);
        assert_eq!(movie.tracks[0].track_id, 1);
        assert_eq!(sample_hashes(view)[0], samples[index]);
    }
    assert_eq!(std::fs::read(&source).unwrap(), original);
}

#[test]
fn progressive_inputs_do_not_enter_fragmented_track_isolation() {
    let clip = Path::new(env!("CARGO_MANIFEST_DIR")).join("../drmr/tests/fixtures/bbb/clip.mp4");
    let original = std::fs::read(&clip).unwrap();
    let owner = crate::native::private_tempdir().unwrap();
    let command = converter();
    assert!(
        complete_track_views(
            &clip,
            owner.path(),
            &command.backend,
            inspection::inspect(&clip).unwrap().tracks.len()
        )
        .unwrap()
        .is_none()
    );
    assert_eq!(std::fs::read_dir(owner.path()).unwrap().count(), 0);
    assert_eq!(std::fs::read(clip).unwrap(), original);
}
