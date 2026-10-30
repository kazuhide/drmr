//! Shared location checks over an already bounded metadata tree, not sample evidence.
use super::{ConversionError, UNSUPPORTED, read::optional};
use crate::mp4::{self, Mp4Box};
use std::collections::BTreeSet;

pub(super) fn semantic_children(container: &Mp4Box) -> Result<(), ConversionError> {
    let known: &[[u8; 4]] = match &container.kind() {
        b"moov" => &[
            *b"mvhd", *b"trak", *b"mvex", *b"iods", *b"pssh", *b"udta", *b"meta",
        ],
        b"trak" => &[
            *b"tkhd", *b"mdia", *b"edts", *b"senc", *b"saiz", *b"saio", *b"udta", *b"meta",
        ],
        b"mdia" => &[*b"mdhd", *b"hdlr", *b"minf"],
        b"minf" => &[*b"smhd", *b"vmhd", *b"dinf", *b"stbl"],
        b"dinf" => &[*b"dref"],
        b"mvex" => &[*b"mehd", *b"trex"],
        b"stbl" => &[
            *b"stsd", *b"stts", *b"ctts", *b"stsc", *b"stsz", *b"stco", *b"co64", *b"stss",
            *b"sdtp", *b"senc", *b"saiz", *b"saio", *b"sgpd", *b"sbgp",
        ],
        b"moof" => &[*b"mfhd", *b"traf"],
        b"traf" => &[
            *b"tfhd", *b"tfdt", *b"trun", *b"senc", *b"saiz", *b"saio", *b"sgpd", *b"sbgp",
        ],
        b"sinf" => &[*b"frma", *b"schm", *b"schi"],
        b"schi" => &[*b"tenc"],
        b"edts" => &[*b"elst"],
        _ => return Ok(()),
    };
    let padding = !matches!(
        &container.kind(),
        b"moof" | b"traf" | b"sinf" | b"schi" | b"edts"
    );
    if container.children.iter().any(|node| {
        !(known.contains(&node.kind()) || padding && matches!(&node.kind(), b"free" | b"skip"))
    }) {
        return Err(UNSUPPORTED);
    }
    Ok(())
}
fn descendant<'a>(
    root: &'a Mp4Box,
    path: &[[u8; 4]],
) -> Result<Option<&'a Mp4Box>, ConversionError> {
    let mut current = root;
    for kind in path {
        let Some(next) = optional(&current.children, *kind)? else {
            return Ok(None);
        };
        current = next;
    }
    Ok(Some(current))
}
fn initialization_offsets(
    moov: &Mp4Box,
    allowed: &mut BTreeSet<usize>,
) -> Result<(), ConversionError> {
    if let Some(mvex) = optional(&moov.children, *b"mvex")? {
        allowed.extend(mvex.children.iter().map(|node| node.header.offset));
    }
    for track in moov.children.iter().filter(|node| node.kind() == *b"trak") {
        if let Some(edits) = optional(&track.children, *b"edts")? {
            allowed.insert(edits.header.offset);
            allowed.extend(edits.children.iter().map(|node| node.header.offset));
        }
        allowed.extend(
            track
                .children
                .iter()
                .filter(|node| matches!(&node.kind(), b"senc" | b"saiz" | b"saio"))
                .map(|node| node.header.offset),
        );
        let Some(table) = descendant(track, &[*b"mdia", *b"minf", *b"stbl"])? else {
            continue;
        };
        allowed.extend(
            table
                .children
                .iter()
                .filter(|node| {
                    matches!(
                        &node.kind(),
                        b"senc" | b"saiz" | b"saio" | b"sgpd" | b"sbgp"
                    )
                })
                .map(|node| node.header.offset),
        );
        let Some(stsd) = optional(&table.children, *b"stsd")? else {
            continue;
        };
        let Some(entry) = stsd.children.first() else {
            continue;
        };
        if let Some(sinf) = optional(&entry.children, *b"sinf")? {
            allowed.extend(
                mp4::walk(std::slice::from_ref(sinf))
                    .iter()
                    .map(|node| node.header.offset),
            );
        }
    }
    Ok(())
}
/// The native parser supplies a canonical movie or fragment root. Descriptive
/// metadata does not grant a nested sample/edit/group/protection declaration authority.
pub(in crate::conversion) fn census(tree: &[Mp4Box]) -> Result<(), ConversionError> {
    let mut allowed = BTreeSet::new();
    for root in tree {
        if root.kind() == *b"moov" {
            initialization_offsets(root, &mut allowed)?;
        } else if root.kind() == *b"moof" {
            allowed.extend(root.children.iter().map(|node| node.header.offset));
            for traf in root.children.iter().filter(|node| node.kind() == *b"traf") {
                allowed.extend(traf.children.iter().map(|node| node.header.offset));
            }
        }
    }
    for node in mp4::walk(tree) {
        semantic_children(node)?;
        if matches!(
            &node.kind(),
            b"sinf"
                | b"frma"
                | b"schm"
                | b"schi"
                | b"tenc"
                | b"senc"
                | b"saiz"
                | b"saio"
                | b"sgpd"
                | b"sbgp"
                | b"uuid"
                | b"trex"
                | b"tfhd"
                | b"tfdt"
                | b"trun"
                | b"mfhd"
                | b"edts"
                | b"elst"
        ) && !allowed.contains(&node.header.offset)
        {
            return Err(UNSUPPORTED);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn boxed(kind: [u8; 4], payload: &[u8]) -> Vec<u8> {
        [
            u32::try_from(payload.len() + 8)
                .unwrap()
                .to_be_bytes()
                .as_slice(),
            kind.as_slice(),
            payload,
        ]
        .concat()
    }
    fn check(bytes: &[u8]) -> Result<(), ConversionError> {
        census(&mp4::parse_boxes(bytes).unwrap())
    }
    fn movie_with_table(extra_track: &[u8], table: &[u8]) -> Vec<u8> {
        boxed(
            *b"moov",
            &boxed(
                *b"trak",
                &[
                    extra_track,
                    &boxed(*b"mdia", &boxed(*b"minf", &boxed(*b"stbl", table))),
                ]
                .concat(),
            ),
        )
    }
    #[test]
    fn canonical_edit_group_and_protection_locations_keep_the_native_rules() {
        let sinf = boxed(
            *b"sinf",
            &[
                boxed(*b"frma", b"avc1"),
                boxed(*b"schm", &[0; 12]),
                boxed(*b"schi", &boxed(*b"tenc", &[0; 24])),
            ]
            .concat(),
        );
        let entry = boxed(*b"encv", &[vec![0; 78], sinf].concat());
        let table = [
            boxed(*b"stsd", &[vec![0, 0, 0, 0, 0, 0, 0, 1], entry].concat()),
            boxed(*b"sgpd", &[]),
            boxed(*b"sbgp", &[]),
            boxed(*b"saiz", &[]),
            boxed(*b"saio", &[]),
        ]
        .concat();
        let edits = boxed(*b"edts", &boxed(*b"elst", &[]));
        let track = [edits, boxed(*b"senc", &[])].concat();
        check(&movie_with_table(&track, &table)).unwrap();
        check(&boxed(*b"moov", &boxed(*b"mvex", &boxed(*b"trex", &[])))).unwrap();
        for kind in [
            *b"sinf", *b"frma", *b"schm", *b"schi", *b"tenc", *b"senc", *b"saiz", *b"saio",
            *b"sgpd", *b"sbgp", *b"trex", *b"tfhd", *b"tfdt", *b"trun", *b"mfhd", *b"edts",
            *b"elst",
        ] {
            let misplaced = boxed(*b"udta", &boxed(kind, &[]));
            assert_eq!(
                check(&movie_with_table(&misplaced, &table)),
                Err(UNSUPPORTED),
                "{kind:?}"
            );
        }
    }
    #[test]
    fn fragment_and_sample_table_boundaries_do_not_accept_movie_controls_or_padding() {
        let mut children = Vec::new();
        for kind in [
            *b"tfhd", *b"tfdt", *b"trun", *b"senc", *b"saiz", *b"saio", *b"sgpd", *b"sbgp",
        ] {
            children.extend(boxed(kind, &[]));
        }
        let valid = boxed(
            *b"moof",
            &[boxed(*b"mfhd", &[]), boxed(*b"traf", &children)].concat(),
        );
        check(&valid).unwrap();
        for parent in [
            *b"moof", *b"traf", *b"stbl", *b"trak", *b"mdia", *b"minf", *b"dinf", *b"mvex",
        ] {
            assert_eq!(
                check(&boxed(parent, &boxed(*b"iods", &[0; 13]))),
                Err(UNSUPPORTED),
                "{parent:?}"
            );
        }
        for parent in [*b"moof", *b"traf"] {
            for kind in [*b"free", *b"skip"] {
                assert_eq!(check(&boxed(parent, &boxed(kind, &[]))), Err(UNSUPPORTED));
            }
        }
        check(&boxed(*b"moov", &boxed(*b"iods", &[0; 13]))).unwrap();
        check(&boxed(
            *b"stbl",
            &[boxed(*b"free", &[]), boxed(*b"skip", &[])].concat(),
        ))
        .unwrap();
    }
}
