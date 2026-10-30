//! Stream box headers and bound metadata without loading media-data payloads.
use super::{ConversionError, MISMATCH, MultiObjects, OutputKey, TargetProtection, common, mp4};
use crate::{concurrency::Stop, native::FileIdentity};
use std::{
    io::{Read as _, Seek as _, SeekFrom},
    path::Path,
};
use zeroize::Zeroizing;

const BOX_LIMIT: usize = 100_000;
const METADATA_BOX_LIMIT: usize = super::super::LIMIT / std::mem::size_of::<mp4::Mp4Box>();

pub(super) enum Selection<'a> {
    Default {
        target: TargetProtection,
        key: Option<&'a OutputKey>,
    },
    Providers(&'a MultiObjects),
    FairPlay,
}

struct Records<'a> {
    selection: Selection<'a>,
    common: usize,
    encrypted_tracks: usize,
    widevine: bool,
    playready: bool,
}

impl Records<'_> {
    fn node(&mut self, node: &mp4::Mp4Box, bytes: &[u8]) -> Result<(), ConversionError> {
        let payload = node.header.payload(bytes);
        match (&node.kind(), &self.selection) {
            (b"uuid", _) | (b"pssh", Selection::FairPlay) => return Err(MISMATCH),
            (b"pssh", Selection::Providers(expected)) => {
                let record = node.header.bytes(bytes);
                if expected
                    .widevine
                    .as_ref()
                    .is_some_and(|value| value.as_slice() == record)
                    && !self.widevine
                {
                    self.widevine = true;
                } else if expected
                    .playready
                    .as_ref()
                    .is_some_and(|value| value.bytes.as_slice() == record)
                    && !self.playready
                {
                    self.playready = true;
                } else {
                    return Err(MISMATCH);
                }
            }
            (b"pssh", Selection::Default { target, key }) => {
                common::payload(payload, common::key(*target, *key)?)?;
                self.common += 1;
                if self.common > 1 {
                    return Err(MISMATCH);
                }
            }
            (b"tenc", Selection::Default { target, key }) => {
                let expected = common::key(*target, *key)?;
                let track = mp4::Tenc::parse(payload).ok_or(MISMATCH)?;
                if track.default_is_protected != 1 || track.default_kid != expected.0.id {
                    return Err(MISMATCH);
                }
                self.encrypted_tracks += 1;
            }
            (b"schm", Selection::Default { target, key }) => {
                common::key(*target, *key)?;
                if payload.len() != 12
                    || payload[..4] != [0; 4]
                    || payload.get(4..8) != Some(target.as_str().as_bytes())
                {
                    return Err(MISMATCH);
                }
            }
            _ => {}
        }
        Ok(())
    }
}

/// Media records may repeat a selected provider's exact init record once per resource.
/// Default Common records retain the stricter initialization-only qualification.
pub(super) fn check(
    path: &Path,
    selection: Selection<'_>,
    stop: &Stop,
) -> Result<(), ConversionError> {
    check_range(path, None, selection, stop)
}

