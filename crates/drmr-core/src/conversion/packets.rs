//! Constant-memory proof of every absolute packet timestamp in its original order.
use super::ConversionError;
use sha2::{Digest, Sha256};

const INVALID: ConversionError = ConversionError("invalid-packet-evidence");
const TIMING: ConversionError = ConversionError("packet-timing-unavailable");
const COUNT: ConversionError = ConversionError("packet-evidence-count-limit");
const RECORD_BYTES: usize = 256;

#[derive(Debug, PartialEq, Eq)]
pub(super) struct Evidence {
    count: u64,
    digest: [u8; 32],
}

pub(super) struct Parser {
    index: u64,
    expected: u64,
    count: u64,
    record: [u8; RECORD_BYTES],
    length: usize,
    digest: Sha256,
    error: Option<ConversionError>,
}

impl Parser {
    pub fn new(index: usize, expected: u64) -> Self {
        let mut digest = Sha256::new();
        digest.update(b"drmr-packet-timing-v1\0");
        Self {
            index: index as u64,
            expected,
            count: 0,
            record: [0; RECORD_BYTES],
            length: 0,
            digest,
            error: None,
        }
    }

    pub fn push(&mut self, bytes: &[u8]) -> Result<(), ()> {
        if self.error.is_some() {
            return Err(());
        }
        for byte in bytes {
            if *byte == b'\n' {
                if let Err(error) = self.consume_record() {
                    self.error = Some(error);
                    return Err(());
                }
                self.length = 0;
            } else {
                if self.length == self.record.len() {
                    self.error = Some(INVALID);
                    return Err(());
                }
                self.record[self.length] = *byte;
                self.length += 1;
            }
        }
        Ok(())
    }

    fn consume_record(&mut self) -> Result<(), ConversionError> {
        let raw = &self.record[..self.length];
        let raw = raw.strip_suffix(b"\r").unwrap_or(raw);
        let text = std::str::from_utf8(raw).map_err(|_| INVALID)?;
        let mut fields = text.split('|');
        if fields.next() != Some("packet") {
            return Err(INVALID);
        }
        let mut index = None;
        let mut pts = None;
        let mut dts = None;
        let mut duration = None;
        let mut trailing = false;
        for field in fields {
            // An omitted side-data section emits a single trailing separator.
            if field.is_empty() && !trailing {
                trailing = true;
                continue;
            }
            if trailing {
                return Err(INVALID);
            }
            let (name, value) = field.split_once('=').ok_or(INVALID)?;
            match name {
                "stream_index" if index.is_none() => {
                    index = Some(value.parse::<u64>().map_err(|_| INVALID)?);
                }
                "pts" if pts.is_none() => {
                    pts = Some(value.parse::<i64>().map_err(|_| TIMING)?);
                }
                "dts" if dts.is_none() => {
                    dts = Some(value.parse::<i64>().map_err(|_| TIMING)?);
                }
                "duration" if duration.is_none() => {
                    duration = Some(value.parse::<i64>().map_err(|_| TIMING)?);
                }
                _ => return Err(INVALID),
            }
        }
        if index != Some(self.index) {
            return Err(ConversionError("single-track-mp4-required"));
        }
        let pts = pts.ok_or(TIMING)?;
        let dts = dts.ok_or(TIMING)?;
        let duration = duration.filter(|value| *value > 0).ok_or(TIMING)?;
        self.count = self.count.checked_add(1).ok_or(COUNT)?;
        if self.count > self.expected {
            return Err(COUNT);
        }
        // Fixed-width signed integers preserve negative values and record boundaries.
        // The time base is compared separately; no timestamp normalization or tolerance applies.
        for value in [pts, dts, duration] {
            self.digest.update(value.to_be_bytes());
        }
        Ok(())
    }

    pub fn error(&self) -> Option<ConversionError> {
        self.error
    }

