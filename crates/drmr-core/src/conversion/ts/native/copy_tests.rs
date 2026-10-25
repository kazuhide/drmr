mod copied_segments {
    use super::*;

    fn admitted(scanner: &NativeScanner, source: &Path, destination: &Path) -> receipt::Receipt {
        receipt::Receipt::copy(
            &scanner.admission,
            &scanner.receipt_context,
            source,
            destination,
            &Stop::new(),
        )
        .unwrap()
    }
    fn assert_poisoned(
        mut scanner: NativeScanner,
        source: &Path,
        destination: &Path,
        error: ConversionError,
    ) {
        assert_eq!(
            scanner.copy_segment(source, destination, &Stop::new()),
            Err(error)
        );
        assert!(!destination.exists());
        assert_eq!(scanner.finish().err(), Some(error));
    }

    #[test]
    fn late_transport_failures_precede_every_observer_and_remove_only_the_owned_copy() {
        let (_owner, root) = root();
        let source = resource(&root, "source.ts", 90_000, &[frame(1)], 0);
        let mut valid = std::fs::read(&source).unwrap();
        valid.extend(packet(256, true, 1, &pes(91_920, &frame(2), true), false));
        let mut crc_packet: [u8; 188] = valid[..188].try_into().unwrap();
        crc_packet[3] = (crc_packet[3] & 0xf0) | 1;
        crc_packet[187] ^= 1;
        for (name, tail, expected) in [
            (
                "crc",
                crc_packet,
                ConversionError("ts-section-crc-mismatch"),
            ),
            (
                "continuity",
                packet(256, false, 7, &[], false),
                ConversionError("ts-continuity-mismatch"),
            ),
            (
                "pes",
                packet(256, true, 2, &[0, 0], false),
                ConversionError("ts-transport-not-supported"),
            ),
        ] {
            let input = root.join(format!("{name}.ts"));
            let bytes = [valid.as_slice(), &tail].concat();
            std::fs::write(&input, &bytes).unwrap();
            let output = root.join(format!("{name}-copy.ts"));
            let mut scanner = NativeScanner::new();
            let before = scanner.admission.clone();
            let mut calls = 0;
            assert_eq!(
                scanner.copy_segment_with(&input, &output, &Stop::new(), &mut |_, _, _, _, _| {
                    calls += 1;
                    Ok(())
                }),
                Err(expected)
            );
            assert_eq!(calls, 0);
            assert!(scanner.admission == before);
            assert!(!output.exists());
            assert_eq!(std::fs::read(&input).unwrap(), bytes);
            assert_poisoned(
                scanner,
                &source,
                &root.join(format!("{name}-retry.ts")),
                expected,
            );
        }
    }

    #[test]
    fn changed_copies_cannot_commit_evidence_or_advance_admission() {
        let (_owner, root) = root();
        let source = resource(&root, "source.ts", 90_000, &[frame(1)], 0);
        let original = std::fs::read(&source).unwrap();
        for change in ["truncated", "extended", "same-inode", "replaced"] {
            let mut scanner = NativeScanner::new();
            let before = scanner.admission.clone();
            let destination = root.join(format!("{change}.ts"));
            let receipt = admitted(&scanner, &source, &destination);
            let identity =
                crate::native::FileIdentity::read(&std::fs::File::open(&destination).unwrap())
                    .unwrap();
            let mut changed = original.clone();
            match change {
                "truncated" => {
                    changed.pop();
                }
                "extended" => changed.push(0),
                "same-inode" => {
                    *changed.last_mut().unwrap() ^= 1;
                }
                "replaced" => {
                    std::fs::rename(&destination, root.join("moved-owned.ts")).unwrap();
                }
                _ => unreachable!(),
            }
            std::fs::write(&destination, &changed).unwrap();
            let current =
                crate::native::FileIdentity::read(&std::fs::File::open(&destination).unwrap())
                    .unwrap();
            assert_eq!(identity == current, change != "replaced");
            let error = scanner
                .scan_receipt(receipt, &Stop::new(), &mut |_, _, _, _, _| Ok(()))
                .unwrap_err();
            assert!(scanner.admission == before);
            if change == "replaced" {
                assert_eq!(std::fs::read(&destination).unwrap(), changed);
                assert_eq!(
                    std::fs::read(root.join("moved-owned.ts")).unwrap(),
                    original
                );
            } else {
                assert!(!destination.exists());
            }
            assert_eq!(std::fs::read(&source).unwrap(), original);
            assert_poisoned(
                scanner,
                &source,
                &root.join(format!("{change}-retry.ts")),
                error,
            );
        }
    }

    #[test]
    fn receipts_cannot_be_reordered_replayed_or_moved_between_scanner_contexts() {
        let (_owner, root) = root();
        let first = resource(&root, "first.ts", 90_000, &[frame(1)], 0);
        let second = resource(&root, "second.ts", 91_920, &[frame(2)], 1);
        let mut scanner = NativeScanner::new();
        let one = admitted(&scanner, &first, &root.join("one.ts"));
        let two = receipt::Receipt::copy(
            &one.after,
            &scanner.receipt_context,
            &second,
            &root.join("two.ts"),
            &Stop::new(),
        )
        .unwrap();
        let before = scanner.admission.clone();
        assert_eq!(
            scanner.scan_receipt(two, &Stop::new(), &mut |_, _, _, _, _| panic!(
                "reordered observer"
            )),
            Err(BAD)
        );
        assert!(scanner.admission == before);
        drop(one);
        assert_poisoned(scanner, &first, &root.join("reorder-retry.ts"), BAD);

        let mut scanner = NativeScanner::new();
        let one = admitted(&scanner, &first, &root.join("accept.ts"));
        let replay = admitted(&scanner, &first, &root.join("replay.ts"));
        scanner
            .scan_receipt(one, &Stop::new(), &mut |_, _, _, _, _| Ok(()))
            .unwrap();
        let before = scanner.admission.clone();
        assert_eq!(
            scanner.scan_receipt(replay, &Stop::new(), &mut |_, _, _, _, _| panic!(
                "replayed observer"
            )),
            Err(BAD)
        );
        assert!(scanner.admission == before);
        assert_poisoned(scanner, &first, &root.join("replay-retry.ts"), BAD);

        let owner = NativeScanner::new();
        let receipt = admitted(&owner, &first, &root.join("foreign.ts"));
        let mut other = NativeScanner::new();
        assert!(owner.admission == other.admission);
        assert_eq!(
            other.scan_receipt(receipt, &Stop::new(), &mut |_, _, _, _, _| panic!(
                "foreign observer"
            )),
            Err(BAD)
        );
        assert_poisoned(other, &first, &root.join("foreign-retry.ts"), BAD);
    }

    #[test]
    fn cancellation_and_late_observer_errors_preserve_the_first_failure_and_no_partial_evidence() {
        let (_owner, root) = root();
        let source = resource(&root, "source.ts", 90_000, &[frame(1), frame(2)], 0);
        let stop = Stop::new();
        stop.set();
        let mut scanner = NativeScanner::new();
        let destination = root.join("cancelled.ts");
        assert_eq!(
            scanner.copy_segment(&source, &destination, &stop),
            Err(ConversionError("cancelled"))
        );
        assert!(!destination.exists());
        assert_poisoned(
            scanner,
            &source,
            &root.join("cancel-retry.ts"),
            ConversionError("cancelled"),
        );

        let mut scanner = NativeScanner::new();
        let destination = root.join("cancel-after-admission.ts");
        let receipt = admitted(&scanner, &source, &destination);
        assert_eq!(
            scanner.scan_receipt(receipt, &stop, &mut |_, _, _, _, _| panic!(
                "cancelled observer"
            )),
            Err(ConversionError("cancelled"))
        );
        assert!(!destination.exists());
        assert_poisoned(
            scanner,
            &source,
            &root.join("late-cancel-retry.ts"),
            ConversionError("cancelled"),
        );

        let mut scanner = NativeScanner::new();
        let before = scanner.admission.clone();
        let stop = Stop::new();
        let destination = root.join("observer-error.ts");
        let observer_error = ConversionError("observer-stopped-late");
        let mut calls = 0;
        assert_eq!(
            scanner.copy_segment_with(&source, &destination, &stop, &mut |_, _, _, _, _| {
                calls += 1;
                if calls == 2 {
                    stop.set();
                    return Err(observer_error);
                }
                Ok(())
            }),
            Err(observer_error)
        );
        assert_eq!(calls, 2);
        assert!(scanner.admission == before);
        assert!(!destination.exists());
        assert_poisoned(
            scanner,
            &source,
            &root.join("observer-retry.ts"),
            observer_error,
        );
    }

    #[test]
    fn exclusive_destination_refusal_preserves_foreign_bytes_and_poisoning() {
        let (_owner, root) = root();
        let source = resource(&root, "source.ts", 90_000, &[frame(1)], 0);
        let destination = root.join("foreign.ts");
        std::fs::write(&destination, b"foreign destination").unwrap();
        let mut scanner = NativeScanner::new();
        let error = scanner
            .copy_segment(&source, &destination, &Stop::new())
            .unwrap_err();
        assert_eq!(std::fs::read(&destination).unwrap(), b"foreign destination");
        assert_poisoned(scanner, &source, &root.join("exclusive-retry.ts"), error);
    }

    #[test]
    fn copied_video_keeps_configuration_payload_and_absolute_clocks() {
        let (_owner, root) = root();
        let first = track_resource(&root, "first.ts", 90_000, &video(1), 0, false);
        let second = track_resource(&root, "second.ts", 93_750, &video(2), 1, false);
        let mut old = NativeScanner::new();
        let mut new = NativeScanner::new();
        for (index, source) in [&first, &second].into_iter().enumerate() {
            old.segment(source, &Stop::new()).unwrap();
            new.copy_segment(source, &root.join(format!("copy-{index}.ts")), &Stop::new())
                .unwrap();
        }
        let (old_admission, old) = old.finish().unwrap();
        let (new_admission, new) = new.finish().unwrap();
        assert!(old_admission == new_admission);
        assert!(old[&256].same_as(&new[&256]));
        assert_eq!(old[&256].config, new[&256].config);
    }

    #[test]
    fn admitted_copy_keeps_the_two_pass_audio_evidence_and_all_admission_state() {
        for fractional in [false, true] {
            let (_owner, root) = root();
            let frames = (0..14)
                .map(|marker| {
                    let mut data = frame(marker);
                    if fractional {
                        data[2] = 0x50;
                    }
                    data
                })
                .collect::<Vec<_>>();
            let first = resource(&root, "first.ts", 126_000, &frames[..7], 0);
            let second = resource(
                &root,
                "second.ts",
                if fractional { 140_629 } else { 139_440 },
                &frames[7..],
                1,
            );
            let stop = Stop::new();
            let mut original = NativeScanner::new();
            let mut copied = NativeScanner::new();
            for (index, source) in [&first, &second].into_iter().enumerate() {
                original.segment(source, &stop).unwrap();
                let destination = root.join(format!("copy-{index}.ts"));
                copied.copy_segment(source, &destination, &stop).unwrap();
                assert!(original.admission() == copied.admission());
                assert_eq!(
                    std::fs::read(source).unwrap(),
                    std::fs::read(destination).unwrap()
                );
            }
            let (old_admission, old) = original.finish().unwrap();
            let (new_admission, new) = copied.finish().unwrap();
            assert!(old_admission == new_admission);
            assert_eq!(
                old.keys().collect::<Vec<_>>(),
                new.keys().collect::<Vec<_>>()
            );
            for (pid, anchor) in old {
                assert!(anchor.same_as(&new[&pid]));
                assert_eq!(anchor.config, new[&pid].config);
                assert_eq!(anchor.count, new[&pid].count);
                assert_eq!(anchor.metadata().unwrap(), new[&pid].metadata().unwrap());
                assert_eq!(anchor.timing().unwrap(), new[&pid].timing().unwrap());
                assert_eq!(anchor.evidence().unwrap(), new[&pid].evidence().unwrap());
                assert_eq!(
                    anchor.segment_timings().unwrap(),
                    new[&pid].segment_timings().unwrap()
                );
            }
        }
    }

    #[test]
    fn the_receipt_binds_null_packet_bytes_that_native_evidence_does_not_include() {
        let (_owner, root) = root();
        let source = resource(&root, "source.ts", 90_000, &[frame(1)], 0);
        let mut bytes = std::fs::read(&source).unwrap();
        bytes.extend(packet(0x1fff, false, 0, &[0x55; 16], false));
        std::fs::write(&source, &bytes).unwrap();
        let destination = root.join("copied.ts");
        let mut scanner = NativeScanner::new();
        let before = scanner.admission.clone();
        let receipt = admitted(&scanner, &source, &destination);
        *bytes.last_mut().unwrap() ^= 1;
        std::fs::write(&destination, &bytes).unwrap();
        let error = scanner
            .scan_receipt(receipt, &Stop::new(), &mut |_, _, _, _, _| Ok(()))
            .unwrap_err();
        assert!(scanner.admission == before);
        assert!(!destination.exists());
        assert_poisoned(scanner, &source, &root.join("retry.ts"), error);
    }

    #[test]
    fn partial_copy_reads_cannot_satisfy_the_whole_byte_receipt() {
        use std::io::Read as _;
        let (_owner, root) = root();
        let source = resource(&root, "source.ts", 90_000, &[frame(1)], 0);
        let destination = root.join("copied.ts");
        let scanner = NativeScanner::new();
        let receipt = admitted(&scanner, &source, &destination);
        let mut checked = receipt.open(&Stop::new()).unwrap();
        checked.read_exact(&mut [0; 188]).unwrap();
        assert!(receipt.verify(checked, &Stop::new()).is_err());
        drop(receipt);
        assert!(!destination.exists());
    }

    #[test]
    fn cancellation_inside_native_visitation_does_not_commit_admission_or_partial_evidence() {
        let (_owner, root) = root();
        let source = resource(&root, "source.ts", 90_000, &[frame(1), frame(2)], 0);
        let destination = root.join("copied.ts");
        let stop = Stop::new();
        let mut scanner = NativeScanner::new();
        let before = scanner.admission.clone();
        let mut calls = 0;
        assert_eq!(
            scanner.copy_segment_with(&source, &destination, &stop, &mut |_, _, _, _, _| {
                calls += 1;
                stop.set();
                Ok(())
            }),
            Err(ConversionError("cancelled"))
        );
        assert_eq!(calls, 1);
        assert!(scanner.admission == before);
        assert!(!destination.exists());
        assert_poisoned(
            scanner,
            &source,
            &root.join("retry.ts"),
            ConversionError("cancelled"),
        );
    }

    #[test]
    fn incomplete_transport_copies_are_refused_before_visitation() {
        let (_owner, root) = root();
        let complete = resource(&root, "complete.ts", 90_000, &[frame(1)], 0);
        let original = std::fs::read(&complete).unwrap();
        for length in [0, 1, 187, original.len() - 1] {
            let source = root.join(format!("partial-{length}.ts"));
            let destination = root.join(format!("copy-{length}.ts"));
            std::fs::write(&source, &original[..length]).unwrap();
            let mut scanner = NativeScanner::new();
            let error = scanner
                .copy_segment_with(&source, &destination, &Stop::new(), &mut |_, _, _, _, _| {
                    panic!("truncated observer")
                })
                .unwrap_err();
            assert!(!destination.exists());
            assert_eq!(std::fs::read(&source).unwrap(), original[..length]);
            assert_poisoned(
                scanner,
                &source,
                &root.join(format!("retry-{length}.ts")),
                error,
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_path_replaced_during_native_read_is_preserved_and_cannot_commit_the_old_copy() {
        let (_owner, root) = root();
        let source = resource(&root, "source.ts", 90_000, &[frame(1)], 0);
        let destination = root.join("copied.ts");
        let displaced = root.join("displaced.ts");
        let mut scanner = NativeScanner::new();
        let before = scanner.admission.clone();
        let error = scanner
            .copy_segment_with(&source, &destination, &Stop::new(), &mut |_, _, _, _, _| {
                std::fs::rename(&destination, &displaced).unwrap();
                std::fs::write(&destination, b"foreign file").unwrap();
                Ok(())
            })
            .unwrap_err();
        assert!(scanner.admission == before);
        assert_eq!(std::fs::read(&destination).unwrap(), b"foreign file");
        assert_eq!(
            std::fs::read(&source).unwrap(),
            std::fs::read(displaced).unwrap()
        );
        assert_poisoned(scanner, &source, &root.join("retry.ts"), error);
    }
}
