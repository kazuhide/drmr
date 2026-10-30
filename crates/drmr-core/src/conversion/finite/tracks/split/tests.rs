use super::*;
use crate::conversion::{ContentKey, InputKeys, native_media};
use sha2::{Digest as _, Sha256};

const KID: &str = "00112233445566778899aabbccddeeff";
const KEY: &str = "101112131415161718191a1b1c1d1e1f";

fn fixture(root: &Path, scheme: &str) -> PathBuf {
    let clip = Path::new(env!("CARGO_MANIFEST_DIR")).join("../drmr/tests/fixtures/bbb/clip.mp4");
    let parts: Vec<_> = (0..2)
        .map(|index| root.join(format!("part-{index}.mp4")))
        .collect();
    let mut command = std::process::Command::new("packager");
    command.args([
        "--single_threaded",
        "--mp4_reset_initial_composition_offset_to_zero=false",
    ]);
    for (kind, path) in ["video", "audio"].into_iter().zip(&parts) {
        command.arg(format!(
            "input={},stream={kind},output={}",
            clip.display(),
            path.display()
        ));
    }
    command.args(["--clear_lead", "0", "--fragment_duration", "2"]);
    if scheme != "clear" {
        command.args([
            "--enable_raw_key_encryption",
            "--protection_scheme",
            scheme,
            "--keys",
            &format!("label=:key_id={KID}:key={KEY}"),
        ]);
    }
    assert!(
        command.output().unwrap().status.success(),
        "licensed fragmented fixture generation failed"
    );
    let source = root.join("source.mp4");
    super::super::combine_parts(&parts, &source, &Stop::new()).unwrap();
    let mut bytes = std::fs::read(&source).unwrap();
    for (offset, id) in id_fields(&bytes) {
        bytes[offset..offset + 4].copy_from_slice(&[7_u32, 42][id as usize - 1].to_be_bytes());
    }
    std::fs::write(&source, bytes).unwrap();
    source
}

fn id_fields(bytes: &[u8]) -> Vec<(usize, u32)> {
    mp4::walk(&mp4::parse_boxes(bytes).unwrap())
        .into_iter()
        .filter_map(|node| {
            let payload = node.header.payload(bytes);
            let field = match node.kind() {
                kind if kind == *b"tkhd" => match payload[0] {
                    0 => 12,
                    1 => 20,
                    _ => panic!("fixture track-header version"),
                },
                kind if kind == *b"trex" || kind == *b"tfhd" => 4,
                _ => return None,
            };
            let offset = node.header.offset + node.header.header_size + field;
            Some((offset, integer(payload, field).unwrap()))
        })
        .collect()
}

type Chunks = Vec<Vec<(native_media::SampleInfo, [u8; 32])>>;

fn evidence(path: &Path, keys: &InputKeys) -> (native_media::Movie, Chunks) {
    let mut chunks: Chunks = vec![Vec::new(), Vec::new()];
    let movie = native_media::scan(path, keys, &Stop::new(), &mut |track, info, bytes| {
        let mut info = info.clone();
        // File addresses change when a track is isolated; coded clocks and bytes do not.
        info.range = 0..0;
        chunks[track.index].push((info, Sha256::digest(bytes).into()));
        Ok(())
    })
    .unwrap();
    chunks.truncate(movie.tracks.len());
    (movie, chunks)
}

#[test]
fn isolated_track_ids_are_canonical_without_changing_media_or_source() {
    for scheme in ["clear", "cenc", "cbcs"] {
        let owner = crate::native::private_tempdir().unwrap();
        let source = fixture(owner.path(), scheme);
        let original = std::fs::read(&source).unwrap();
        let keys = InputKeys::new(vec![ContentKey::from_hex(KID, KEY).unwrap()]).unwrap();
        let (movie, chunks) = evidence(&source, &keys);
        assert_eq!(
            movie.tracks.iter().map(|t| t.track_id).collect::<Vec<_>>(),
            [7, 42]
        );
        let directory = owner.path().join("views");
        crate::native::create_media_dir_all(&directory).unwrap();
        let views = split_parts(&source, &directory, &Stop::new()).unwrap();
        assert_eq!(views.len(), 2);
        for (index, view) in views.iter().enumerate() {
            let bytes = std::fs::read(view).unwrap();
            assert!(id_fields(&bytes).iter().all(|(_, id)| *id == 1));
            let (actual, actual_chunks) = evidence(view, &keys);
            let mut expected = movie.tracks[index].clone();
            expected.index = 0;
            expected.track_id = 1;
            assert_eq!(actual.tracks, [expected]);
            assert_eq!(actual_chunks, [chunks[index].clone()]);
        }
        assert_eq!(std::fs::read(&source).unwrap(), original);
    }
}

#[test]
fn an_unbound_fragment_track_id_is_refused_before_creating_views() {
    let owner = crate::native::private_tempdir().unwrap();
    let source = fixture(owner.path(), "clear");
    let mut bytes = std::fs::read(&source).unwrap();
    let nodes = mp4::parse_boxes(&bytes).unwrap();
    let header = mp4::walk(&nodes)
        .into_iter()
        .find(|node| node.kind() == *b"tfhd")
        .unwrap();
    let at = header.header.offset + header.header.header_size + 4;
    bytes[at..at + 4].copy_from_slice(&99_u32.to_be_bytes());
    std::fs::write(&source, &bytes).unwrap();
    let directory = owner.path().join("views");
    crate::native::create_media_dir_all(&directory).unwrap();
    assert!(split_parts(&source, &directory, &Stop::new()).is_err());
    assert_eq!(std::fs::read_dir(&directory).unwrap().count(), 0);
    assert_eq!(std::fs::read(&source).unwrap(), bytes);
}

#[test]
fn hidden_track_references_remain_unsupported_before_creating_views() {
    let owner = crate::native::private_tempdir().unwrap();
    let source = fixture(owner.path(), "clear");
    let original = std::fs::read(&source).unwrap();
    let nodes = mp4::parse_boxes(&original).unwrap();
    let movie = nodes.iter().find(|node| node.kind() == *b"moov").unwrap();
    let track = movie
        .children
        .iter()
        .find(|node| node.kind() == *b"trak")
        .unwrap();
    let mut children = track.header.payload(&original).to_vec();
    children.extend(boxed(*b"tref", &boxed(*b"hint", &42_u32.to_be_bytes()).unwrap()).unwrap());
    let replacement = boxed(*b"trak", &children).unwrap();
    let mut children = Vec::new();
    for child in &movie.children {
        if child.header.offset == track.header.offset {
            children.extend_from_slice(&replacement);
        } else {
            children.extend_from_slice(child.header.bytes(&original));
        }
    }
    let mut bytes = original[..movie.header.offset].to_vec();
    bytes.extend(boxed(*b"moov", &children).unwrap());
    bytes.extend_from_slice(&original[movie.header.offset + movie.header.size..]);
    std::fs::write(&source, &bytes).unwrap();
    let directory = owner.path().join("views");
    crate::native::create_media_dir_all(&directory).unwrap();
    assert!(split_parts(&source, &directory, &Stop::new()).is_err());
    assert_eq!(std::fs::read_dir(&directory).unwrap().count(), 0);
    assert_eq!(std::fs::read(&source).unwrap(), bytes);
}
