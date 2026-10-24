//! Bounded MP4 initialization and fragment metadata rewriting with streamed payload output.
use super::{
    BAD, ConversionError, MAX_METADATA_BYTES, Scheme, TrackDescriptor, crypto, encrypt, init, read,
};
use crate::{
    concurrency::Stop,
    mp4::{self, Mp4Box},
    native::FileIdentity,
};
use sha2::{Digest as _, Sha256};
use std::{
    fs::File,
    io::Write as _,
    path::{Path, PathBuf},
};
use zeroize::{Zeroize as _, Zeroizing};

const WRITE: ConversionError = ConversionError("output-staging-failed");

pub(super) fn boxed(kind: [u8; 4], payload: &[u8]) -> Result<Vec<u8>, ConversionError> {
    let size = payload
        .len()
        .checked_add(8)
        .filter(|n| *n <= MAX_METADATA_BYTES)
        .ok_or(ConversionError("resource-budget"))?;
    let mut bytes = u32::try_from(size).map_err(|_| BAD)?.to_be_bytes().to_vec();
    bytes.extend(kind);
    bytes.extend(payload);
    Ok(bytes)
}
fn full(kind: [u8; 4], flags: u32, values: &[u8]) -> Result<Vec<u8>, ConversionError> {
    boxed(kind, &[flags.to_be_bytes().as_slice(), values].concat())
}
fn protection(
    track: &TrackDescriptor,
    scheme: Scheme,
    kid: &[u8; 16],
    iv: Option<&[u8; 16]>,
) -> Result<Vec<u8>, ConversionError> {
    let mut tenc = vec![
        0,
        if scheme == Scheme::Cbcs && track.kind == crate::inspection::TrackKind::Video {
            0x19
        } else {
            0
        },
        1,
        if scheme == Scheme::Cbcs { 0 } else { 16 },
    ];
    tenc.extend(kid);
    if scheme == Scheme::Cbcs {
        tenc.push(16);
        tenc.extend(iv.ok_or(BAD)?);
    }
    let tenc = full(
        *b"tenc",
        if scheme == Scheme::Cbcs {
            0x0100_0000
        } else {
            0
        },
        &tenc,
    )?;
    let mut children = boxed(*b"frma", &track.codec)?;
    children.extend(full(
        *b"schm",
        0,
        &[
            if scheme == Scheme::Cenc {
                b"cenc"
            } else {
                b"cbcs"
            }
            .as_slice(),
            &0x0001_0000_u32.to_be_bytes(),
        ]
        .concat(),
    )?);
    children.extend(boxed(*b"schi", &tenc)?);
    boxed(*b"sinf", &children)
}
fn rewrite_node(
    node: &Mp4Box,
    bytes: &[u8],
    entries: &[(usize, &TrackDescriptor)],
    scheme: Scheme,
    kid: Option<&[u8; 16]>,
    constant_ivs: &[Zeroizing<[u8; 16]>],
    stop: &Stop,
) -> Result<Vec<u8>, ConversionError> {
    read::check_stop(stop)?;
    if matches!(
        &node.kind(),
        b"pssh" | b"senc" | b"saiz" | b"saio" | b"sinf"
    ) {
        return Ok(Vec::new());
    }
    let descriptor = entries
        .iter()
        .find(|(offset, _)| *offset == node.header.offset)
        .map(|(_, track)| *track);
    if node.children.is_empty() && descriptor.is_none() {
        return Ok(node.header.bytes(bytes).to_vec());
    }
    let first = node
        .children
        .first()
        .map_or(node.header.offset + node.header.size, |child| {
            child.header.offset
        });
    let mut payload = bytes
        .get(node.header.offset + node.header.header_size..first)
        .ok_or(BAD)?
        .to_vec();
    for child in &node.children {
        payload.extend(rewrite_node(
            child,
            bytes,
            entries,
            scheme,
            kid,
            constant_ivs,
            stop,
        )?);
        if payload.len() > MAX_METADATA_BYTES {
            return Err(ConversionError("resource-budget"));
        }
    }
    let entry_kind = if let Some(track) = descriptor {
        if scheme == Scheme::Clear {
            track.codec
        } else {
            payload.extend(protection(
                track,
                scheme,
                kid.ok_or(BAD)?,
                constant_ivs.get(track.index).map(|iv| &**iv),
            )?);
            if track.kind == crate::inspection::TrackKind::Video {
                *b"encv"
            } else {
                *b"enca"
            }
        }
    } else {
        node.kind()
    };
    boxed(entry_kind, &payload)
}
pub(super) fn initialization(
    original: &[u8],
    source: &read::Layout,
    parsed: &init::Initialization,
    scheme: Scheme,
    kid: Option<&[u8; 16]>,
    constant_ivs: &[Zeroizing<[u8; 16]>],
    stop: &Stop,
) -> Result<Vec<u8>, ConversionError> {
    let entries = parsed
        .tracks
        .iter()
        .map(|track| {
            let stsd = read::one(&track.table.children, *b"stsd")?;
            Ok((
                stsd.children.first().ok_or(BAD)?.header.offset,
                &track.descriptor,
            ))
        })
        .collect::<Result<Vec<_>, ConversionError>>()?;
    let nodes = mp4::parse_boxes_with_limit(original, super::MAX_PARSED_NODES).map_err(|_| BAD)?;
    let mut output = Vec::new();
    let mut ftyp = false;
    for node in &nodes {
        read::check_stop(stop)?;
        match &node.kind() {
            b"ftyp" => {
                if ftyp {
                    return Err(BAD);
                }
                ftyp = true;
                output.extend(node.header.bytes(original));
            }
            b"moov" => {
                let movie = mp4::parse_boxes_with_limit(&source.movie, super::MAX_PARSED_NODES)
                    .map_err(|_| BAD)?;
                let mut rewritten = rewrite_node(
                    read::one(&movie, *b"moov")?,
                    &source.movie,
                    &entries,
                    scheme,
                    kid,
                    constant_ivs,
                    stop,
                )?;
                if scheme != Scheme::Clear {
                    let kid = kid.ok_or(BAD)?;
                    let mut pssh =
                        hex::decode("1077efecc0b24d02ace33c1e52e2fb4b").map_err(|_| BAD)?;
                    pssh.extend(1_u32.to_be_bytes());
                    pssh.extend(kid);
                    pssh.extend(0_u32.to_be_bytes());
                    let pssh = full(*b"pssh", 0x0100_0000, &pssh)?;
                    let mut payload = rewritten.split_off(8);
                    payload.extend(pssh);
                    rewritten = boxed(*b"moov", &payload)?;
                }
                output.extend(rewritten);
            }
            b"pssh" | b"sidx" | b"ssix" | b"mfra" | b"mdat" => {}
            b"free" | b"skip" | b"wide" => output.extend(node.header.bytes(original)),
            _ => return Err(super::UNSUPPORTED),
        }
        if output.len() > MAX_METADATA_BYTES {
            return Err(ConversionError("resource-budget"));
        }
    }
    if !ftyp {
        output.splice(0..0, boxed(*b"ftyp", b"isom\0\0\0\0isomiso6")?);
    }
    Ok(output)
}

