use super::*;
use crate::{
    concurrency::Stop,
    conversion::{Aes128Key, SegmentIv},
};

#[test]
fn cancelled_encoder_creates_no_output_and_remains_failed() {
    let directory = crate::native::private_tempdir().unwrap();
    let source = directory.path().join("input.ts");
    crate::native::create_private_new(&source)
        .unwrap()
        .write_all(b"unused")
        .unwrap();
    let output = directory.path().join("output.ts");
    let stop = Stop::new();
    stop.set();
    let key = Aes128Key::from_hex("000102030405060708090a0b0c0d0e0f").unwrap();
    let iv = SegmentIv::from_hex("101112131415161718191a1b1c1d1e1f").unwrap();
    let mut encoder = crate::conversion::ts::SegmentEncryptor::new();
    assert_eq!(
        encoder
            .encrypt_segment(&source, &output, &key, &iv, &stop)
            .unwrap_err()
            .0,
        "cancelled"
    );
    assert!(!output.exists());
    assert_eq!(
        encoder
            .encrypt_segment(&source, &output, &key, &iv, &Stop::new())
            .unwrap_err()
            .0,
        "cancelled"
    );
    assert_eq!(encoder.finish().unwrap_err().0, "cancelled");
    assert_eq!(std::fs::read(source).unwrap(), b"unused");
}

fn tool(program: &str, arguments: Vec<String>) -> crate::toolrun::Completed {
    use crate::{
        events::NoSink,
        toolrun::{RunOptions, Secrets, ToolRunner},
    };
    use std::{sync::Arc, time::Duration};
    let runner = ToolRunner::new(Arc::new(Secrets::default()), Arc::new(NoSink));
    let argv = std::iter::once(program.to_owned())
        .chain(arguments)
        .collect::<Vec<_>>();
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
    result
}

fn source(directory: &Path, rate: u32, video: bool, irregular: bool) -> Vec<std::path::PathBuf> {
    files::create_dir(directory).unwrap();
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("../drmr/tests/fixtures/bbb/clip.mp4");
    let mut argv = vec![
        "-v".into(),
        "error".into(),
        "-nostdin".into(),
        "-threads".into(),
        "1".into(),
        "-i".into(),
        files::tool_path(&input).unwrap(),
    ];
    if video {
        argv.extend(["-map".into(), "0:v:0".into(), "-c:v".into()]);
        if irregular {
            argv.extend([
                "libx264".into(),
                "-preset".into(),
                "ultrafast".into(),
                "-threads:v".into(),
                "1".into(),
                "-g".into(),
                "240".into(),
                "-sc_threshold".into(),
                "0".into(),
                "-force_key_frames".into(),
                "0,1,4,7".into(),
            ]);
        } else {
            argv.push("copy".into());
        }
    } else {
        argv.push("-vn".into());
    }
    argv.extend([
        "-map".into(),
        "0:a:0".into(),
        "-c:a".into(),
        "aac".into(),
        "-ar".into(),
        rate.to_string(),
        "-threads:a".into(),
        "1".into(),
        "-hls_time".into(),
        "2".into(),
        "-hls_playlist_type".into(),
        "vod".into(),
        "-hls_segment_filename".into(),
        files::tool_path(&directory.join("segment-%03d.ts")).unwrap(),
        files::tool_path(&directory.join("media.m3u8")).unwrap(),
    ]);
    tool("ffmpeg", argv);
    std::fs::read_to_string(directory.join("media.m3u8"))
        .unwrap()
        .lines()
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(|name| directory.join(name))
        .collect()
}

fn assert_native_equal(a: &canonical::Native, b: &canonical::Native) {
    assert!(a.same_as(b));
    let a = a.segment_timings().unwrap();
    let b = b.segment_timings().unwrap();
    assert_eq!(a.len(), b.len());
    for (a, b) in a.iter().zip(&b) {
        assert_eq!(
            (
                a.count,
                a.first_sample_pts,
                a.first_pts,
                a.max_pts,
                a.end_pts,
                a.first_dts,
                a.last_dts,
                a.terminal_duration
            ),
            (
                b.count,
                b.first_sample_pts,
                b.first_pts,
                b.max_pts,
                b.end_pts,
                b.first_dts,
                b.last_dts,
                b.terminal_duration
            ),
        );
    }
}

