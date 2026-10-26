//! Track byte I/O through a single owned input cursor.
use std::{
    fs::File,
    io::{self, Read, Seek, SeekFrom},
    ops::Deref,
};

pub(in crate::conversion::native_media) struct PositionedInput {
    file: File,
    pub(super) position: Option<u64>,
    #[cfg(test)]
    pub(super) reads: usize,
    #[cfg(test)]
    pub(super) seeks: usize,
}
impl PositionedInput {
    pub fn new(file: File) -> Self {
        Self {
            file,
            position: Some(0),
            #[cfg(test)]
            reads: 0,
            #[cfg(test)]
            seeks: 0,
        }
    }
}

impl Deref for PositionedInput {
    type Target = File;
    fn deref(&self) -> &File {
        &self.file
    }
}
impl Read for PositionedInput {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        #[cfg(test)]
        {
            self.reads += 1;
        }
        match self.file.read(bytes) {
            Ok(length) => {
                self.position = self.position.and_then(|at| at.checked_add(length as u64));
                Ok(length)
            }
            Err(error) => {
                self.position = None;
                Err(error)
            }
        }
    }
}
impl Seek for PositionedInput {
    fn seek(&mut self, target: SeekFrom) -> io::Result<u64> {
        if let SeekFrom::Start(position) = target
            && self.position == Some(position)
        {
            return Ok(position);
        }
        #[cfg(test)]
        {
            self.seeks += 1;
        }
        match self.file.seek(target) {
            Ok(position) => {
                self.position = Some(position);
                Ok(position)
            }
            Err(error) => {
                self.position = None;
                Err(error)
            }
        }
    }
}