pub(super) struct OutputSample {
    pub info: super::SampleInfo,
    pub iv: Option<Zeroizing<[u8; 16]>>,
    pub spans: Vec<crypto::Subsample>,
    pub digest: [u8; 32],
}
pub(super) struct OutputPart {
    pub index: usize,
    pub runs: Vec<Vec<OutputSample>>,
}
fn run_box(run: &[OutputSample], payload_size: &mut u64) -> Result<Vec<u8>, ConversionError> {
    let signed = run.iter().any(|sample| sample.info.pts < sample.info.dts);
    let mut payload = u32::try_from(run.len())
        .map_err(|_| BAD)?
        .to_be_bytes()
        .to_vec();
    payload.extend(0_i32.to_be_bytes());
    for sample in run {
        let info = &sample.info;
        payload.extend(info.duration.to_be_bytes());
        payload.extend(info.size.to_be_bytes());
        payload.extend(info.flags.to_be_bytes());
        let composition = info.pts.checked_sub(info.dts).ok_or(BAD)?;
        if signed {
            payload.extend(
                i32::try_from(composition)
                    .map_err(|_| super::UNSUPPORTED)?
                    .to_be_bytes(),
            );
        } else {
            payload.extend(u32::try_from(composition).map_err(|_| BAD)?.to_be_bytes());
        }
        *payload_size = payload_size.checked_add(u64::from(info.size)).ok_or(BAD)?;
    }
    full(
        *b"trun",
        if signed { 0x0100_0f01 } else { 0x0000_0f01 },
        &payload,
    )
}