#[test]
fn real_bbb_segment_membership_and_all_clocks_survive_native_sample_encryption() {
    use sha2::{Digest as _, Sha256};
    for (rate, video, irregular) in [
        (48_000, true, false),
        (44_100, true, true),
        (44_100, false, false),
    ] {
        let work = crate::native::private_tempdir().unwrap();
        let input_dir = work.path().join("source");
        let segments = source(&input_dir, rate, video, irregular);
        let before = segments
            .iter()
            .map(|p| Sha256::digest(std::fs::read(p).unwrap()))
            .collect::<Vec<_>>();
        let output = work.path().join("output");
        files::create_dir(&output).unwrap();
        let key = Aes128Key::from_hex("101112131415161718191a1b1c1d1e1f").unwrap();
        let iv = SegmentIv::from_hex("303132333435363738393a3b3c3d3e3f").unwrap();
        let stop = Stop::new();
        let mut encoder = SegmentEncryptor::new();
        let mut original = NativeScanner::new();
        let mut projection = super::super::Projector::new();
        let mut pcr = Vec::new();
        let mut cipher_pcr = Vec::new();
        for (i, segment) in segments.iter().enumerate() {
            original.segment(segment, &stop).unwrap();
            let cipher = output.join(format!("cipher-{i}.ts"));
            encoder
                .encrypt_segment(segment, &cipher, &key, &iv, &stop)
                .unwrap();
            projection
                .encrypted_segment(
                    &cipher,
                    &output.join(format!("clear-{i}.ts")),
                    &key,
                    &iv,
                    &stop,
                )
                .unwrap();
            for (path, clocks) in [
                (segment.as_path(), &mut pcr),
                (cipher.as_path(), &mut cipher_pcr),
            ] {
                let bytes = std::fs::read(path).unwrap();
                for packet in bytes.as_chunks::<188>().0 {
                    if packet[3] & 0x20 != 0 && packet[4] >= 7 && packet[5] & 0x10 != 0 {
                        clocks.push(packet[6..12].to_vec());
                    }
                }
            }
        }
        encoder.finish().unwrap();
        projection.finish().unwrap();
        assert_eq!(pcr, cipher_pcr);
        let (_, native) = original.finish().unwrap();
        assert_eq!(native.len(), if video { 2 } else { 1 });
        for (id, expected) in &native {
            assert_native_equal(expected, &projection.native[id]);
            if video && matches!(expected.config, canonical::Config::Video { .. }) && irregular {
                let timings = expected.segment_timings().unwrap();
                assert!(
                    timings.windows(2).any(|p| p[0].count != p[1].count),
                    "fixture must have unequal segment lengths"
                );
            }
        }
        for (segment, expected) in segments.iter().zip(before) {
            assert_eq!(Sha256::digest(std::fs::read(segment).unwrap()), expected);
        }
    }
}

#[test]
fn packet_rewrite_preserves_original_pcr_and_random_access_flags() {
    let mut packet = [0xff; 188];
    packet[..6].copy_from_slice(&[0x47, 0x41, 0, 0x30, 20, 0x70]);
    packet[6..12].copy_from_slice(&[1, 2, 3, 4, 0x7e, 5]);
    let replacement = rewrite_payload(&packet, &[0x55; 170]).unwrap();
    assert_eq!(&replacement[..4], &packet[..4]);
    assert_eq!(&replacement[5..12], &packet[5..12]);
    assert_eq!(&replacement[18..], &[0x55; 170]);
    assert!(rewrite_payload(&packet, &[0x55; 177]).is_err());
}

