//! Byte-level MP4 framing and inspection of initialization and media segments.
//!
//! Reads sample entries, protection boxes, PSSH, decode times, and sample counts.
//! Malformed-box handling follows the Bento4 behavior recorded by the fixture tests.

use crate::media::{FragmentBoundary, HexSecret, HlsMediaError, InitProbe, MediaError};
use memchr::memmem;
use std::fmt;
use std::path::Path;
use zeroize::Zeroizing;

pub(crate) mod audio_edit;

/// A box type.
pub type FourCc = [u8; 4];

/// Where a box lies in its file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BoxHeader {
    /// The box type.
    pub kind: FourCc,
    /// The offset of its first byte.
    pub offset: usize,
    /// Its header length (8, 16 with a 64-bit size, plus 16 for `uuid`).
    pub header_size: usize,
    /// Its total length.
    pub size: usize,
}

impl BoxHeader {
    /// The bytes after the header.
    pub fn payload<'a>(&self, data: &'a [u8]) -> &'a [u8] {
        &data[self.offset + self.header_size..self.offset + self.size]
    }

    /// The whole box.
    pub fn bytes<'a>(&self, data: &'a [u8]) -> &'a [u8] {
        &data[self.offset..self.offset + self.size]
    }
}

/// A malformed box structure the walker cannot read past.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct Mp4Error(pub &'static str);

impl From<Mp4Error> for MediaError {
    /// A structure the walker cannot read is malformed media: a `ValueError` with the token.
    fn from(error: Mp4Error) -> Self {
        MediaError::Value(error.0.to_string())
    }
}

/// `split_fragmented_mp4`'s header rules: 8 bytes, a 64-bit size when the 32-bit size is 1,
/// the size at least the header and within the file (a zero size is invalid).
fn split_header(data: &[u8], pos: usize) -> Result<BoxHeader, HlsMediaError> {
    let invalid = || HlsMediaError::new("invalid-normalized-mp4");
    let fixed = data.get(pos..pos + 8).ok_or_else(invalid)?;
    let mut size = u64::from(u32::from_be_bytes([fixed[0], fixed[1], fixed[2], fixed[3]]));
    let kind = [fixed[4], fixed[5], fixed[6], fixed[7]];
    let mut header = 8u64;
    if size == 1 {
        let large = data.get(pos + 8..pos + 16).ok_or_else(invalid)?;
        size = u64::from_be_bytes(large.try_into().expect("eight bytes"));
        header = 16;
    }
    let remaining = u64::try_from(data.len() - pos).expect("a length fits u64");
    if size < header || size > remaining {
        return Err(invalid());
    }
    Ok(BoxHeader {
        kind,
        offset: pos,
        header_size: usize::try_from(header).expect("8 or 16"),
        size: usize::try_from(size).expect("within the data"),
    })
}

/// The framing split of `split_fragmented_mp4` over bytes: `ftyp` and `moov` make the
/// initialization (exactly two of them), each `moof` with the `mdat`s after it one media
/// segment, `mfra` / `free` / `skip` dropped, any other box `unsupported-normalized-mp4-box`.
/// No box byte changes.
pub fn split_fragmented(data: &[u8]) -> Result<(Vec<u8>, Vec<Vec<u8>>), HlsMediaError> {
    let mut pos = 0;
    let mut initialization: Vec<&[u8]> = Vec::new();
    let mut media: Vec<Vec<u8>> = Vec::new();
    let mut current: Vec<&[u8]> = Vec::new();
    while pos < data.len() {
        let header = split_header(data, pos)?;
        let bytes = header.bytes(data);
        match &header.kind {
            b"ftyp" | b"moov" => initialization.push(bytes),
            b"moof" => {
                if !current.is_empty() {
                    media.push(current.concat());
                }
                current = vec![bytes];
            }
            b"mdat" if !current.is_empty() => current.push(bytes),
            b"mfra" | b"free" | b"skip" => {}
            _ => return Err(HlsMediaError::new("unsupported-normalized-mp4-box")),
        }
        pos += header.size;
    }
    if !current.is_empty() {
        media.push(current.concat());
    }
    if media.is_empty() || initialization.len() != 2 {
        return Err(HlsMediaError::new("text-segment-correspondence-mismatch"));
    }
    Ok((initialization.concat(), media))
}

