use std::{
    collections::BTreeMap,
    io::{self, Read, Write},
};
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Mode {
    #[default]
    Response,
    Packager,
    Ffprobe,
    Ffmpeg,
    Mp4dump,
}
#[derive(Clone, Debug, Default)]
pub struct ToolSpec {
    pub mode: Mode,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub exit_code: i32,
    pub sleep_ms: u64,
    pub values: BTreeMap<String, String>,
}
impl ToolSpec {
    pub fn encode(&self, mut writer: impl Write) -> io::Result<()> {
        writer.write_all(b"DRMRFX01")?;
        writer.write_all(&[match self.mode {
            Mode::Response => 0,
            Mode::Packager => 1,
            Mode::Ffprobe => 2,
            Mode::Ffmpeg => 3,
            Mode::Mp4dump => 4,
        }])?;
        writer.write_all(&self.exit_code.to_le_bytes())?;
        writer.write_all(&self.sleep_ms.to_le_bytes())?;
        write_bytes(&mut writer, &self.stdout)?;
        write_bytes(&mut writer, &self.stderr)?;
        writer.write_all(
            &u32::try_from(self.values.len())
                .map_err(|_| bad())?
                .to_le_bytes(),
        )?;
        for (key, value) in &self.values {
            write_bytes(&mut writer, key.as_bytes())?;
            write_bytes(&mut writer, value.as_bytes())?;
        }
        Ok(())
    }
    pub fn decode(mut reader: impl Read) -> io::Result<Self> {
        let mut magic = [0; 8];
        reader.read_exact(&mut magic)?;
        if &magic != b"DRMRFX01" {
            return Err(bad());
        }
        let mut mode = [0];
        reader.read_exact(&mut mode)?;
        let mode = match mode[0] {
            0 => Mode::Response,
            1 => Mode::Packager,
            2 => Mode::Ffprobe,
            3 => Mode::Ffmpeg,
            4 => Mode::Mp4dump,
            _ => return Err(bad()),
        };
        let mut exit = [0; 4];
        reader.read_exact(&mut exit)?;
        let mut sleep = [0; 8];
        reader.read_exact(&mut sleep)?;
        let stdout = read_bytes(&mut reader)?;
        let stderr = read_bytes(&mut reader)?;
        let mut count = [0; 4];
        reader.read_exact(&mut count)?;
        let count = u32::from_le_bytes(count);
        if count > 128 {
            return Err(bad());
        }
        let mut values = BTreeMap::new();
        for _ in 0..count {
            let key = String::from_utf8(read_bytes(&mut reader)?).map_err(|_| bad())?;
            let value = String::from_utf8(read_bytes(&mut reader)?).map_err(|_| bad())?;
            if values.insert(key, value).is_some() {
                return Err(bad());
            }
        }
        let mut extra = [0];
        if reader.read(&mut extra)? != 0 {
            return Err(bad());
        }
        Ok(Self {
            mode,
            stdout,
            stderr,
            exit_code: i32::from_le_bytes(exit),
            sleep_ms: u64::from_le_bytes(sleep),
            values,
        })
    }
}
fn bad() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "invalid native fixture configuration",
    )
}
fn write_bytes(writer: &mut impl Write, bytes: &[u8]) -> io::Result<()> {
    writer.write_all(&u32::try_from(bytes.len()).map_err(|_| bad())?.to_le_bytes())?;
    writer.write_all(bytes)
}
fn read_bytes(reader: &mut impl Read) -> io::Result<Vec<u8>> {
    let mut length = [0; 4];
    reader.read_exact(&mut length)?;
    let length = u32::from_le_bytes(length) as usize;
    if length > 16 * 1024 * 1024 {
        return Err(bad());
    }
    let mut bytes = vec![0; length];
    reader.read_exact(&mut bytes)?;
    Ok(bytes)
}