#[test]
fn encryption_growth_inserts_a_continuation_after_the_final_original_slot() {
    use aes::cipher::{BlockModeDecrypt as _, KeyIvInit as _};
    let key = Aes128Key::from_hex("101112131415161718191a1b1c1d1e1f").unwrap();
    let iv = SegmentIv::from_hex("303132333435363738393a3b3c3d3e3f").unwrap();
    let mut zeros = [0; 16];
    let mut independent = cbc::Decryptor::<aes::Aes128>::new((&*key.0).into(), (&*iv.0).into());
    independent.decrypt_block((&mut zeros).into());
    let mut pes = vec![0, 0, 1, 0xe0, 0, 0, 0x80, 0x80, 5, 0x21, 0, 1, 0, 1];
    pes.extend([0, 0, 1, 0x65]);
    pes.extend([0x55; 31]);
    pes.extend(zeros);
    pes.resize(176, 0x55);
    let original = pes[14..].to_vec();
    let mut packet = Zeroizing::new([0xff; 188]);
    packet[..6].copy_from_slice(&[0x47, 0x41, 0, 0x30, 7, 0x50]);
    packet[6..12].copy_from_slice(&[1, 2, 3, 4, 0x7e, 5]);
    packet[12..].copy_from_slice(&pes);
    let mut window = Window::default();
    window.push(packet).unwrap();
    let mut encoder = SegmentEncryptor::new();
    SegmentEncryptor::protect(
        (256, TrackKind::Video),
        &Pending {
            first: 0,
            slots: vec![0],
            bytes: Zeroizing::new(pes),
        },
        &mut window,
        &key,
        &iv,
        &Stop::new(),
    )
    .unwrap();
    assert_eq!(window.get(0).unwrap().extra.len(), 1);
    let mut output = Vec::new();
    encoder
        .emit(&mut window, 1, &mut output, &Stop::new())
        .unwrap();
    assert_eq!(output.len(), 376);
    assert_eq!(&output[5..12], &[0x50, 1, 2, 3, 4, 0x7e, 5]);
    assert_eq!(output[3] & 15, 0);
    assert_eq!(output[188 + 3] & 15, 1);
    let mut cipher = Vec::new();
    for packet in output.as_chunks::<188>().0 {
        cipher.extend_from_slice(&packet[payload_at(packet).unwrap()..]);
    }
    let (at, pts, dts) = admission::pes_content(&cipher, TrackKind::Video).unwrap();
    assert_eq!((pts, dts), (0, 0));
    assert_eq!(
        sample_aes::decrypt_h264_annex_b(&cipher[at..], &key, &iv)
            .unwrap()
            .as_slice(),
        original
    );
}