/// `media::split_fragmented_mp4`: split ffmpeg's fragmented output into the init (written to
/// `init`) and the media segments (returned), without changing a byte.
pub fn split_fragmented_mp4(source: &Path, init: &Path) -> Result<Vec<Vec<u8>>, MediaError> {
    let data = std::fs::read(source)?;
    let (initialization, media) = split_fragmented(&data)?;
    crate::media::write_output_file(init, &initialization)?;
    Ok(media)
}

/// `media::pssh_box_present`: whether the exact `pssh_box` bytes appear in the init
/// segment (Shaka's `--pssh` embeds the box verbatim); an empty box is always present.
pub fn pssh_box_present(init_segment_path: &Path, pssh_box: &[u8]) -> Result<bool, MediaError> {
    let data = std::fs::read(init_segment_path)?;
    Ok(memmem::find(&data, pssh_box).is_some())
}

/// A box and the boxes inside it (for the containers the walker opens).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Mp4Box {
    /// Where it lies.
    pub header: BoxHeader,
    /// Its children, in file order.
    pub children: Vec<Mp4Box>,
}

impl Mp4Box {
    /// The box type.
    pub fn kind(&self) -> FourCc {
        self.header.kind
    }
}

/// Nesting beyond this is refused rather than recursed into.
const MAX_DEPTH: usize = 64;

const PLAIN_CONTAINERS: &[&FourCc] = &[
    b"moov", b"trak", b"mdia", b"minf", b"stbl", b"dinf", b"edts", b"udta", b"mvex", b"moof",
    b"traf", b"mfra", b"sinf", b"schi",
];
const VISUAL_ENTRIES: &[&FourCc] = &[
    b"avc1", b"avc2", b"avc3", b"avc4", b"hvc1", b"hev1", b"dvh1", b"dvhe", b"dva1", b"dvav",
    b"vp08", b"vp09", b"av01", b"mp4v", b"encv",
];
const AUDIO_ENTRIES: &[&FourCc] = &[
    b"mp4a", b"enca", b"ac-3", b"ec-3", b"ac-4", b"Opus", b"fLaC", b"alac", b"dtsc", b"dtse",
    b"dtsh", b"dtsl", b"mha1", b"mhm1",
];

/// The walker's header rules: a 64-bit size when the 32-bit size is 1, a zero size reaching the
/// end of the enclosing box, 16 more header bytes for `uuid`.
fn walk_header(data: &[u8], pos: usize, end: usize) -> Result<BoxHeader, Mp4Error> {
    let truncated = Mp4Error("mp4-box-truncated");
    let fixed = data
        .get(pos..pos + 8)
        .filter(|_| pos + 8 <= end)
        .ok_or(truncated)?;
    let size32 = u32::from_be_bytes([fixed[0], fixed[1], fixed[2], fixed[3]]);
    let kind = [fixed[4], fixed[5], fixed[6], fixed[7]];
    let available = end - pos;
    let (mut header, size) = match size32 {
        0 => (8, available),
        1 => {
            let large = data
                .get(pos + 8..pos + 16)
                .filter(|_| pos + 16 <= end)
                .ok_or(truncated)?;
            let size = u64::from_be_bytes(large.try_into().expect("eight bytes"));
            (16, usize::try_from(size).unwrap_or(usize::MAX))
        }
        size => (8, usize::try_from(size).expect("u32 fits usize")),
    };
    if &kind == b"uuid" {
        header += 16;
    }
    if size < header || size > available {
        return Err(Mp4Error("mp4-box-size-invalid"));
    }
    Ok(BoxHeader {
        kind,
        offset: pos,
        header_size: header,
        size,
    })
}

/// Where a box's children start (relative to the box), for the containers the walker opens.
fn children_offset(data: &[u8], header: &BoxHeader) -> Option<usize> {
    let kind = &header.kind;
    let base = header.header_size;
    if PLAIN_CONTAINERS.contains(&kind) {
        return Some(base);
    }
    match kind {
        b"meta" => return Some(base + 4),
        b"stsd" | b"dref" | b"wvtt" => return Some(base + 8),
        _ => {}
    }
    if VISUAL_ENTRIES.contains(&kind) {
        return Some(base + 78);
    }
    if AUDIO_ENTRIES.contains(&kind) {
        // QuickTime sound sample description versions 1 and 2 carry 16 / 36 more bytes.
        let payload = header.payload(data);
        let version = payload.get(8..10).map(|v| u16::from_be_bytes([v[0], v[1]]));
        let extra = match version {
            Some(1) => 16,
            Some(2) => 36,
            _ => 0,
        };
        return Some(base + 28 + extra);
    }
    None
}