    pub fn finish(self) -> Result<Evidence, ConversionError> {
        if let Some(error) = self.error {
            return Err(error);
        }
        if self.length != 0 {
            return Err(INVALID);
        }
        if self.count == 0 || self.count != self.expected {
            return Err(COUNT);
        }
        Ok(Evidence {
            count: self.count,
            digest: self.digest.finalize().into(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn proof(raw: &[u8], expected: u64) -> Result<Evidence, ConversionError> {
        let mut parser = Parser::new(0, expected);
        for chunk in raw.chunks(7) {
            if parser.push(chunk).is_err() {
                break;
            }
        }
        parser.finish()
    }

    #[test]
    fn every_absolute_timestamp_and_its_order_contributes_to_the_proof() {
        let a = b"packet|stream_index=0|pts=-1024|dts=-2048|duration=1024|\r\n";
        let b = b"packet|stream_index=0|pts=0|dts=-1024|duration=1024\n";
        let original = proof(&[a.as_slice(), b].concat(), 2).unwrap();
        let reversed = proof(&[b.as_slice(), a].concat(), 2).unwrap();
        assert_ne!(original, reversed);
        for changed in [
            "packet|stream_index=0|pts=-1023|dts=-2048|duration=1024|\n",
            "packet|stream_index=0|pts=-1024|dts=-2047|duration=1024|\n",
            "packet|stream_index=0|pts=-1024|dts=-2048|duration=1023|\n",
        ] {
            assert_ne!(
                original,
                proof(&[changed.as_bytes(), b].concat(), 2).unwrap()
            );
        }
    }

    #[test]
    fn malformed_incomplete_or_wrong_count_records_never_yield_a_proof() {
        for invalid in [
            "packet|stream_index=0|pts=0|dts=0|duration=1",
            "packet|stream_index=0|pts=0|dts=0|duration=0\n",
            "packet|stream_index=0|pts=N/A|dts=0|duration=1\n",
            "packet|stream_index=0|pts=9223372036854775808|dts=0|duration=1\n",
            "packet|stream_index=0|pts=0|pts=0|dts=0|duration=1\n",
            "packet|stream_index=0|pts=0|duration=1\n",
            "packet|stream_index=1|pts=0|dts=0|duration=1\n",
            "packet|stream_index=0|pts=0|dts=0|duration=1||\n",
            "packet|stream_index=0|pts=0|dts=0|duration=1|unexpected=1\n",
            "\n",
        ] {
            assert!(
                proof(invalid.as_bytes(), 1).is_err(),
                "invalid record accepted"
            );
        }
        let record = b"packet|stream_index=0|pts=0|dts=0|duration=1\n";
        assert!(proof(record, 2).is_err());
        assert!(proof(&[record.as_slice(), record].concat(), 1).is_err());
        assert!(proof(&[b'x'; RECORD_BYTES + 1], 1).is_err());
        assert!(proof(&[], 0).is_err());
    }

    #[test]
    fn valid_packet_output_over_thirty_two_mebibytes_is_streamed_without_capture() {
        use std::io::Write as _;
        use std::sync::Arc;
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("packets.txt");
        let mut file = std::io::BufWriter::new(std::fs::File::create(&path).unwrap());
        let count = 700_001_u64;
        for index in 0..count {
            writeln!(
                file,
                "packet|stream_index=0|pts={index}|dts={index}|duration=1"
            )
            .unwrap();
        }
        file.flush().unwrap();
        drop(file);
        assert!(std::fs::metadata(&path).unwrap().len() > 32 * 1024 * 1024);
        let tool = drmr_test_support::install(
            &root.path().join("response"),
            &drmr_test_support::ToolSpec::default().with("stdout_file", path.to_str().unwrap()),
        )
        .unwrap();
        let runner = crate::toolrun::ToolRunner::new(
            Arc::new(crate::toolrun::Secrets::new()),
            Arc::new(crate::events::NoSink),
        );
        let mut parser = Parser::new(0, count);
        let completed = runner
            .run_tool_streamed(
                &[tool.to_str().unwrap().into()],
                "ffprobe",
                &crate::toolrun::RunOptions {
                    max_stdout_bytes: 16 * 1024,
                    timeout: Some(std::time::Duration::from_secs(30)),
                    ..Default::default()
                },
                |chunk| parser.push(chunk),
            )
            .unwrap();
        assert_eq!(completed.returncode, Some(0));
        assert_eq!(completed.stdout.len(), 0);
        assert_eq!(parser.finish().unwrap().count, count);
    }

    #[test]
    fn more_than_one_hundred_thousand_packets_use_one_fixed_record_buffer() {
        let mut parser = Parser::new(0, 200_001);
        for index in 0..200_001_i64 {
            let line = format!("packet|stream_index=0|pts={index}|dts={index}|duration=1\n");
            parser.push(line.as_bytes()).unwrap();
            assert_eq!(parser.record.len(), RECORD_BYTES);
            assert_eq!(parser.length, 0);
        }
        assert_eq!(parser.finish().unwrap().count, 200_001);
    }
}