#[test]
fn semantic_input_failures_and_existing_destinations_cannot_produce_success() {
    let work = crate::native::private_tempdir().unwrap();
    let segments = source(&work.path().join("source"), 48_000, true, false);
    let bytes = std::fs::read(&segments[0]).unwrap();
    let key = Aes128Key::from_hex("101112131415161718191a1b1c1d1e1f").unwrap();
    let iv = SegmentIv::from_hex("303132333435363738393a3b3c3d3e3f").unwrap();
    let mut bad = bytes.clone();
    let mut seen = std::collections::BTreeSet::new();
    let at = bad
        .as_chunks::<188>()
        .0
        .iter()
        .position(|p| p[3] & 0x10 != 0 && packet_pid(p) != 0x1fff && !seen.insert(packet_pid(p)))
        .unwrap();
    bad[at * 188 + 3] ^= 1;
    let mut crc = bytes.clone();
    let (pmt_slot, pmt_at) = crc
        .as_chunks::<188>()
        .0
        .iter()
        .enumerate()
        .find_map(|(slot, packet)| {
            let at = payload_at(packet).unwrap();
            if packet[1] & 0x40 != 0 && at < 187 {
                let section = at + 1 + usize::from(packet[at]);
                if section < 188 && packet[section] == 2 {
                    return Some((slot, section));
                }
            }
            None
        })
        .unwrap();
    let pmt = pmt_slot * 188 + pmt_at;
    let length = (usize::from(crc[pmt + 1] & 15) << 8) | usize::from(crc[pmt + 2]);
    assert!(pmt_at + 3 + length <= 188);
    crc[pmt + 2 + length] ^= 1;
    let mut pcr = bytes.clone();
    let clock_slots = pcr
        .as_chunks::<188>()
        .0
        .iter()
        .enumerate()
        .filter_map(|(slot, packet)| {
            (packet[3] & 0x20 != 0 && packet[4] >= 7 && packet[5] & 0x10 != 0).then_some(slot)
        })
        .collect::<Vec<_>>();
    assert_ne!(
        &pcr[clock_slots[0] * 188 + 6..clock_slots[0] * 188 + 12],
        &[0, 0, 0, 0, 0x7e, 0]
    );
    pcr[clock_slots[1] * 188 + 6..clock_slots[1] * 188 + 12]
        .copy_from_slice(&[0, 0, 0, 0, 0x7e, 0]);
    for (index, (altered, reason)) in [
        (
            bytes[..bytes.len() - 1].to_vec(),
            "ts-transport-not-supported",
        ),
        (bad, "ts-continuity-mismatch"),
        (crc, "ts-section-crc-mismatch"),
        (pcr, "ts-clock-wrap-or-discontinuity-not-supported"),
    ]
    .into_iter()
    .enumerate()
    {
        let input = work.path().join(format!("bad-{index}.ts"));
        crate::native::create_private_new(&input)
            .unwrap()
            .write_all(&altered)
            .unwrap();
        let output = work.path().join(format!("rejected-{index}.ts"));
        let mut encoder = SegmentEncryptor::new();
        let error = encoder
            .encrypt_segment(&input, &output, &key, &iv, &Stop::new())
            .unwrap_err();
        assert_eq!(error.0, reason);
        assert!(!output.exists());
        assert_eq!(encoder.finish().unwrap_err(), error);
        assert_eq!(std::fs::read(input).unwrap(), altered);
    }
    let destination = work.path().join("foreign.ts");
    crate::native::create_private_new(&destination)
        .unwrap()
        .write_all(b"foreign-owned")
        .unwrap();
    let mut encoder = SegmentEncryptor::new();
    assert!(
        encoder
            .encrypt_segment(&segments[0], &destination, &key, &iv, &Stop::new())
            .is_err()
    );
    assert_eq!(std::fs::read(destination).unwrap(), b"foreign-owned");
    assert_eq!(std::fs::read(&segments[0]).unwrap(), bytes);
}

#[test]
fn cancellation_during_emission_stops_before_the_next_packet() {
    struct CancelWriter<'a> {
        stop: &'a Stop,
        bytes: usize,
    }
    impl std::io::Write for CancelWriter<'_> {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.bytes += bytes.len();
            self.stop.set();
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let stop = Stop::new();
    let mut output = CancelWriter {
        stop: &stop,
        bytes: 0,
    };
    let mut window = Window::default();
    window.push(null_packet()).unwrap();
    window.push(null_packet()).unwrap();
    let mut encoder = SegmentEncryptor::new();
    assert_eq!(
        encoder
            .emit(&mut window, 2, &mut output, &stop)
            .unwrap_err()
            .0,
        "cancelled"
    );
    assert_eq!(output.bytes, 188);
    assert_eq!(window.slots.len(), 1);
}

#[test]
fn pending_bounds_refuse_before_retaining_extra_plaintext_or_slot_metadata() {
    let mut slots = Pending {
        first: 0,
        slots: vec![0; MAX_SLOTS],
        bytes: Zeroizing::new(Vec::new()),
    };
    assert_eq!(slots.append(1, b"x").unwrap_err(), LIMIT);
    assert!(slots.bytes.is_empty());
    let mut bytes = Pending {
        first: 0,
        slots: Vec::new(),
        bytes: Zeroizing::new(vec![0; MAX_PES]),
    };
    assert_eq!(bytes.append(1, b"x").unwrap_err(), LIMIT);
    assert_eq!(bytes.slots.as_slice(), [] as [u64; 0]);
    assert_eq!(bytes.bytes.len(), MAX_PES);
}