fn parse_range(
    data: &[u8],
    start: usize,
    end: usize,
    depth: usize,
) -> Result<Vec<Mp4Box>, Mp4Error> {
    if depth > MAX_DEPTH {
        return Err(Mp4Error("mp4-box-nesting-too-deep"));
    }
    let mut boxes = Vec::new();
    let mut pos = start;
    while pos < end {
        let header = walk_header(data, pos, end)?;
        // A sample entry too short for its fixed fields has no children (Bento4: opaque).
        let children = match children_offset(data, &header) {
            Some(offset) if offset <= header.size => {
                parse_range(data, pos + offset, pos + header.size, depth + 1)?
            }
            _ => Vec::new(),
        };
        pos += header.size;
        boxes.push(Mp4Box { header, children });
    }
    Ok(boxes)
}

/// Every box of `data`, the containers opened (`moov`, `trak`, `mdia`, `minf`, `stbl`, `stsd`
/// and its sample entries, `sinf`, `schi`, `mvex`, `moof`, `traf`, `edts`, `dinf`, `udta`,
/// `meta`, `mfra`).
pub fn parse_boxes(data: &[u8]) -> Result<Vec<Mp4Box>, Mp4Error> {
    parse_range(data, 0, data.len(), 0)
}

/// The boxes of a tree in depth-first file order (mp4dump's order).
pub fn walk(boxes: &[Mp4Box]) -> Vec<&Mp4Box> {
    let mut out = Vec::new();
    let mut stack: Vec<&Mp4Box> = boxes.iter().rev().collect();
    while let Some(found) = stack.pop() {
        out.push(found);
        stack.extend(found.children.iter().rev());
    }
    out
}

/// A full box's version, flags and body; `None` when shorter than four bytes.
fn full_box(payload: &[u8]) -> Option<(u8, u32, &[u8])> {
    let fixed = payload.get(..4)?;
    Some((
        fixed[0],
        u32::from_be_bytes([0, fixed[1], fixed[2], fixed[3]]),
        &payload[4..],
    ))
}

fn be_u32(bytes: &[u8], at: usize) -> Option<u32> {
    bytes
        .get(at..at + 4)
        .map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
}

fn be_u64(bytes: &[u8], at: usize) -> Option<u64> {
    bytes
        .get(at..at + 8)
        .map(|b| u64::from_be_bytes(b.try_into().expect("eight bytes")))
}

/// A 4CC as mp4dump prints it: up to the first byte that is not printable ASCII.
pub fn fourcc_text(code: &FourCc) -> String {
    code.iter()
        .take_while(|byte| (0x20..=0x7e).contains(*byte))
        .map(|byte| char::from(*byte))
        .collect()
}

/// A `tenc` box (ISO/IEC 23001-7). Holds the constant IV: `Debug` prints its length only and
/// the bytes are wiped on drop.
#[derive(Clone, PartialEq, Eq)]
pub struct Tenc {
    /// The box version (1 carries the pattern).
    pub version: u8,
    /// `default_crypt_byte_block` (0 for version 0).
    pub default_crypt_byte_block: u8,
    /// `default_skip_byte_block` (0 for version 0).
    pub default_skip_byte_block: u8,
    /// `default_isProtected`.
    pub default_is_protected: u8,
    /// `default_Per_Sample_IV_Size`.
    pub default_per_sample_iv_size: u8,
    /// `default_KID` (public).
    pub default_kid: [u8; 16],
    /// `default_constant_IV`, present when the per-sample IV size is 0 (as Bento4 reads it,
    /// whatever `default_isProtected` says).
    pub default_constant_iv: Option<Zeroizing<Vec<u8>>>,
}

impl fmt::Debug for Tenc {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Tenc")
            .field("version", &self.version)
            .field("default_crypt_byte_block", &self.default_crypt_byte_block)
            .field("default_skip_byte_block", &self.default_skip_byte_block)
            .field("default_is_protected", &self.default_is_protected)
            .field(
                "default_per_sample_iv_size",
                &self.default_per_sample_iv_size,
            )
            .field(
                "default_constant_iv_len",
                &self.default_constant_iv.as_ref().map(|iv| iv.len()),
            )
            .finish_non_exhaustive()
    }
}

