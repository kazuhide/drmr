//! Independent test-only SAMPLE-AES encryption of the attributed BBB transport fixture.
//!
//! This is an encryption oracle, never a call to the production decryption kernel.
//! The complete video and AAC ciphertexts are checked against Shaka; CBC is also
//! checked against NIST SP 800-38A F.2.1. The input is the existing BBB clip
//! attributed under CC BY 3.0 in crates/drmr/tests/fixtures/bbb/README.md.
//! Only public synthetic test keys and IVs are used.
//! Cipher boundaries and PMT descriptors follow the Apple MPEG-2 Stream Encryption
//! Format: <https://developer.apple.com/library/archive/documentation/AudioVideo/Conceptual/HLS_Sample_Encryption/Encryption/Encryption.html>.
use aes::cipher::{Block, BlockEncryptMut as _, KeyIvInit as _};
use drmr_core::{
    events::NoSink,
    toolrun::{RunOptions, Secrets, ToolRunner},
};
use std::{
    collections::BTreeMap,
    io::Write as _,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

pub const KEY_URI: &str = "https://operator.invalid/ts-sample-aes";
const MAX_BYTES: usize = 8 * 1024 * 1024;

pub struct Fixture {
    pub clear: PathBuf,
    pub encrypted: PathBuf,
}

fn write(path: &Path, bytes: &[u8]) {
    drmr_core::native::create_media_new(path)
        .unwrap()
        .write_all(bytes)
        .unwrap();
}

fn tool(program: &str, arguments: &[String], secrets: &[&str]) -> Vec<u8> {
    let runner = ToolRunner::new(
        Arc::new(Secrets::with(secrets.iter().copied())),
        Arc::new(NoSink),
    );
    let argv: Vec<_> = std::iter::once(program.to_owned())
        .chain(arguments.iter().cloned())
        .collect();
    let shown = [program.to_owned(), "[fixture arguments redacted]".into()];
    let result = runner
        .run_tool(
            &argv,
            program,
            &RunOptions {
                timeout: Some(Duration::from_secs(30)),
                argv_redacted: Some(&shown),
                log_stdout: false,
                max_output_chars: 0,
                ..RunOptions::default()
            },
        )
        .unwrap();
    assert_eq!(
        result.returncode,
        Some(0),
        "required fixture tool failed: {program}"
    );
    result.stdout
}

fn path(path: &Path) -> String {
    drmr_core::manifest_support::path::fspath(path).unwrap()
}

/// Return clear and encrypted media playlists; key material is supplied by the caller.
pub fn source(root: &Path, key: &str, iv: &str) -> Fixture {
    let clear = root.join("clear");
    let encrypted = root.join("encrypted");
    drmr_core::native::create_media_dir_all(&clear).unwrap();
    drmr_core::native::create_media_dir_all(&encrypted).unwrap();
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("../drmr/tests/fixtures/bbb/clip.mp4");
    // Portable separators keep FFmpeg's auxiliary HLS paths in the fixture tree.
    tool(
        "ffmpeg",
        &[
            "-v".into(),
            "error".into(),
            "-nostdin".into(),
            "-threads".into(),
            "1".into(),
            "-i".into(),
            path(&input),
            "-map".into(),
            "0:v:0".into(),
            "-map".into(),
            "0:a:0".into(),
            "-c".into(),
            "copy".into(),
            "-hls_time".into(),
            "2".into(),
            "-hls_playlist_type".into(),
            "vod".into(),
            "-start_number".into(),
            "7".into(),
            "-hls_segment_filename".into(),
            path(&clear.join("seg-%02d.ts")),
            path(&clear.join("media.m3u8")),
        ],
        &[],
    );
    let key_bytes: [u8; 16] = hex::decode(key).unwrap().try_into().unwrap();
    let iv_bytes: [u8; 16] = hex::decode(iv).unwrap().try_into().unwrap();
    let playlist = std::fs::read_to_string(clear.join("media.m3u8")).unwrap();
    let mut resources = Vec::new();
    for line in playlist
        .lines()
        .filter(|line| !line.starts_with('#') && !line.is_empty())
    {
        assert!(
            line.starts_with("seg-")
                && Path::new(line)
                    .extension()
                    .is_some_and(|ext| ext.eq_ignore_ascii_case("ts"))
                && !line.contains('/')
        );
        encrypt_segment(
            &clear.join(line),
            &encrypted.join(line),
            &key_bytes,
            &iv_bytes,
        );
        resources.push(encrypted.join(line));
    }
    normalize_continuity(&resources);
    let header = format!("#EXT-X-KEY:METHOD=SAMPLE-AES,URI=\"{KEY_URI}\",IV=0x{iv}\n");
    let cipher_playlist = playlist
        .replacen("#EXTM3U\n", &format!("#EXTM3U\n{header}"), 1)
        .replacen("#EXT-X-VERSION:3\n", "#EXT-X-VERSION:5\n", 1);
    write(&encrypted.join("media.m3u8"), cipher_playlist.as_bytes());
    Fixture {
        clear: clear.join("media.m3u8"),
        encrypted: encrypted.join("media.m3u8"),
    }
}

/// Repair fixture payload counters across an ordered cohort after insertion or rotation.
/// Only header continuity bits change; PCR, PES clocks, and coded bytes stay exact.
pub fn normalize_continuity(segments: &[PathBuf]) {
    let mut counters = BTreeMap::<u16, u8>::new();
    for segment in segments {
        assert!(std::fs::metadata(segment).unwrap().len() <= MAX_BYTES as u64);
        let mut bytes = std::fs::read(segment).unwrap();
        assert_eq!(bytes.len() % 188, 0);
        for packet in bytes.as_chunks_mut::<188>().0 {
            let id = pid(packet);
            let value = match counters.get(&id) {
                Some(previous) if packet[3] & 0x10 != 0 => (previous + 1) & 15,
                Some(previous) => *previous,
                None => packet[3] & 15,
            };
            packet[3] = (packet[3] & 0xf0) | value;
            counters.insert(id, value);
        }
        drmr_core::native::create_media_or_truncate(segment)
            .unwrap()
            .write_all(&bytes)
            .unwrap();
    }
}

fn payload(packet: &[u8; 188]) -> usize {
    assert_eq!(packet[0], 0x47);
    assert_eq!(packet[1] & 0x80, 0);
    let mode = (packet[3] >> 4) & 3;
    assert_ne!(mode, 0);
    if mode & 1 == 0 {
        return 188;
    }
    let at = if mode & 2 == 0 {
        4
    } else {
        5 + usize::from(packet[4])
    };
    assert!(at <= 188);
    at
}
fn pid(packet: &[u8; 188]) -> u16 {
    (u16::from(packet[1] & 31) << 8) | u16::from(packet[2])
}
fn crc(bytes: &[u8]) -> u32 {
    let mut value = u32::MAX;
    for byte in bytes {
        value ^= u32::from(*byte) << 24;
        for _ in 0..8 {
            value = if value & 0x8000_0000 == 0 {
                value << 1
            } else {
                (value << 1) ^ 0x04c1_1db7
            };
        }
    }
    value
}
fn program_map(packet: &mut [u8; 188]) {
    let at = payload(packet);
    if packet[1] & 0x40 == 0 || at == 188 {
        return;
    }
    let start = at + 1 + usize::from(packet[at]);
    if packet.get(start) != Some(&2) {
        return;
    }
    let length = (usize::from(packet[start + 1] & 15) << 8) | usize::from(packet[start + 2]);
    let end = start + 3 + length;
    assert!(end <= 188 && crc(&packet[start..end]) == 0);
    let info = (usize::from(packet[start + 10] & 15) << 8) | usize::from(packet[start + 11]);
    let mut out = packet[start..start + 12 + info].to_vec();
    let mut pos = start + 12 + info;
    while pos < end - 4 {
        let size = (usize::from(packet[pos + 3] & 15) << 8) | usize::from(packet[pos + 4]);
        let mut descriptors = packet[pos + 5..pos + 5 + size].to_vec();
        let kind = match packet[pos] {
            0x1b => {
                descriptors.extend_from_slice(&[15, 4, b'z', b'a', b'v', b'c']);
                0xdb
            }
            0x0f => {
                descriptors.extend_from_slice(&[15, 4, b'a', b'a', b'c', b'd']);
                descriptors.extend_from_slice(&[
                    5, 14, b'a', b'p', b'a', b'd', b'z', b'a', b'a', b'c', 0, 0, 1, 2, 0x11, 0x90,
                ]);
                0xcf
            }
            other => panic!("unexpected clear fixture stream type {other}"),
        };
        out.extend_from_slice(&[
            kind,
            packet[pos + 1],
            packet[pos + 2],
            0xf0 | u8::try_from(descriptors.len() >> 8).unwrap(),
            u8::try_from(descriptors.len() & 255).unwrap(),
        ]);
        out.extend_from_slice(&descriptors);
        pos += 5 + size;
    }
    assert_eq!(pos, end - 4);
    let size = out.len() + 1;
    out[1] = 0xb0 | u8::try_from(size >> 8).unwrap();
    out[2] = u8::try_from(size & 255).unwrap();
    out.extend_from_slice(&crc(&out).to_be_bytes());
    // PMT adaptation bytes are stuffing only in both actual fixture producers.
    if (packet[3] >> 4) & 2 != 0 && packet[4] != 0 {
        assert_eq!(packet[5], 0);
    }
    assert!(out.len() <= 183);
    packet[3] = (packet[3] & 15) | 0x10;
    packet[4..].fill(0xff);
    packet[4] = 0;
    packet[5..5 + out.len()].copy_from_slice(&out);
}
fn start(bytes: &[u8], at: usize) -> usize {
    if bytes.get(at..at + 4) == Some(&[0, 0, 0, 1]) {
        4
    } else if bytes.get(at..at + 3) == Some(&[0, 0, 1]) {
        3
    } else {
        0
    }
}
fn escape(bytes: &[u8], out: &mut Vec<u8>) {
    let mut zeros = 0;
    for byte in bytes {
        if zeros == 2 && *byte <= 3 {
            out.push(3);
            zeros = 0;
        }
        out.push(*byte);
        zeros = if *byte == 0 { zeros + 1 } else { 0 };
    }
}
fn encrypt_blocks(bytes: &mut [u8], key: &[u8; 16], iv: &[u8; 16], skip: usize) {
    let mut cipher = cbc::Encryptor::<aes::Aes128>::new_from_slices(key, iv).unwrap();
    let mut at = 0;
    while bytes.len() - at >= 16 {
        if skip != 0 && bytes.len() - at == 16 {
            break;
        }
        cipher.encrypt_block_mut(Block::<aes::Aes128>::from_mut_slice(
            &mut bytes[at..at + 16],
        ));
        at += 16;
        at += skip.min(bytes.len() - at);
    }
}
fn video(bytes: &[u8], key: &[u8; 16], iv: &[u8; 16]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut pos = 0;
    while pos < bytes.len() {
        let width = start(bytes, pos);
        assert!(width > 0);
        let begin = pos + width;
        let mut next = begin;
        while next < bytes.len() && start(bytes, next) == 0 {
            next += 1;
        }
        let mut end = next;
        while end > begin && bytes[end - 1] == 0 {
            end -= 1;
        }
        let mut nal = bytes[begin..end].to_vec();
        assert_ne!(nal, [] as [u8; 0]);
        out.extend_from_slice(&bytes[pos..begin]);
        if matches!(nal[0] & 31, 1 | 5) && nal.len() > 48 {
            encrypt_blocks(&mut nal[32..], key, iv, 144);
            escape(&nal, &mut out);
        } else {
            out.extend_from_slice(&nal);
        }
        out.extend_from_slice(&bytes[end..next]);
        pos = next;
    }
    out
}
fn audio(bytes: &[u8], key: &[u8; 16], iv: &[u8; 16]) -> Vec<u8> {
    let mut out = bytes.to_vec();
    let mut pos = 0;
    while pos < out.len() {
        assert!(out.len() - pos >= 7 && out[pos] == 0xff && out[pos + 1] == 0xf1);
        let length = (usize::from(out[pos + 3] & 3) << 11)
            | (usize::from(out[pos + 4]) << 3)
            | usize::from(out[pos + 5] >> 5);
        assert!(length > 7 && pos + length <= out.len());
        if length > 23 {
            encrypt_blocks(&mut out[pos + 23..pos + length], key, iv, 0);
        }
        pos += length;
    }
    out
}
struct Pes {
    slots: Vec<usize>,
    bytes: Vec<u8>,
    video: bool,
}
fn extra_packet(id: u16, bytes: &[u8]) -> [u8; 188] {
    assert!(!bytes.is_empty() && bytes.len() <= 184);
    let mut packet = [0xff; 188];
    packet[..4].copy_from_slice(&[
        0x47,
        u8::try_from(id >> 8).unwrap(),
        u8::try_from(id & 255).unwrap(),
        0x10,
    ]);
    let at = if bytes.len() == 184 {
        4
    } else {
        packet[3] = 0x30;
        packet[4] = u8::try_from(183 - bytes.len()).unwrap();
        if packet[4] != 0 {
            packet[5] = 0;
        }
        188 - bytes.len()
    };
    packet[at..].copy_from_slice(bytes);
    packet
}
fn replace(
    pes: &Pes,
    packets: &mut [[u8; 188]],
    insert: &mut BTreeMap<usize, Vec<[u8; 188]>>,
    key: &[u8; 16],
    iv: &[u8; 16],
) {
    assert!(pes.bytes.len() <= MAX_BYTES && pes.bytes.starts_with(&[0, 0, 1]));
    let header = 9 + usize::from(pes.bytes[8]);
    let coded = if pes.video {
        video(&pes.bytes[header..], key, iv)
    } else {
        audio(&pes.bytes[header..], key, iv)
    };
    let mut bytes = pes.bytes[..header].to_vec();
    bytes.extend_from_slice(&coded);
    if bytes[4] != 0 || bytes[5] != 0 {
        let length = u16::try_from(bytes.len() - 6).unwrap().to_be_bytes();
        bytes[4..6].copy_from_slice(&length);
    }
    assert!(bytes.len() >= pes.bytes.len());
    let mut pos = 0;
    for slot in &pes.slots {
        let at = payload(&packets[*slot]);
        let amount = 188 - at;
        packets[*slot][at..].copy_from_slice(&bytes[pos..pos + amount]);
        pos += amount;
    }
    let last = *pes.slots.last().unwrap();
    let id = pid(&packets[last]);
    let mut extra = Vec::new();
    while pos < bytes.len() {
        let end = (pos + 184).min(bytes.len());
        extra.push(extra_packet(id, &bytes[pos..end]));
        pos = end;
    }
    assert!(insert.insert(last, extra).is_none());
}

/// Encrypt an owned, bounded clear TS segment without changing PES clocks or PCR bytes.
pub fn encrypt_segment(source: &Path, dest: &Path, key: &[u8; 16], iv: &[u8; 16]) {
    assert!(std::fs::metadata(source).unwrap().len() <= MAX_BYTES as u64);
    let original = std::fs::read(source).unwrap();
    assert_eq!(original.len() % 188, 0);
    let mut packets: Vec<[u8; 188]> = original.as_chunks::<188>().0.to_vec();
    let mut pending = BTreeMap::<u16, Pes>::new();
    let mut insert = BTreeMap::new();
    for index in 0..packets.len() {
        program_map(&mut packets[index]);
        let at = payload(&packets[index]);
        if at == 188 {
            continue;
        }
        let id = pid(&packets[index]);
        let bytes = &packets[index][at..];
        let begin = packets[index][1] & 0x40 != 0 && bytes.starts_with(&[0, 0, 1]);
        if begin {
            if let Some(previous) = pending.remove(&id) {
                replace(&previous, &mut packets, &mut insert, key, iv);
            }
            let stream = packets[index][at + 3];
            assert!((0xc0..=0xef).contains(&stream));
            pending.insert(
                id,
                Pes {
                    slots: Vec::new(),
                    bytes: Vec::new(),
                    video: stream >= 0xe0,
                },
            );
        }
        if let Some(pes) = pending.get_mut(&id) {
            pes.slots.push(index);
            pes.bytes.extend_from_slice(&packets[index][at..]);
            assert!(pes.bytes.len() <= MAX_BYTES);
        }
    }
    for pes in pending.into_values() {
        replace(&pes, &mut packets, &mut insert, key, iv);
    }
    let mut out = Vec::with_capacity(original.len() * 2);
    let mut counters = BTreeMap::<u16, (u8, bool)>::new();
    for (index, packet) in packets.into_iter().enumerate() {
        for mut item in std::iter::once(packet).chain(insert.remove(&index).unwrap_or_default()) {
            let id = pid(&item);
            let counter = counters.entry(id).or_default();
            if (item[3] >> 4) & 1 != 0 {
                if counter.1 {
                    counter.0 = (counter.0 + 1) & 15;
                }
                counter.1 = true;
            }
            item[3] = (item[3] & 0xf0) | counter.0;
            out.extend_from_slice(&item);
        }
    }
    assert!(out.len() <= MAX_BYTES);
    write(dest, &out);
    assert_eq!(std::fs::read(source).unwrap(), original);
}

#[cfg(test)]
mod tests {
    use super::*;
    const KEY: &str = "101112131415161718191a1b1c1d1e1f";
    const IV: &str = "303132333435363738393a3b3c3d3e3f";
    fn pes(path: &Path, is_video: bool) -> Vec<Vec<u8>> {
        let bytes = std::fs::read(path).unwrap();
        let mut pending = BTreeMap::<u16, Vec<u8>>::new();
        let mut out = Vec::new();
        for packet in bytes.as_chunks::<188>().0 {
            let at = payload(packet);
            if at == 188 {
                continue;
            }
            let id = pid(packet);
            let bytes = &packet[at..];
            if packet[1] & 0x40 != 0 && bytes.starts_with(&[0, 0, 1]) {
                let selected = if is_video {
                    (0xe0..=0xef).contains(&bytes[3])
                } else {
                    (0xc0..=0xdf).contains(&bytes[3])
                };
                if !selected {
                    continue;
                }
                if let Some(previous) = pending.remove(&id) {
                    out.push(previous);
                }
                pending.insert(id, Vec::new());
            }
            if let Some(previous) = pending.get_mut(&id) {
                previous.extend_from_slice(bytes);
            }
        }
        out.extend(pending.into_values());
        out
    }
    #[test]
    fn generated_presentation_keeps_continuity_across_every_resource() {
        let root = drmr_core::native::private_tempdir().unwrap();
        let fixture = source(root.path(), KEY, IV);
        let playlist = std::fs::read_to_string(&fixture.encrypted).unwrap();
        let mut counters = BTreeMap::<u16, u8>::new();
        for name in playlist
            .lines()
            .filter(|line| !line.is_empty() && !line.starts_with('#'))
        {
            let bytes = std::fs::read(fixture.encrypted.parent().unwrap().join(name)).unwrap();
            for packet in bytes.as_chunks::<188>().0 {
                let id = pid(packet);
                let current = packet[3] & 15;
                if let Some(previous) = counters.get(&id) {
                    let expected = if packet[3] & 0x10 != 0 {
                        (previous + 1) & 15
                    } else {
                        *previous
                    };
                    assert_eq!(current, expected, "resource-spanning payload continuity");
                }
                counters.insert(id, current);
            }
        }
        assert!(counters.len() >= 4);
    }
    #[test]
    fn fixture_aac_encryption_matches_nist_cbc_and_clear_tail() {
        let key = hex::decode("2b7e151628aed2a6abf7158809cf4f3c")
            .unwrap()
            .try_into()
            .unwrap();
        let iv = hex::decode("000102030405060708090a0b0c0d0e0f")
            .unwrap()
            .try_into()
            .unwrap();
        let plain = hex::decode(concat!(
            "6bc1bee22e409f96e93d7e117393172a",
            "ae2d8a571e03ac9c9eb76fac45af8e51",
            "30c81c46a35ce411e5fbc1191a0a52ef",
            "f69f2445df4f9b17ad2b417be66c3710"
        ))
        .unwrap();
        let expected = hex::decode(concat!(
            "7649abac8119b246cee98e9b12e9197d",
            "5086cb9b507219ee95db113a917678b2",
            "73bed6b8e3c1743b7116e69e22229516",
            "3ff1caa1681fac09120eca307586e1a7"
        ))
        .unwrap();
        let mut frame = vec![0xff, 0xf1, 0x4c, 0x80, 0x0b, 0x1f, 0xfc]; // 88 bytes: header + leader + four blocks + one-byte tail.
        frame.extend_from_slice(&[0x55; 16]);
        frame.extend_from_slice(&plain);
        frame.push(0x77);
        assert_eq!(frame.len(), 88);
        let encrypted = audio(&frame, &key, &iv);
        assert_eq!(&encrypted[..23], &frame[..23]);
        assert_eq!(&encrypted[23..87], expected);
        assert_eq!(encrypted[87], 0x77);
    }
    fn shaka(root: &Path, input: &Path, role: &str, encrypted: bool) {
        drmr_core::native::create_media_dir_all(root).unwrap();
        let mut args = vec![
            format!(
                "input={},stream={role},segment_template={},playlist_name=media.m3u8",
                path(input),
                path(&root.join("seg_$Number$.ts"))
            ),
            "--hls_master_playlist_output".into(),
            path(&root.join("master.m3u8")),
            "--segment_duration".into(),
            "2".into(),
            "--fragment_duration".into(),
            "2".into(),
            "--transport_stream_timestamp_offset_ms".into(),
            "0".into(),
        ];
        if encrypted {
            args.extend([
                "--enable_raw_key_encryption".into(),
                "--keys".into(),
                format!("label=:key_id=000102030405060708090a0b0c0d0e0f:key={KEY}"),
                "--iv".into(),
                IV.into(),
                "--protection_scheme".into(),
                "cbcs".into(),
                "--clear_lead".into(),
                "0".into(),
                "--hls_key_uri".into(),
                KEY_URI.into(),
            ]);
        }
        tool("packager", &args, &[KEY, IV]);
    }
    #[test]
    fn fixture_encryptor_matches_all_real_shaka_video_and_aac_ciphertexts() {
        // BBB's CC BY 3.0 attribution is recorded with the existing clip.mp4 fixture.
        // Shaka independently generates both reference sides; no product decryption
        // participates in this fixture generator's oracle.
        let root = drmr_core::native::private_tempdir().unwrap();
        let original =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../drmr/tests/fixtures/bbb/clip.mp4");
        let original_bytes = std::fs::read(&original).unwrap();
        let fixture = source(root.path(), KEY, IV);
        assert!(fixture.clear.is_file() && fixture.encrypted.is_file());
        assert!(
            std::fs::read_to_string(&fixture.encrypted)
                .unwrap()
                .contains("#EXT-X-VERSION:5\n")
        );
        let input = root.path().join("clear/seg-07.ts");
        let mut transport = Vec::new();
        for number in 7..12 {
            transport.extend(
                std::fs::read(root.path().join(format!("clear/seg-{number:02}.ts"))).unwrap(),
            );
        }
        let baseline = root.path().join("source.ts");
        write(&baseline, &transport);
        assert!(input.is_file());
        let key = hex::decode(KEY).unwrap().try_into().unwrap();
        let iv = hex::decode(IV).unwrap().try_into().unwrap();
        for (role, is_video, segments, total) in [("video", true, 5, 240), ("audio", false, 6, 470)]
        {
            let clear = root.path().join(format!("shaka-{role}-clear"));
            let cipher = root.path().join(format!("shaka-{role}-cipher"));
            shaka(&clear, &baseline, role, false);
            shaka(&cipher, &baseline, role, true);
            let mut count = 0;
            for number in 1..=segments {
                let source = clear.join(format!("seg_{number}.ts"));
                let ours = root.path().join(format!("ours-{role}-{number}.ts"));
                encrypt_segment(&source, &ours, &key, &iv);
                let expected = pes(&cipher.join(format!("seg_{number}.ts")), is_video);
                let actual = pes(&ours, is_video);
                assert_eq!(actual.len(), expected.len());
                for (a, b) in actual.iter().zip(&expected) {
                    let ah = 9 + usize::from(a[8]);
                    let bh = 9 + usize::from(b[8]);
                    assert_eq!(&a[6..ah], &b[6..bh], "all original PES flags and clocks");
                    assert_eq!(
                        &a[ah..],
                        &b[bh..],
                        "independent Shaka ciphertext at {role} packet {count}"
                    );
                    count += 1;
                }
            }
            assert_eq!(count, total);
        }
        assert_eq!(std::fs::read(original).unwrap(), original_bytes);
    }
}