fn auxiliary_boxes(
    part: &OutputPart,
    scheme: Scheme,
    video: bool,
) -> Result<Vec<u8>, ConversionError> {
    let total = part
        .runs
        .iter()
        .map(Vec::len)
        .try_fold(0_usize, usize::checked_add)
        .ok_or(BAD)?;
    let mut auxiliary = u32::try_from(total)
        .map_err(|_| BAD)?
        .to_be_bytes()
        .to_vec();
    let mut sizes = Vec::new();
    let mut indexed = true;
    for sample in part.runs.iter().flatten() {
        let start = auxiliary.len();
        if scheme == Scheme::Cenc {
            auxiliary.extend(sample.iv.as_ref().ok_or(BAD)?.as_ref());
        }
        if video {
            auxiliary.extend(
                u16::try_from(sample.spans.len())
                    .map_err(|_| BAD)?
                    .to_be_bytes(),
            );
            for span in &sample.spans {
                auxiliary.extend(u16::try_from(span.clear).map_err(|_| BAD)?.to_be_bytes());
                auxiliary.extend(span.encrypted.to_be_bytes());
            }
        }
        match u8::try_from(auxiliary.len() - start) {
            Ok(size) => sizes.push(size),
            Err(_) => indexed = false,
        }
        if auxiliary.len() > MAX_METADATA_BYTES {
            return Err(ConversionError("resource-budget"));
        }
    }
    let mut children = full(*b"senc", if video { 2 } else { 0 }, &auxiliary)?;
    if indexed {
        let mut payload = vec![0];
        payload.extend(u32::try_from(total).map_err(|_| BAD)?.to_be_bytes());
        payload.extend(sizes);
        children.extend(full(*b"saiz", 0, &payload)?);
        children.extend(full(
            *b"saio",
            0,
            &[1_u32.to_be_bytes(), 0_u32.to_be_bytes()].concat(),
        )?);
    }
    Ok(children)
}

pub(super) fn fragment_header(
    original: &super::fragment::Fragment,
    parts: &[OutputPart],
    tracks: &[TrackDescriptor],
    scheme: Scheme,
) -> Result<(Vec<u8>, Vec<u8>), ConversionError> {
    let nodes =
        mp4::parse_boxes_with_limit(&original.bytes, super::MAX_PARSED_NODES).map_err(|_| BAD)?;
    let moof = read::one(&nodes, *b"moof")?;
    let original_parts = moof
        .children
        .iter()
        .filter(|node| node.kind() == *b"traf")
        .collect::<Vec<_>>();
    if original_parts.len() != parts.len() {
        return Err(BAD);
    }
    let mut children = read::one(&moof.children, *b"mfhd")?
        .header
        .bytes(&original.bytes)
        .to_vec();
    let mut payload_offsets = Vec::new();
    let mut payload_size = 0_u64;
    for (part, original_traf) in parts.iter().zip(&original_parts) {
        let track = tracks.get(part.index).ok_or(BAD)?;
        let mut children_part = full(*b"tfhd", 0x0002_0000, &track.track_id.to_be_bytes())?;
        children_part.extend(
            read::one(&original_traf.children, *b"tfdt")?
                .header
                .bytes(&original.bytes),
        );
        for run in &part.runs {
            payload_offsets.push(payload_size);
            children_part.extend(run_box(run, &mut payload_size)?);
        }
        for group in original_traf
            .children
            .iter()
            .filter(|node| matches!(&node.kind(), b"sgpd" | b"sbgp"))
        {
            children_part.extend(group.header.bytes(&original.bytes));
        }
        if scheme != Scheme::Clear {
            children_part.extend(auxiliary_boxes(
                part,
                scheme,
                track.kind == crate::inspection::TrackKind::Video,
            )?);
        }
        children.extend(boxed(*b"traf", &children_part)?);
        if children.len() > MAX_METADATA_BYTES {
            return Err(ConversionError("resource-budget"));
        }
    }
    let mut output = boxed(*b"moof", &children)?;
    let small = payload_size
        .checked_add(8)
        .filter(|n| u32::try_from(*n).is_ok());
    let mdat_header = if let Some(size) = small {
        [
            u32::try_from(size)
                .map_err(|_| BAD)?
                .to_be_bytes()
                .as_slice(),
            b"mdat",
        ]
        .concat()
    } else {
        [
            1_u32.to_be_bytes().as_slice(),
            b"mdat",
            &payload_size.checked_add(16).ok_or(BAD)?.to_be_bytes(),
        ]
        .concat()
    };
    let parsed = mp4::parse_boxes_with_limit(&output, super::MAX_PARSED_NODES).map_err(|_| BAD)?;
    let mut offsets = payload_offsets.into_iter();
    for traf in read::one(&parsed, *b"moof")?
        .children
        .iter()
        .filter(|node| node.kind() == *b"traf")
    {
        for run in traf.children.iter().filter(|node| node.kind() == *b"trun") {
            let offset = (output.len() as u64)
                .checked_add(mdat_header.len() as u64)
                .and_then(|n| n.checked_add(offsets.next()?))
                .ok_or(BAD)?;
            let offset = i32::try_from(offset)
                .map_err(|_| ConversionError("native-output-fragment-size-limit"))?;
            let at = run.header.offset + run.header.header_size + 8;
            output[at..at + 4].copy_from_slice(&offset.to_be_bytes());
        }
        if let Some(saio) = read::optional(&traf.children, *b"saio")? {
            let senc = read::one(&traf.children, *b"senc")?;
            let offset =
                u32::try_from(senc.header.offset + senc.header.header_size + 8).map_err(|_| BAD)?;
            let at = saio.header.offset + saio.header.header_size + 8;
            output[at..at + 4].copy_from_slice(&offset.to_be_bytes());
        }
    }
    if offsets.next().is_some() {
        return Err(BAD);
    }
    Ok((output, mdat_header))
}

