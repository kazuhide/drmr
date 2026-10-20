//! Recognize only immediate zero-length cue pairs with no active ad interval.
use super::ConversionError;

const BAD: ConversionError = ConversionError("hls-ad-cue-not-supported");
#[derive(Default, PartialEq, Eq)]
enum Phase {
    #[default]
    None,
    In,
    Segment,
}
#[derive(Default)]
pub(super) struct EmptyCue {
    phase: Phase,
    seen: bool,
}
impl EmptyCue {
    pub fn seen(&self) -> bool {
        self.seen
    }
    pub fn finish(&self) -> Result<(), ConversionError> {
        if self.phase == Phase::None {
            Ok(())
        } else {
            Err(BAD)
        }
    }
    pub fn line(
        &mut self,
        line: &str,
        previous_segment: bool,
        pending_duration: bool,
    ) -> Result<bool, ConversionError> {
        if self.phase == Phase::In {
            if line != "#EXT-X-CUE-IN" {
                return Err(BAD);
            }
            self.phase = Phase::Segment;
            self.seen = true;
            return Ok(true);
        }
        if self.phase == Phase::Segment {
            if !line.starts_with("#EXTINF:") {
                return Err(BAD);
            }
            self.phase = Phase::None;
            return Ok(false);
        }
        if line.starts_with("#EXT-X-CUE-IN") {
            return Err(BAD);
        }
        if !line.starts_with("#EXT-X-CUE-OUT") {
            return Ok(false);
        }
        let value = line.strip_prefix("#EXT-X-CUE-OUT:").ok_or(BAD)?;
        let (integral, fraction) = value.split_once('.').unwrap_or((value, ""));
        let zero = value.len() <= 64
            && !integral.is_empty()
            && integral.bytes().all(|b| b == b'0')
            && (!value.contains('.')
                || (!fraction.is_empty() && fraction.bytes().all(|b| b == b'0')));
        if !previous_segment || pending_duration || !zero {
            return Err(BAD);
        }
        self.phase = Phase::In;
        Ok(true)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn zero_pairs_require_a_complete_segment_and_exact_immediate_neighbors() {
        for value in ["0", "000", "0.0", "000.000"] {
            let mut cue = EmptyCue::default();
            assert!(
                cue.line(&format!("#EXT-X-CUE-OUT:{value}"), true, false)
                    .unwrap()
            );
            assert!(!cue.seen());
            assert!(cue.line("#EXT-X-CUE-IN", true, false).unwrap());
            assert!(cue.seen());
            assert!(cue.finish().is_err());
            assert!(!cue.line("#EXTINF:2,", true, false).unwrap());
            cue.finish().unwrap();
        }
        for (previous, pending) in [(false, false), (true, true)] {
            assert!(
                EmptyCue::default()
                    .line("#EXT-X-CUE-OUT:0.0", previous, pending)
                    .is_err()
            );
        }
    }
    #[test]
    fn active_malformed_unbalanced_and_interleaved_cues_are_refused() {
        for value in ["30", "-0", "NaN", "0e0", ".0", "0.", "0.0:extra"] {
            assert!(
                EmptyCue::default()
                    .line(&format!("#EXT-X-CUE-OUT:{value}"), true, false)
                    .is_err()
            );
        }
        for between in [
            "#EXTINF:2,",
            "#EXT-X-KEY:METHOD=NONE",
            "#EXT-X-CUE-OUT:0.0",
            "#EXT-X-CUE-IN:extra",
            "#EXT-X-ENDLIST",
        ] {
            let mut cue = EmptyCue::default();
            cue.line("#EXT-X-CUE-OUT:0.0", true, false).unwrap();
            assert!(
                cue.line(between, true, false).is_err(),
                "accepted {between}"
            );
        }
        assert!(
            EmptyCue::default()
                .line("#EXT-X-CUE-IN", true, false)
                .is_err()
        );
        let mut cue = EmptyCue::default();
        cue.line("#EXT-X-CUE-OUT:0.0", true, false).unwrap();
        assert!(cue.finish().is_err());
        cue.line("#EXT-X-CUE-IN", true, false).unwrap();
        assert!(cue.line("#EXT-X-ENDLIST", true, false).is_err());
    }
}
