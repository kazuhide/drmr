//! One-use proof binding an admitted private copy to its exact scanner state.
use super::{Admission, ConversionError, Stop};
use crate::{native::FileIdentity, sha256::Sha256};
use std::{
    fs::File,
    io::{self, BufReader, BufWriter, Read, Write as _},
    path::{Path, PathBuf},
    sync::Arc,
};

const BAD: ConversionError = ConversionError("ts-admitted-copy-invalid");
const BUFFER_BYTES: usize = 64 * 1024;

fn stopped(stop: &Stop) -> Result<(), ConversionError> {
    if stop.is_set() {
        Err(ConversionError("cancelled"))
    } else {
        Ok(())
    }
}

fn unchanged(
    path: &Path,
    held: &File,
    identity: FileIdentity,
    length: u64,
) -> Result<(), ConversionError> {
    super::super::super::validate_held_input(held)?;
    let current = super::super::super::files::regular_input(path)?;
    if FileIdentity::read(held).map_err(|_| BAD)? != identity
        || FileIdentity::read(&current).map_err(|_| BAD)? != identity
        || held.metadata().map_err(|_| BAD)?.len() != length
        || current.metadata().map_err(|_| BAD)?.len() != length
    {
        return Err(BAD);
    }
    Ok(())
}

/// Only this exclusively created file may be removed on refusal.
struct Created {
    path: PathBuf,
    writer: Option<File>,
    identity: FileIdentity,
    keep: bool,
}
impl Created {
    fn new(path: &Path) -> Result<Self, ConversionError> {
        let writer = super::super::super::files::create_new(path)?;
        Ok(Self {
            identity: FileIdentity::read(&writer).map_err(|_| BAD)?,
            writer: Some(writer),
            path: path.into(),
            keep: false,
        })
    }
}
impl Drop for Created {
    fn drop(&mut self) {
        drop(self.writer.take());
        if !self.keep {
            let owned = super::super::super::files::regular_input(&self.path)
                .is_ok_and(|file| FileIdentity::read(&file).is_ok_and(|id| id == self.identity));
            if owned {
                let _ = std::fs::remove_file(&self.path);
            }
        }
    }
}

struct Tee<'a> {
    source: &'a mut File,
    destination: BufWriter<&'a mut File>,
    digest: Sha256,
    count: u64,
    length: u64,
}
impl Read for Tee<'_> {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        let count = self.source.read(bytes)?;
        let total = self
            .count
            .checked_add(count as u64)
            .filter(|n| *n <= self.length)
            .ok_or_else(|| io::Error::other(BAD))?;
        self.destination.write_all(&bytes[..count])?;
        self.digest.update(&bytes[..count]);
        self.count = total;
        Ok(count)
    }
}

/// Hashes precisely the byte stream supplied to the native parser.
pub(super) struct CheckedRead {
    file: File,
    digest: Sha256,
    count: u64,
    length: u64,
}
impl Read for CheckedRead {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        let count = self.file.read(bytes)?;
        let total = self
            .count
            .checked_add(count as u64)
            .filter(|n| *n <= self.length)
            .ok_or_else(|| io::Error::other(BAD))?;
        self.digest.update(&bytes[..count]);
        self.count = total;
        Ok(count)
    }
}

fn eof(input: &mut impl Read) -> Result<(), ConversionError> {
    loop {
        match input.read(&mut [0]) {
            Ok(0) => return Ok(()),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            _ => return Err(BAD),
        }
    }
}

pub(super) struct Receipt {
    before: Admission,
    pub after: Admission,
    context: Arc<()>,
    copy: Created,
    digest: [u8; 32],
    length: u64,
}
impl Receipt {
    pub fn length(&self) -> u64 {
        self.length
    }
    pub fn copy(
        before: &Admission,
        context: &Arc<()>,
        source: &Path,
        destination: &Path,
        stop: &Stop,
    ) -> Result<Self, ConversionError> {
        stopped(stop)?;
        let mut input = super::super::super::files::regular_input(source)?;
        super::super::super::validate_held_input(&input)?;
        let identity = FileIdentity::read(&input).map_err(|_| BAD)?;
        let length = input.metadata().map_err(|_| BAD)?.len();
        unchanged(source, &input, identity, length)?;
        let mut copy = Created::new(destination)?;
        let mut after = before.clone();
        let digest = {
            let mut tee = Tee {
                source: &mut input,
                destination: BufWriter::with_capacity(
                    BUFFER_BYTES,
                    copy.writer.as_mut().ok_or(BAD)?,
                ),
                digest: Sha256::new(),
                count: 0,
                length,
            };
            let admitted = (|| {
                let mut reader = BufReader::with_capacity(BUFFER_BYTES, &mut tee);
                after.segment_reader(&mut reader, length, stop)?;
                eof(&mut reader)?;
                drop(reader);
                tee.destination.flush().map_err(|_| BAD)?;
                Ok(())
            })();
            stopped(stop)?;
            admitted?;
            if tee.count != length {
                return Err(BAD);
            }
            tee.digest.finalize()
        };
        unchanged(source, &input, identity, length)?;
        drop(copy.writer.take());
        Ok(Self {
            before: before.clone(),
            after,
            context: Arc::clone(context),
            copy,
            digest,
            length,
        })
    }
    pub fn matches(&self, before: &Admission, context: &Arc<()>) -> bool {
        self.before == *before && Arc::ptr_eq(&self.context, context)
    }
    pub fn open(&self, stop: &Stop) -> Result<CheckedRead, ConversionError> {
        stopped(stop)?;
        let file = super::super::super::files::regular_input(&self.copy.path)?;
        unchanged(&self.copy.path, &file, self.copy.identity, self.length)?;
        Ok(CheckedRead {
            file,
            digest: Sha256::new(),
            count: 0,
            length: self.length,
        })
    }
    pub fn verify(&self, mut input: CheckedRead, stop: &Stop) -> Result<(), ConversionError> {
        stopped(stop)?;
        eof(&mut input)?;
        unchanged(
            &self.copy.path,
            &input.file,
            self.copy.identity,
            self.length,
        )?;
        if input.count != self.length || input.digest.finalize() != self.digest {
            return Err(BAD);
        }
        stopped(stop)
    }
    pub fn accept(mut self) -> Admission {
        self.copy.keep = true;
        self.after
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn held_source_size_and_path_identity_guards_detect_changes() {
        let owner = crate::native::private_tempdir().unwrap();
        let root = owner.path().join("media");
        crate::native::create_media_dir_all(&root).unwrap();
        for change in ["shorter", "longer", "replacement"] {
            let source = root.join(format!("{change}.ts"));
            crate::native::create_media_new(&source)
                .unwrap()
                .write_all(&[0x55; 188])
                .unwrap();
            // The standard handle permits the adversarial updates on Windows;
            // production regular_input also denies write/delete sharing there.
            let held = File::open(&source).unwrap();
            let identity = FileIdentity::read(&held).unwrap();
            unchanged(&source, &held, identity, 188).unwrap();
            match change {
                "shorter" => std::fs::write(&source, [0x55; 187]).unwrap(),
                "longer" => std::fs::write(&source, [0x55; 189]).unwrap(),
                "replacement" => {
                    std::fs::rename(&source, root.join("displaced.ts")).unwrap();
                    std::fs::write(&source, [0x55; 188]).unwrap();
                }
                _ => unreachable!(),
            }
            assert_eq!(unchanged(&source, &held, identity, 188), Err(BAD));
        }
    }
}