/// Repeated records are scoped to one addressed media subsegment, including indexed files.
pub(super) fn check_range(
    path: &Path,
    range: Option<(u64, u64)>,
    selection: Selection<'_>,
    stop: &Stop,
) -> Result<(), ConversionError> {
    super::super::cancelled(stop)?;
    let mut file = super::super::super::files::regular_input(path).map_err(|_| MISMATCH)?;
    let identity = FileIdentity::read(&file).map_err(|_| MISMATCH)?;
    let length = file.metadata().map_err(|_| MISMATCH)?.len();
    let (start, end) = range.map_or(Ok((0, length)), |(start, end)| {
        end.checked_add(1)
            .filter(|end| start < *end && *end <= length)
            .map(|end| (start, end))
            .ok_or(MISMATCH)
    })?;
    file.seek(SeekFrom::Start(start)).map_err(|_| MISMATCH)?;
    let mut records = Records {
        selection,
        common: 0,
        encrypted_tracks: 0,
        widevine: false,
        playready: false,
    };
    let mut at = start;
    let mut remaining = BOX_LIMIT;
    while at < end {
        super::super::cancelled(stop)?;
        remaining = remaining.checked_sub(1).ok_or(MISMATCH)?;
        let mut header = [0; 8];
        file.read_exact(&mut header).map_err(|_| MISMATCH)?;
        let mut bytes = Zeroizing::new(header.to_vec());
        let size = match u32::from_be_bytes(header[..4].try_into().unwrap()) {
            0 => end - at,
            1 => {
                let mut wide = [0; 8];
                file.read_exact(&mut wide).map_err(|_| MISMATCH)?;
                bytes.extend(wide);
                u64::from_be_bytes(wide)
            }
            value => u64::from(value),
        };
        if size < bytes.len() as u64 || size > end - at {
            return Err(MISMATCH);
        }
        if !matches!(
            &header[4..],
            b"mdat" | b"ftyp" | b"styp" | b"free" | b"skip" | b"wide" | b"sidx" | b"ssix"
        ) {
            if size > super::super::LIMIT as u64 {
                return Err(MISMATCH);
            }
            let begin = bytes.len();
            bytes.resize(usize::try_from(size).map_err(|_| MISMATCH)?, 0);
            for chunk in bytes[begin..].chunks_mut(65_536) {
                super::super::cancelled(stop)?;
                file.read_exact(chunk).map_err(|_| MISMATCH)?;
            }
            let boxes =
                mp4::parse_boxes_with_limit(&bytes, METADATA_BOX_LIMIT).map_err(|_| MISMATCH)?;
            for node in mp4::walk(&boxes) {
                super::super::cancelled(stop)?;
                records.node(node, &bytes)?;
            }
        }
        at += size;
        file.seek(SeekFrom::Start(at)).map_err(|_| MISMATCH)?;
    }
    super::super::cancelled(stop)?;
    if FileIdentity::read(&file).map_err(|_| MISMATCH)? != identity
        || file.metadata().map_err(|_| MISMATCH)?.len() != length
        || records.common != 0 && records.encrypted_tracks == 0
    {
        return Err(MISMATCH);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;

    #[test]
    fn indexed_subsegments_have_independent_exact_provider_record_scopes() {
        let owner = crate::native::private_tempdir().unwrap();
        let path = owner.path().join("indexed-media");
        let key = OutputKey::from_hex(
            "000102030405060708090a0b0c0d0e0f",
            "101112131415161718191a1b1c1d1e1f",
        )
        .unwrap();
        let objects = super::super::OutputSignaling::playready("https://example.invalid/license")
            .unwrap()
            .objects(&key, TargetProtection::Cbcs)
            .unwrap();
        let selected = MultiObjects::legacy_playready(objects);
        let record = &selected.playready.as_ref().unwrap().bytes;
        std::fs::write(&path, [record.as_slice(), record.as_slice()].concat()).unwrap();
        let width = record.len() as u64;
        let stop = Stop::new();
        assert_eq!(
            check(&path, Selection::Providers(&selected), &stop),
            Err(MISMATCH)
        );
        for range in [(0, width - 1), (width, width * 2 - 1)] {
            check_range(&path, Some(range), Selection::Providers(&selected), &stop).unwrap();
        }
        for range in [(1, width - 1), (0, width - 2), (0, width * 2)] {
            assert_eq!(
                check_range(&path, Some(range), Selection::Providers(&selected), &stop),
                Err(MISMATCH)
            );
        }
    }

    #[test]
    fn large_sparse_mdat_is_skipped_but_trailing_records_are_checked() {
        let owner = crate::native::private_tempdir().unwrap();
        let path = owner.path().join("opaque-resource");
        let length = 64_u64 * 1024 * 1024;
        let mut file = crate::native::create_private_new(&path).unwrap();
        file.write_all(&1_u32.to_be_bytes()).unwrap();
        file.write_all(b"mdat").unwrap();
        file.write_all(&length.to_be_bytes()).unwrap();
        file.set_len(length).unwrap();
        drop(file);
        let stop = Stop::new();
        check(&path, Selection::FairPlay, &stop).unwrap();
        let key = OutputKey::from_hex(
            "000102030405060708090a0b0c0d0e0f",
            "101112131415161718191a1b1c1d1e1f",
        )
        .unwrap();
        let objects = super::super::OutputSignaling::playready("https://example.invalid/license")
            .unwrap()
            .objects(&key, TargetProtection::Cbcs)
            .unwrap();
        let selected = MultiObjects::legacy_playready(objects);
        std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(&selected.playready.as_ref().unwrap().bytes)
            .unwrap();
        check(&path, Selection::Providers(&selected), &stop).unwrap();
        assert_eq!(check(&path, Selection::FairPlay, &stop), Err(MISMATCH));
        assert_eq!(
            check(
                &path,
                Selection::Default {
                    target: TargetProtection::Cbcs,
                    key: Some(&key)
                },
                &stop
            ),
            Err(MISMATCH)
        );
        stop.set();
        assert_eq!(
            check(&path, Selection::Providers(&selected), &stop),
            Err(ConversionError("cancelled"))
        );
    }

    #[test]
    fn default_common_records_require_the_same_resource_to_declare_the_selected_track() {
        let owner = crate::native::private_tempdir().unwrap();
        let path = owner.path().join("media-with-common-record");
        let key = OutputKey::from_hex(
            "000102030405060708090a0b0c0d0e0f",
            "101112131415161718191a1b1c1d1e1f",
        )
        .unwrap();
        let mut common = vec![1, 0, 0, 0];
        common.extend(hex::decode("1077efecc0b24d02ace33c1e52e2fb4b").unwrap());
        common.extend(1_u32.to_be_bytes());
        common.extend(key.0.id);
        common.extend(0_u32.to_be_bytes());
        let record = [
            (u32::try_from(common.len()).unwrap() + 8)
                .to_be_bytes()
                .as_slice(),
            b"pssh",
            &common,
        ]
        .concat();
        std::fs::write(&path, record).unwrap();
        let stop = Stop::new();
        for target in [TargetProtection::Cenc, TargetProtection::Cbcs] {
            assert_eq!(
                check(
                    &path,
                    Selection::Default {
                        target,
                        key: Some(&key)
                    },
                    &stop
                ),
                Err(MISMATCH)
            );
        }
    }

    #[test]
    fn separate_bounded_metadata_trees_do_not_share_a_new_resource_wide_node_limit() {
        let owner = crate::native::private_tempdir().unwrap();
        let path = owner.path().join("many-small-metadata-trees");
        let children = [0, 0, 0, 8, b'f', b'r', b'e', b'e'].repeat(100);
        let metadata = [
            (u32::try_from(children.len()).unwrap() + 8)
                .to_be_bytes()
                .as_slice(),
            b"moof",
            &children,
        ]
        .concat();
        let mut file = crate::native::create_private_new(&path).unwrap();
        for _ in 0..1001 {
            file.write_all(&metadata).unwrap();
        }
        drop(file);
        check(&path, Selection::FairPlay, &Stop::new()).unwrap();
    }
}
