//! Resolve missing TS field-order metadata only from every independently decoded frame.
use super::super::ConversionError;
const BAD: ConversionError = ConversionError("ts-progressive-frame-evidence-required");
pub(super) struct Evidence {
    count: u64,
}
impl Evidence {
    pub fn resolve_field_order(&self, profile: &mut serde_json::Value) {
        if self.count > 0 && profile["field_order"].is_null() {
            profile["field_order"] = "progressive".into();
        }
    }
}
pub(super) struct Parser {
    line: Vec<u8>,
    count: u64,
    expected: u64,
    error: bool,
}
impl Parser {
    pub fn new(expected: u64) -> Self {
        Self {
            line: Vec::new(),
            count: 0,
            expected,
            error: false,
        }
    }
    pub fn push(&mut self, chunk: &[u8]) -> Result<(), ()> {
        for &byte in chunk {
            if byte == b'\n' {
                let line = self.line.strip_suffix(b"\r").unwrap_or(&self.line);
                if !matches!(
                    line,
                    b"frame|interlaced_frame=0|top_field_first=0"
                        | b"frame|interlaced_frame=0|top_field_first=0|"
                ) || self.count == self.expected
                {
                    self.error = true;
                    return Err(());
                }
                self.count += 1;
                self.line.clear();
            } else {
                if self.line.len() == 128 {
                    self.error = true;
                    return Err(());
                }
                self.line.push(byte);
            }
        }
        Ok(())
    }
    pub fn finish(self) -> Result<Evidence, ConversionError> {
        if self.error || !self.line.is_empty() || self.count != self.expected || self.count == 0 {
            Err(BAD)
        } else {
            Ok(Evidence { count: self.count })
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn every_frame_must_be_complete_progressive_and_counted() {
        let valid = b"frame|interlaced_frame=0|top_field_first=0|\n";
        let mut parser = Parser::new(2);
        for part in valid.chunks(3) {
            parser.push(part).unwrap();
        }
        parser.push(valid).unwrap();
        parser.finish().unwrap();
        for bad in [
            &b"frame|interlaced_frame=1|top_field_first=1|\n"[..],
            &valid[..valid.len() - 1],
            b"frame|top_field_first=0|\n",
            b"frame|interlaced_frame=0|top_field_first=0|extra=0\n",
        ] {
            let mut parser = Parser::new(1);
            let _ = parser.push(bad);
            assert!(parser.finish().is_err());
        }
        let mut parser = Parser::new(2);
        parser.push(valid).unwrap();
        assert!(parser.finish().is_err());
    }

    #[test]
    fn only_complete_progressive_evidence_can_resolve_a_missing_order() {
        let frame = b"frame|interlaced_frame=0|top_field_first=0|\n";
        let mut parser = Parser::new(2);
        parser.push(frame).unwrap();
        parser.push(frame).unwrap();
        let evidence = parser.finish().unwrap();
        let mut omitted = serde_json::json!({"field_order": null, "width": 320});
        evidence.resolve_field_order(&mut omitted);
        assert_eq!(
            omitted,
            serde_json::json!({"field_order": "progressive", "width": 320})
        );
        let mut contradictory = serde_json::json!({"field_order": "tt", "width": 320});
        evidence.resolve_field_order(&mut contradictory);
        assert_eq!(contradictory["field_order"], "tt");
        for bytes in [
            frame.as_slice(),
            b"frame|interlaced_frame=1|top_field_first=0|\n",
        ] {
            let mut incomplete = Parser::new(2);
            let _ = incomplete.push(bytes);
            assert!(incomplete.finish().is_err());
        }
    }
}