impl Tenc {
    /// Parse a `tenc` payload as Bento4 does; `None` (an opaque box to mp4dump) when it is
    /// short or its constant IV is longer than 16 bytes or than the box.
    pub fn parse(payload: &[u8]) -> Option<Tenc> {
        let (version, _, body) = full_box(payload)?;
        let fixed = body.get(..20)?;
        let (crypt, skip) = if version == 0 {
            (0, 0)
        } else {
            (fixed[1] >> 4, fixed[1] & 0x0f)
        };
        let per_sample = fixed[3];
        let default_constant_iv = if per_sample == 0 {
            let size = usize::from(*body.get(20)?);
            if size > 16 {
                return None;
            }
            Some(Zeroizing::new(body.get(21..21 + size)?.to_vec()))
        } else {
            None
        };
        Some(Tenc {
            version,
            default_crypt_byte_block: crypt,
            default_skip_byte_block: skip,
            default_is_protected: fixed[2],
            default_per_sample_iv_size: per_sample,
            default_kid: fixed[4..20].try_into().expect("sixteen bytes"),
            default_constant_iv,
        })
    }
}

/// A `schm` box.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Schm {
    /// `scheme_type` (`cenc`, `cbcs`).
    pub scheme_type: FourCc,
    /// `scheme_version`.
    pub scheme_version: u32,
    /// `scheme_uri`, when flag 1 is set (up to its NUL).
    pub scheme_uri: Option<Vec<u8>>,
}

impl Schm {
    /// Parse a `schm` payload; `None` when short (an opaque box to mp4dump).
    pub fn parse(payload: &[u8]) -> Option<Schm> {
        let (_, flags, body) = full_box(payload)?;
        let scheme_type = body.get(..4)?.try_into().expect("four bytes");
        let scheme_version = be_u32(body, 4)?;
        let scheme_uri = (flags & 1 == 1).then(|| {
            let uri = &body[8..];
            uri[..memchr::memchr(0, uri).unwrap_or(uri.len())].to_vec()
        });
        Some(Schm {
            scheme_type,
            scheme_version,
            scheme_uri,
        })
    }
}

/// A `pssh` box. `Debug` prints the data's length only: a `PlayReady` object carries the
/// `LA_URL`, which a clear-HLS job treats as secret.
#[derive(Clone, PartialEq, Eq)]
pub struct Pssh {
    /// The box version (1 lists KIDs).
    pub version: u8,
    /// The DRM system id.
    pub system_id: [u8; 16],
    /// The KIDs of a version 1 box.
    pub kids: Vec<[u8; 16]>,
    /// The system-specific data.
    pub data: Vec<u8>,
}

impl fmt::Debug for Pssh {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Pssh")
            .field("version", &self.version)
            .field("system_id", &hex::encode(self.system_id))
            .field("kids", &self.kids.len())
            .field("data_len", &self.data.len())
            .finish()
    }
}

impl Pssh {
    /// Parse a `pssh` payload; `None` when short or inconsistent.
    pub fn parse(payload: &[u8]) -> Option<Pssh> {
        let (version, _, body) = full_box(payload)?;
        let system_id = body.get(..16)?.try_into().expect("sixteen bytes");
        let mut at = 16;
        let mut kids = Vec::new();
        if version > 0 {
            let count = usize::try_from(be_u32(body, at)?).ok()?;
            at += 4;
            for _ in 0..count {
                kids.push(body.get(at..at + 16)?.try_into().expect("sixteen bytes"));
                at += 16;
            }
        }
        let size = usize::try_from(be_u32(body, at)?).ok()?;
        at += 4;
        let data = body.get(at..at + size)?.to_vec();
        Some(Pssh {
            version,
            system_id,
            kids,
            data,
        })
    }
}

/// A `tfdt` box.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Tfdt {
    /// The box version (1: 64-bit time).
    pub version: u8,
    /// `baseMediaDecodeTime`.
    pub base_media_decode_time: u64,
}

/// The bytes from a box's payload to the end of the file: Bento4 reads a `tfdt` / `trun`
/// field by field from its stream without bounding the reads by the box, so a short box's
/// field can continue into the bytes after it. The native reader deterministically returns
/// zero for a field cut by EOF. This does not claim a defined value for external tools'
/// uninitialized optional fields in malformed atoms.
struct FieldReader<'a> {
    data: &'a [u8],
    at: usize,
}

impl FieldReader<'_> {
    fn u32(&mut self) -> u32 {
        let value = be_u32(self.data, self.at).unwrap_or(0);
        self.at += 4;
        value
    }

    fn u64(&mut self) -> u64 {
        let value = be_u64(self.data, self.at).unwrap_or(0);
        self.at += 8;
        value
    }
}

