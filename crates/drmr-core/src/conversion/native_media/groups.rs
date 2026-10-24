//! Resolve known recovery groups instead of discarding fragment sample-group declarations.
use super::read::{count, full, integer, signed16};
use super::{BAD, ConversionError, UNSUPPORTED};
use crate::mp4::Mp4Box;

#[derive(Default)]
struct Description {
    distances: Vec<i16>,
    default: u32,
}
#[derive(Default)]
pub(super) struct Descriptions {
    roll: Description,
    prol: Description,
}
fn group<'a>(
    nodes: &'a [Mp4Box],
    data: &[u8],
    box_kind: [u8; 4],
    group_kind: [u8; 4],
) -> Result<Option<&'a Mp4Box>, ConversionError> {
    let mut found = None;
    for node in nodes.iter().filter(|node| node.kind() == box_kind) {
        let payload = node.header.payload(data);
        if !matches!(payload.get(4..8), Some(b"roll" | b"prol")) {
            return Err(UNSUPPORTED);
        }
        if payload.get(4..8) == Some(group_kind.as_slice()) && found.replace(node).is_some() {
            return Err(BAD);
        }
    }
    Ok(found)
}
fn descriptions(
    nodes: &[Mp4Box],
    data: &[u8],
    kind: [u8; 4],
) -> Result<Description, ConversionError> {
    let Some(node) = group(nodes, data, *b"sgpd", kind)? else {
        return Ok(Description::default());
    };
    let p = node.header.payload(data);
    let version = *p.first().ok_or(BAD)?;
    if version > 2 {
        return Err(UNSUPPORTED);
    }
    full(p, version, 0)?;
    let mut at = 8;
    let width = if version == 0 {
        2
    } else {
        let width = integer(p, at)?;
        at += 4;
        if !matches!(width, 0 | 2) {
            return Err(UNSUPPORTED);
        }
        width
    };
    let default = if version == 2 {
        let value = integer(p, at)?;
        at += 4;
        value
    } else {
        0
    };
    let total = usize::try_from(integer(p, at)?).map_err(|_| BAD)?;
    at += 4;
    let record = if width == 0 { 6 } else { 2 };
    if total.checked_mul(record).and_then(|n| n.checked_add(at)) != Some(p.len()) {
        return Err(BAD);
    }
    let mut distances = Vec::with_capacity(total);
    for _ in 0..total {
        if width == 0 {
            if integer(p, at)? != 2 {
                return Err(BAD);
            }
            at += 4;
        }
        distances.push(signed16(p, at)?);
        at += 2;
    }
    if default as usize > total {
        return Err(BAD);
    }
    Ok(Description { distances, default })
}
impl Descriptions {
    pub fn parse(nodes: &[Mp4Box], data: &[u8]) -> Result<Self, ConversionError> {
        // Census both box kinds even when there is no description for a mapping.
        group(nodes, data, *b"sbgp", *b"roll")?;
        group(nodes, data, *b"sbgp", *b"prol")?;
        Ok(Self {
            roll: descriptions(nodes, data, *b"roll")?,
            prol: descriptions(nodes, data, *b"prol")?,
        })
    }
}
struct Assignment {
    end: u64,
    distance: Option<i16>,
}
struct Mapping {
    entries: Vec<Assignment>,
    default: Option<i16>,
}
impl Mapping {
    fn new(
        base: &Description,
        local: &Description,
        nodes: &[Mp4Box],
        data: &[u8],
        kind: [u8; 4],
        samples: u64,
    ) -> Result<Self, ConversionError> {
        let lookup = |index: u32| -> Result<Option<i16>, ConversionError> {
            if index == 0 {
                return Ok(None);
            }
            let (description, index) = if index >= 0x10000 {
                (local, index - 0x10000)
            } else {
                (base, index)
            };
            description
                .distances
                .get(index.checked_sub(1).ok_or(BAD)? as usize)
                .copied()
                .map(Some)
                .ok_or(BAD)
        };
        let default = if local.default != 0 {
            local.distances.get(local.default as usize - 1).copied()
        } else if base.default != 0 {
            base.distances.get(base.default as usize - 1).copied()
        } else {
            None
        };
        let Some(node) = group(nodes, data, *b"sbgp", kind)? else {
            return Ok(Self {
                entries: Vec::new(),
                default,
            });
        };
        let p = node.header.payload(data);
        let version = *p.first().ok_or(BAD)?;
        if version > 1 {
            return Err(UNSUPPORTED);
        }
        full(p, version, 0)?;
        let at = if version == 0 {
            8
        } else {
            if integer(p, 8)? != 0 {
                return Err(UNSUPPORTED);
            }
            12
        };
        let total = count(p, at, at + 4, 8)?;
        let mut entries = Vec::with_capacity(total);
        let mut end = 0_u64;
        for row in p[at + 4..].as_chunks::<8>().0 {
            let count = integer(row, 0)?;
            if count == 0 {
                return Err(BAD);
            }
            end = end
                .checked_add(u64::from(count))
                .filter(|end| *end <= samples)
                .ok_or(BAD)?;
            let index = integer(row, 4)?;
            let distance = if index == 0 { default } else { lookup(index)? };
            entries.push(Assignment { end, distance });
        }
        Ok(Self { entries, default })
    }
    fn at(&self, ordinal: u64) -> Option<i16> {
        let at = self.entries.partition_point(|entry| entry.end <= ordinal);
        self.entries
            .get(at)
            .map_or(self.default, |entry| entry.distance)
    }
}
pub(super) struct Resolver {
    roll: Mapping,
    prol: Mapping,
}
impl Resolver {
    pub fn new(
        base: &Descriptions,
        nodes: &[Mp4Box],
        data: &[u8],
        samples: u64,
        fragment: bool,
    ) -> Result<Self, ConversionError> {
        let local = if fragment {
            Descriptions::parse(nodes, data)?
        } else {
            Descriptions::default()
        };
        Ok(Self {
            roll: Mapping::new(&base.roll, &local.roll, nodes, data, *b"roll", samples)?,
            prol: Mapping::new(&base.prol, &local.prol, nodes, data, *b"prol", samples)?,
        })
    }
    pub fn at(&self, ordinal: u64) -> (Option<i16>, Option<i16>) {
        (self.roll.at(ordinal), self.prol.at(ordinal))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mp4;
    fn boxed(kind: [u8; 4], payload: &[u8]) -> Vec<u8> {
        let mut bytes = u32::try_from(payload.len() + 8)
            .unwrap()
            .to_be_bytes()
            .to_vec();
        bytes.extend(kind);
        bytes.extend(payload);
        bytes
    }
    fn description(kind: [u8; 4], distance: i16) -> Vec<u8> {
        let mut p = vec![1, 0, 0, 0];
        p.extend(kind);
        p.extend(2_u32.to_be_bytes());
        p.extend(1_u32.to_be_bytes());
        p.extend(distance.to_be_bytes());
        boxed(*b"sgpd", &p)
    }
    fn mapping(count: u32, index: u32) -> Vec<u8> {
        let mut p = vec![0, 0, 0, 0];
        p.extend(b"roll");
        p.extend(1_u32.to_be_bytes());
        p.extend(count.to_be_bytes());
        p.extend(index.to_be_bytes());
        boxed(*b"sbgp", &p)
    }
    #[test]
    fn movie_and_fragment_description_indices_have_distinct_exact_meanings() {
        let base_data = description(*b"roll", -1);
        let base_nodes = mp4::parse_boxes(&base_data).unwrap();
        let base = Descriptions::parse(&base_nodes, &base_data).unwrap();
        let local = [description(*b"roll", -2), mapping(2, 0x0001_0001)].concat();
        let nodes = mp4::parse_boxes(&local).unwrap();
        let resolver = Resolver::new(&base, &nodes, &local, 3, true).unwrap();
        assert_eq!(resolver.at(0), (Some(-2), None));
        assert_eq!(resolver.at(1), (Some(-2), None));
        assert_eq!(resolver.at(2), (None, None));
        let movie = mapping(3, 1);
        let nodes = mp4::parse_boxes(&movie).unwrap();
        assert_eq!(
            Resolver::new(&base, &nodes, &movie, 3, true).unwrap().at(2),
            (Some(-1), None)
        );
    }
    #[test]
    fn unknown_groups_bad_indices_overcounts_and_duplicates_are_not_ignored() {
        let bytes = description(*b"seig", -1);
        let nodes = mp4::parse_boxes(&bytes).unwrap();
        assert!(Descriptions::parse(&nodes, &bytes).is_err());
        let bytes = [description(*b"roll", -1), description(*b"roll", -2)].concat();
        let nodes = mp4::parse_boxes(&bytes).unwrap();
        assert!(Descriptions::parse(&nodes, &bytes).is_err());
        let base = Descriptions::default();
        for (count, index) in [(4, 0), (1, 1), (1, 0x0001_0000), (1, 0x0001_0001)] {
            let bytes = mapping(count, index);
            let nodes = mp4::parse_boxes(&bytes).unwrap();
            assert!(Resolver::new(&base, &nodes, &bytes, 3, true).is_err());
        }
    }
}
