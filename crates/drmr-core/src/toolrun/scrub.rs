//! Original-byte overlap union with bounded memory and regex search work.

use super::REDACTED;
use regex::Regex;
use std::cmp::Reverse;
use std::collections::BinaryHeap;

const WINDOW_BYTES: usize = 16 * 1024;
const SCAN_PASSES: usize = 32;

struct Search<'a> {
    pattern: &'a Regex,
    literal_width: Option<usize>,
    remaining: usize,
}

impl<'a> Search<'a> {
    fn new(pattern: &'a Regex, text_len: usize) -> Self {
        Self {
            pattern,
            literal_width: literal_width(pattern.as_str()),
            remaining: text_len.saturating_add(1).saturating_mul(SCAN_PASSES),
        }
    }

    fn next(&mut self, text: &str, mut at: usize) -> Result<Option<(usize, usize)>, ()> {
        if let Some(width) = self.literal_width {
            // This grammar has no assertions or variable-length operators. A
            // truncated prefix cannot change a match contained in that prefix.
            // Overlap windows by the maximum UTF-8 width, including casefolds
            // such as ASCII K matching the three-byte Kelvin sign.
            loop {
                let limit = text.floor_char_boundary(
                    at.saturating_add(WINDOW_BYTES)
                        .saturating_add(width)
                        .min(text.len()),
                );
                if let Some(found) = self.pattern.find_at(&text[..limit], at) {
                    return Ok(Some((found.start(), found.end())));
                }
                if limit == text.len() {
                    return Ok(None);
                }
                at = text.floor_char_boundary(limit - width);
            }
        }
        // Regex::find_at can inspect the entire remaining haystack even when
        // it returns a short match (for example a.*b|a). Charge that upper bound
        // BEFORE each call, never the returned match length. Per pattern, at
        // most 32 * (N + 1) haystack bytes reach the regex engine. This also
        // bounds greedy edge patterns with a suffix, without guessing their
        // syntax or retaining all overlaps. Each supplied pattern has its own
        // budget, so registering more secrets does not consume another's share.
        let cost = text.len() - at + 1;
        self.remaining = self.remaining.checked_sub(cost).ok_or(())?;
        Ok(self
            .pattern
            .find_at(text, at)
            .map(|found| (found.start(), found.end())))
    }
}

/// Recognize only escaped literals with an optional leading casefold flag.
/// Everything else, including assertions, escaped character classes and literals
/// wider than one window, uses the conservative full-haystack budget. The width
/// cap also keeps every literal query within two fixed-size windows, even for
/// arbitrary public Regex inputs. No regex is cloned or recompiled.
fn literal_width(pattern: &str) -> Option<usize> {
    let mut characters = pattern.strip_prefix("(?i)").unwrap_or(pattern).chars();
    let mut width = 0_usize;
    while let Some(character) = characters.next() {
        match character {
            '\\' => {
                if characters.next()?.is_alphanumeric() {
                    return None;
                }
            }
            '.' | '+' | '*' | '?' | '(' | ')' | '|' | '[' | ']' | '{' | '}' | '^' | '$' => {
                return None;
            }
            _ => {}
        }
        width = width.checked_add(4)?;
        if width > WINDOW_BYTES {
            return None;
        }
    }
    Some(width)
}

pub(super) fn combined(registered: &[Regex], extra: &[Regex], text: &str) -> String {
    let mut searches: Vec<_> = registered
        .iter()
        .chain(extra)
        .map(|pattern| Search::new(pattern, text.len()))
        .collect();
    // One pending match and one constant-sized search state per pattern. All
    // matches refer to original bytes, preserving nested and partial overlaps.
    let mut pending = BinaryHeap::with_capacity(searches.len());
    for (index, search) in searches.iter_mut().enumerate() {
        match search.next(text, 0) {
            Ok(Some((start, end))) => pending.push(Reverse((start, end, index))),
            Ok(None) => {}
            Err(()) => return REDACTED.to_string(),
        }
    }
    let mut result = String::with_capacity(text.len());
    let mut at = 0;
    let mut active: Option<(usize, usize)> = None;
    while let Some(Reverse((start, end, index))) = pending.pop() {
        if let Some((first, last)) = active
            && start < last
        {
            active = Some((first, last.max(end)));
        } else {
            if let Some((first, last)) = active {
                result.push_str(&text[at..first]);
                result.push_str(REDACTED);
                at = last;
            }
            active = Some((start, end));
        }
        if active.is_some_and(|(first, last)| first < last && last == text.len()) {
            break;
        }
        // Advancing from the start, rather than the end, finds self-overlaps
        // without splitting UTF-8. On budget exhaustion discard the entire
        // diagnostic, including any partially constructed result.
        if let Some(character) = text[start..].chars().next() {
            match searches[index].next(text, start + character.len_utf8()) {
                Ok(Some((start, end))) => pending.push(Reverse((start, end, index))),
                Ok(None) => {}
                Err(()) => return REDACTED.to_string(),
            }
        }
    }
    if let Some((first, last)) = active {
        result.push_str(&text[at..first]);
        result.push_str(REDACTED);
        at = last;
    }
    result.push_str(&text[at..]);
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_short_match_does_not_discount_an_unbounded_search() {
        let pattern = Regex::new("a.*b|a").unwrap();
        let text = "a".repeat(4096);
        let mut search = Search::new(&pattern, text.len());
        for at in 0..SCAN_PASSES {
            assert_eq!(search.next(&text, at), Ok(Some((at, at + 1))));
        }
        let remaining = search.remaining;
        assert_eq!(search.next(&text, SCAN_PASSES), Err(()));
        // Refusal happens before the next regex dispatch, without underflow.
        assert_eq!(search.remaining, remaining);
    }

    #[test]
    fn only_a_proven_literal_grammar_uses_windows() {
        for pattern in ["a+b", "a.*b|a", "^a$", r"\bfoo", r"\pL", "[a]", "(?i:a)"] {
            assert_eq!(literal_width(pattern), None);
        }
        for pattern in ["ababa", r"abc\.def", r"\*\[\]", "(?i)機密", r"\\"] {
            assert!(literal_width(pattern).is_some());
        }
        assert_eq!(
            literal_width(&"a".repeat(WINDOW_BYTES / 4)),
            Some(WINDOW_BYTES)
        );
        let long = "a".repeat(WINDOW_BYTES / 4 + 1);
        assert_eq!(literal_width(&long), None);
        let pattern = Regex::new(&long).unwrap();
        let mut search = Search::new(&pattern, 1);
        assert!(search.literal_width.is_none());
        // Exhaustion must refuse before dispatch even for a very long literal.
        search.remaining = 0;
        assert_eq!(search.next("a", 0), Err(()));
    }
}