/// A full box's version and flags from its payload and a reader of the fields after them;
/// `None` when the payload has no version and flags (an opaque box).
fn full_fields<'a>(data: &'a [u8], header: &BoxHeader) -> Option<(u8, u32, FieldReader<'a>)> {
    let (version, flags, _) = full_box(header.payload(data))?;
    let at = header.offset + header.header_size + 4;
    Some((version, flags, FieldReader { data, at }))
}

impl Tfdt {
    /// Read a `tfdt` box of `data` as Bento4 does (see [`FieldReader`]); `None` without
    /// version and flags.
    pub fn read(data: &[u8], header: &BoxHeader) -> Option<Tfdt> {
        let (version, _, mut fields) = full_fields(data, header)?;
        let base_media_decode_time = if version == 1 {
            fields.u64()
        } else {
            u64::from(fields.u32())
        };
        Some(Tfdt {
            version,
            base_media_decode_time,
        })
    }
}

/// A `trun` box's header fields.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Trun {
    /// The box version.
    pub version: u8,
    /// The box flags.
    pub flags: u32,
    /// The declared count, or zero when the declared records exceed the native box budget.
    pub sample_count: u32,
    /// `data_offset`, when flag 0x1 is set.
    pub data_offset: Option<i32>,
    /// `first_sample_flags`, when flag 0x4 is set.
    pub first_sample_flags: Option<u32>,
}

impl Trun {
    /// Read header fields with deterministic native EOF handling (see [`FieldReader`]);
    /// `None` without version and flags.
    pub fn read(data: &[u8], header: &BoxHeader) -> Option<Trun> {
        let (version, flags, mut fields) = full_fields(data, header)?;
        let declared = fields.u32();
        let data_offset =
            (flags & 0x1 != 0).then(|| i32::from_be_bytes(fields.u32().to_be_bytes()));
        let first_sample_flags = (flags & 0x4 != 0).then(|| fields.u32());
        let optional = u32::from(flags & 0x1 != 0) + u32::from(flags & 0x4 != 0);
        let record_fields = [0x100, 0x200, 0x400, 0x800]
            .iter()
            .map(|bit| u64::from(flags & bit != 0))
            .sum::<u64>();
        // Retain the native reader's 32-bit budget arithmetic, including short-box wrapping.
        let size = u32::try_from(header.size).unwrap_or(u32::MAX);
        let available = size
            .wrapping_sub(12)
            .wrapping_sub(4)
            .wrapping_sub(optional * 4);
        let sample_count = if record_fields * u64::from(declared) * 4 > u64::from(available) {
            0
        } else {
            declared
        };
        Some(Trun {
            version,
            flags,
            sample_count,
            data_offset,
            first_sample_flags,
        })
    }
}

/// A sample entry of an `stsd` box and its protection boxes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SampleEntry {
    /// The entry type (`avc1`, `encv`, `mp4a`, …).
    pub format: FourCc,
    /// `sinf/frma`'s original format of a protected entry.
    pub original_format: Option<FourCc>,
    /// `sinf/schm`.
    pub scheme: Option<Schm>,
    /// `sinf/schi/tenc`.
    pub tenc: Option<Tenc>,
}

/// The first box of `kind` among `boxes` (not descending).
fn child(boxes: &[Mp4Box], kind: FourCc) -> Option<&Mp4Box> {
    boxes.iter().find(|found| found.header.kind == kind)
}

/// Every sample entry of every `moov/trak/mdia/minf/stbl/stsd`, in file order.
pub fn sample_entries(data: &[u8]) -> Result<Vec<SampleEntry>, Mp4Error> {
    let boxes = parse_boxes(data)?;
    let mut entries = Vec::new();
    for stsd in walk(&boxes)
        .into_iter()
        .filter(|found| &found.kind() == b"stsd")
    {
        for entry in &stsd.children {
            let sinf = child(&entry.children, *b"sinf");
            let inside = |kind: FourCc| sinf.and_then(|sinf| child(&sinf.children, kind));
            entries.push(SampleEntry {
                format: entry.kind(),
                original_format: inside(*b"frma").and_then(|frma| {
                    frma.header
                        .payload(data)
                        .get(..4)
                        .map(|b| [b[0], b[1], b[2], b[3]])
                }),
                scheme: inside(*b"schm").and_then(|schm| Schm::parse(schm.header.payload(data))),
                tenc: inside(*b"schi")
                    .and_then(|schi| child(&schi.children, *b"tenc"))
                    .and_then(|tenc| Tenc::parse(tenc.header.payload(data))),
            });
        }
    }
    Ok(entries)
}