/// A failed or cancelled resource is removed only if the path still names our file.
pub(super) struct Output {
    file: Option<File>,
    path: PathBuf,
    identity: FileIdentity,
    complete: bool,
    written: u64,
}
impl Output {
    pub(super) fn new(path: &Path) -> Result<Self, ConversionError> {
        let file = super::super::files::create_new(path)?;
        let identity = FileIdentity::read(&file).map_err(|_| WRITE)?;
        Ok(Self {
            file: Some(file),
            path: path.into(),
            identity,
            complete: false,
            written: 0,
        })
    }
    pub(super) fn write(&mut self, bytes: &[u8]) -> Result<(), ConversionError> {
        let next = self
            .written
            .checked_add(u64::try_from(bytes.len()).map_err(|_| WRITE)?)
            .filter(|n| *n <= 16 * 1024 * 1024 * 1024)
            .ok_or(ConversionError("resource-budget"))?;
        self.file
            .as_mut()
            .ok_or(WRITE)?
            .write_all(bytes)
            .map_err(|_| WRITE)?;
        self.written = next;
        Ok(())
    }
    pub(super) fn finish(mut self) -> Result<(), ConversionError> {
        self.file
            .as_ref()
            .ok_or(WRITE)?
            .sync_all()
            .map_err(|_| WRITE)?;
        self.file.take();
        let current = super::super::files::regular_input(&self.path)?;
        if FileIdentity::read(&current).map_err(|_| WRITE)? != self.identity {
            return Err(ConversionError("native-output-changed"));
        }
        self.complete = true;
        Ok(())
    }
}
impl Drop for Output {
    fn drop(&mut self) {
        if self.complete {
            return;
        }
        self.file.take();
        if super::super::files::regular_input(&self.path)
            .ok()
            .and_then(|file| FileIdentity::read(&file).ok())
            == Some(self.identity)
        {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

pub(super) struct SampleWriter<'a> {
    output: &'a mut Output,
    plan: &'a OutputSample,
    cipher: Option<encrypt::Encryptor>,
    span: usize,
    clear_left: u32,
    encrypted_left: u32,
    pending: Zeroizing<Vec<u8>>,
    received: u32,
    hash: Sha256,
}
impl<'a> SampleWriter<'a> {
    pub(super) fn new(
        output: &'a mut Output,
        plan: &'a OutputSample,
        scheme: Scheme,
        key: Option<&[u8; 16]>,
        video: bool,
    ) -> Result<Self, ConversionError> {
        let cipher = if scheme == Scheme::Clear {
            None
        } else {
            Some(encrypt::Encryptor::new(
                key.ok_or(BAD)?,
                plan.iv.as_ref().ok_or(BAD)?,
                scheme,
                video,
            )?)
        };
        let mut writer = Self {
            output,
            plan,
            cipher,
            span: 0,
            clear_left: 0,
            encrypted_left: 0,
            pending: Zeroizing::new(Vec::with_capacity(16)),
            received: 0,
            hash: Sha256::new(),
        };
        writer.next_span()?;
        Ok(writer)
    }
    fn next_span(&mut self) -> Result<(), ConversionError> {
        if let Some(span) = self.plan.spans.get(self.span) {
            self.clear_left = span.clear;
            self.encrypted_left = span.encrypted;
            self.cipher.as_mut().ok_or(BAD)?.begin_subsample();
        }
        Ok(())
    }
    fn encrypted(&mut self, bytes: &[u8]) -> Result<(), ConversionError> {
        let cipher = self.cipher.as_mut().ok_or(BAD)?;
        if !cipher.block_aligned() {
            let mut encrypted = Zeroizing::new(bytes.to_vec());
            cipher.transform(&mut encrypted)?;
            return self.output.write(&encrypted);
        }
        let mut at = 0;
        if !self.pending.is_empty() {
            let take = (16 - self.pending.len()).min(bytes.len());
            self.pending.extend_from_slice(&bytes[..take]);
            at += take;
            if self.pending.len() == 16 {
                cipher.transform(&mut self.pending)?;
                self.output.write(&self.pending)?;
                self.pending.zeroize();
            }
        }
        let length = (bytes.len() - at) / 16 * 16;
        if length != 0 {
            let mut encrypted = Zeroizing::new(bytes[at..at + length].to_vec());
            cipher.transform(&mut encrypted)?;
            self.output.write(&encrypted)?;
            at += length;
        }
        self.pending.extend_from_slice(&bytes[at..]);
        Ok(())
    }
    pub(super) fn push(&mut self, bytes: &[u8]) -> Result<(), ConversionError> {
        self.hash.update(bytes);
        self.received = self
            .received
            .checked_add(u32::try_from(bytes.len()).map_err(|_| BAD)?)
            .filter(|n| *n <= self.plan.info.size)
            .ok_or(BAD)?;
        if self.cipher.is_none() {
            return self.output.write(bytes);
        }
        let mut at = 0;
        while at < bytes.len() {
            if self.clear_left != 0 {
                let length = usize::try_from(self.clear_left)
                    .map_err(|_| BAD)?
                    .min(bytes.len() - at);
                self.output.write(&bytes[at..at + length])?;
                self.clear_left -= u32::try_from(length).map_err(|_| BAD)?;
                at += length;
            } else if self.encrypted_left != 0 {
                let length = usize::try_from(self.encrypted_left)
                    .map_err(|_| BAD)?
                    .min(bytes.len() - at);
                self.encrypted(&bytes[at..at + length])?;
                self.encrypted_left -= u32::try_from(length).map_err(|_| BAD)?;
                at += length;
            } else {
                if !self.pending.is_empty() {
                    self.output.write(&self.pending)?;
                    self.pending.zeroize();
                }
                self.span = self.span.checked_add(1).ok_or(BAD)?;
                if self.span >= self.plan.spans.len() {
                    return Err(BAD);
                }
                self.next_span()?;
            }
        }
        Ok(())
    }
    pub(super) fn finish(self) -> Result<(), ConversionError> {
        if self.received != self.plan.info.size
            || self.clear_left != 0
            || self.encrypted_left != 0
            || (self.cipher.is_some() && self.span + 1 != self.plan.spans.len())
        {
            return Err(BAD);
        }
        if !self.pending.is_empty() {
            self.output.write(&self.pending)?;
        }
        if <[u8; 32]>::from(self.hash.finalize()) != self.plan.digest {
            return Err(ConversionError("native-source-changed"));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn failed_output_cleanup_preserves_a_replacement_file_after_closing_the_writer() {
        let root = crate::native::private_tempdir().unwrap();
        let path = root.path().join("output.m4s");
        let mut output = Output::new(&path).unwrap();
        output.write(b"owned").unwrap();
        // Windows also requires closing a private writer before rename/readback.
        output.file.take();
        let original = root.path().join("moved-original.m4s");
        std::fs::rename(&path, &original).unwrap();
        std::fs::write(&path, b"replacement").unwrap();
        drop(output);
        assert_eq!(std::fs::read(&path).unwrap(), b"replacement");
        assert_eq!(std::fs::read(&original).unwrap(), b"owned");
    }
}
