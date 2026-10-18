//! Media concatenation, codec inspection, and protected raw-key files.

use super::python;
use super::{HlsMediaError, MediaError};
use memchr::memmem;
use std::io::Write as _;
use std::path::{Path, PathBuf};

/// `media::concatenate`: `paths` byte for byte into `output` (its directory created first),
/// in order; returns `output`.
pub fn concatenate<P: AsRef<Path>>(paths: &[P], output: &Path) -> Result<PathBuf, MediaError> {
    #[cfg(unix)]
    if let Some(parent) = output.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut dest = create_output_file(output)?;
    for path in paths {
        let mut source = std::fs::File::open(path)?;
        std::io::copy(&mut source, &mut dest)?;
    }
    Ok(output.to_path_buf())
}

/// Open a known output without exposing its payload through inherited access.
pub(crate) fn create_output_file(path: &Path) -> std::io::Result<std::fs::File> {
    #[cfg(windows)]
    {
        crate::native::create_media_or_truncate(path)
    }
    #[cfg(unix)]
    {
        std::fs::File::create(path)
    }
}

pub(crate) fn write_output_file(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    create_output_file(path)?.write_all(bytes)
}

pub(crate) fn create_output_dir(path: &Path) -> std::io::Result<()> {
    #[cfg(windows)]
    {
        crate::native::create_private_dir_all(path)
    }
    #[cfg(unix)]
    {
        std::fs::create_dir_all(path)
    }
}

/// `media::codec_from_init`: the RFC 6381 codec string from the first `avcC` (`avc1.PPCCLL`)
/// or `hvcC` (`hvc1.<space><profile>.<compat>.<tier><level>[.<constraints>]`) found anywhere in
/// the file's bytes; `mp4a.40.2` for an AAC fallback; otherwise `output-codec-unproven`.
pub fn codec_from_init(path: &Path, fallback: &str) -> Result<String, MediaError> {
    let data = std::fs::read(path)?;
    if let Some(pos) = memmem::find(&data, b"avcC")
        && data.len() >= pos + 8
    {
        return Ok(format!("avc1.{}", hex::encode(&data[pos + 5..pos + 8])));
    }
    if let Some(pos) = memmem::find(&data, b"hvcC")
        && data.len() >= pos + 17
    {
        return Ok(hevc_codec(&data[pos + 4..]));
    }
    if fallback == "aac" {
        return Ok("mp4a.40.2".to_string());
    }
    Err(HlsMediaError::new("output-codec-unproven").into())
}

/// The `hvc1` string of an `HEVCDecoderConfigurationRecord` (at least 13 bytes).
fn hevc_codec(conf: &[u8]) -> String {
    let profile = conf[1];
    let space = ["", "A", "B", "C"][usize::from(profile >> 6)];
    // The 32 compatibility flags in reverse bit order (ISO/IEC 14496-15 E.3).
    let compat = u32::from_be_bytes([conf[2], conf[3], conf[4], conf[5]]).reverse_bits();
    let constraints = &conf[6..12];
    let kept = constraints
        .iter()
        .rposition(|byte| *byte != 0)
        .map_or(0, |last| last + 1);
    let suffix: Vec<String> = constraints[..kept]
        .iter()
        .map(|byte| format!("{byte:02X}"))
        .collect();
    let tier = if profile & 32 == 0 { 'L' } else { 'H' };
    let mut codec = format!("hvc1.{space}{}.{compat:X}.{tier}{}", profile & 31, conf[12]);
    if !suffix.is_empty() {
        codec.push('.');
        codec.push_str(&suffix.join("."));
    }
    codec
}

/// `media::write_raw_key_file`: the raw 16-byte content key (from hex) into `dest` for
/// ffmpeg's identity-KEYFORMAT decrypt; returns `dest`. The key touches only this file, which is
/// private before a byte is written: a new file is created 0o600 (never world-readable, not even
/// empty, so no other process can hold it open), an existing one is truncated and set to 0o600
/// on the open descriptor. The decoded bytes are wiped, and no error carries the key (Python's
/// `bytes.fromhex` error names a position only). Python writes with the umask's mode; the
/// private mode is this implementation's (private key-file handling).
pub fn write_raw_key_file(key_hex: &str, dest: &Path) -> Result<PathBuf, MediaError> {
    if let Some(parent) = dest.parent() {
        #[cfg(unix)]
        std::fs::create_dir_all(parent)?;
        #[cfg(windows)]
        crate::native::create_private_dir_all(parent)?;
    }
    let key = python::fromhex(key_hex)?;
    let mut file = open_private(dest)?;
    file.write_all(&key)?;
    Ok(dest.to_path_buf())
}

/// `dest` opened for writing, empty and private (see [`write_raw_key_file`]).
#[cfg(windows)]
fn open_private(dest: &Path) -> std::io::Result<std::fs::File> {
    crate::native::create_private_or_truncate(dest)
}

#[cfg(unix)]
fn open_private(dest: &Path) -> std::io::Result<std::fs::File> {
    let mut fresh = std::fs::OpenOptions::new();
    fresh.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        fresh.mode(0o600);
    }
    match fresh.open(dest) {
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            // As Python's open(…, "wb"): an existing file (or a symlink's target) is truncated.
            let file = std::fs::OpenOptions::new()
                .write(true)
                .truncate(true)
                .open(dest)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt as _;
                file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
            }
            Ok(file)
        }
        opened => opened,
    }
}