/// Every `pssh` box (`moov/pssh`, `moof/pssh`), in file order.
pub fn pssh_boxes(data: &[u8]) -> Result<Vec<Pssh>, Mp4Error> {
    let boxes = parse_boxes(data)?;
    Ok(walk(&boxes)
        .into_iter()
        .filter(|found| &found.kind() == b"pssh")
        .filter_map(|found| Pssh::parse(found.header.payload(data)))
        .collect())
}

fn first_of(boxes: &[Mp4Box], kind: FourCc) -> Option<&Mp4Box> {
    walk(boxes).into_iter().find(|found| found.kind() == kind)
}

/// The hex of a constant IV as `parse_*` reads mp4dump's bracketed bytes: an empty IV prints
/// as `[]`, which Python's `_hex_from_bracketed_bytes` rejects (`ValueError`).
fn constant_iv_hex(iv: &[u8]) -> Result<HexSecret, MediaError> {
    if iv.is_empty() {
        return Err(MediaError::Value("not a bracketed byte array".to_string()));
    }
    Ok(HexSecret::new(hex::encode(iv)))
}

/// `parse_init_probe` over the boxes themselves: the first `schm`'s scheme type; the first
/// `tenc`'s KID, per-sample IV size and (size 0) constant IV. A box Bento4 cannot parse reads
/// as absent.
pub fn init_probe(data: &[u8]) -> Result<InitProbe, MediaError> {
    let boxes = parse_boxes(data)?;
    let mut probe = InitProbe::default();
    if let Some(schm) = first_of(&boxes, *b"schm") {
        probe.scheme_type =
            Schm::parse(schm.header.payload(data)).map(|schm| fourcc_text(&schm.scheme_type));
    }
    if let Some(tenc) =
        first_of(&boxes, *b"tenc").and_then(|tenc| Tenc::parse(tenc.header.payload(data)))
    {
        probe.per_sample_iv_size = Some(i64::from(tenc.default_per_sample_iv_size));
        probe.default_kid_hex = Some(hex::encode(tenc.default_kid));
        if let Some(iv) = &tenc.default_constant_iv {
            probe.constant_iv_hex = Some(constant_iv_hex(iv)?);
        }
    }
    Ok(probe)
}

/// `parse_tenc_constant_iv_hex` over the boxes: the first `tenc`'s constant IV, `None` for
/// per-sample IVs, without a `tenc`, or when Bento4 could not parse it.
pub fn tenc_constant_iv_hex(data: &[u8]) -> Result<Option<HexSecret>, MediaError> {
    let boxes = parse_boxes(data)?;
    let Some(tenc) =
        first_of(&boxes, *b"tenc").and_then(|tenc| Tenc::parse(tenc.header.payload(data)))
    else {
        return Ok(None);
    };
    tenc.default_constant_iv
        .as_ref()
        .map(|iv| constant_iv_hex(iv))
        .transpose()
}

/// `parse_fragment_boundary` over the boxes: the first `tfdt`'s base media decode time and the
/// first `trun`'s sample count as Bento4 reads them (a box without version and flags is
/// skipped), `None` unless both exist.
pub fn fragment_boundary(data: &[u8]) -> Result<Option<FragmentBoundary>, MediaError> {
    let boxes = parse_boxes(data)?;
    let mut tfdt = None;
    let mut sample_count = None;
    for found in walk(&boxes) {
        match &found.kind() {
            b"tfdt" if tfdt.is_none() => tfdt = Tfdt::read(data, &found.header),
            b"trun" if sample_count.is_none() => {
                sample_count = Trun::read(data, &found.header).map(|trun| trun.sample_count);
            }
            _ => {}
        }
    }
    Ok(tfdt
        .zip(sample_count)
        .map(|(tfdt, count)| FragmentBoundary::new(tfdt.base_media_decode_time, count)))
}

/// `initialization segment validation`'s pattern read: the first `tenc`'s
/// `(default_crypt_byte_block, default_skip_byte_block)`, `(0, 0)` for version 0, an unreadable
/// `tenc` or none.
pub fn tenc_pattern(data: &[u8]) -> Result<(u8, u8), MediaError> {
    let boxes = parse_boxes(data)?;
    Ok(first_of(&boxes, *b"tenc")
        .and_then(|tenc| Tenc::parse(tenc.header.payload(data)))
        .map_or((0, 0), |tenc| {
            (tenc.default_crypt_byte_block, tenc.default_skip_byte_block)
        }))
}
