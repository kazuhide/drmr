//! The playlist text transforms of `media_inspect`: the `#EXT-X-KEY` IV extraction and the
//! TN2454 identity-KEYFORMAT localizers (one shared core, a filesystem flavour for ffmpeg and
//! HTTP flavours for the GUI). Pure text: no key bytes, no tool.

use super::python;
use super::{HexSecret, MediaError};
use regex::{NoExpand, Regex};
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

static HLS_IV: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"#EXT-X-KEY:[^\n]*\bIV=0x([0-9A-Fa-f]{32})\b").expect("a fixed pattern")
});
static KEYFORMAT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"KEYFORMAT="[^"]*""#).expect("a fixed pattern"));
static KEY_URI: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"URI="[^"]*""#).expect("a fixed pattern"));

/// `media::extract_hls_iv_hex_values`: every `#EXT-X-KEY:…IV=0x<32 hex>` value, lower
/// case, without `0x`, in order (the greedy line match takes a line's last `IV=`).
pub fn extract_hls_iv_hex_values(playlist_text: &str) -> Vec<HexSecret> {
    HLS_IV
        .captures_iter(playlist_text)
        .map(|captures| HexSecret::new(captures[1].to_ascii_lowercase()))
        .collect()
}

/// `_URI_LINE_RE.match(line)` (`^(?!#)\S.*$`): a non-empty line that starts with neither `#`
/// nor whitespace.
fn is_uri_line(line: &str) -> bool {
    line.chars()
        .next()
        .is_some_and(|first| first != '#' && !python::is_space(first))
}

fn re_error(message: String) -> MediaError {
    MediaError::uncaught("re.error", message)
}

/// `\g<name>` at `name_start` (the character after `<`): `\g<0>` is the match; any other group
/// is the `re.error` (or, for an identifier, the `IndexError`) `CPython` 3.11 raises, since the
/// patterns have no groups. Returns the index after `>`.
fn group_reference(
    chars: &[char],
    name_start: usize,
    matched: &str,
    out: &mut String,
) -> Result<usize, MediaError> {
    if chars.get(name_start - 1) != Some(&'<') {
        return Err(re_error(format!(
            "missing < at position {}",
            name_start - 1
        )));
    }
    let Some(length) = chars[name_start..].iter().position(|c| *c == '>') else {
        return Err(re_error(format!(
            "missing >, unterminated name at position {name_start}"
        )));
    };
    let name: String = chars[name_start..name_start + length].iter().collect();
    let identifier = name
        .chars()
        .next()
        .is_some_and(|first| first.is_alphabetic() || first == '_')
        && name.chars().all(|c| c.is_alphanumeric() || c == '_');
    if name.is_empty() {
        return Err(re_error(format!(
            "missing group name at position {name_start}"
        )));
    }
    if identifier {
        return Err(MediaError::uncaught(
            "IndexError",
            format!("unknown group name {}", python::repr_str(&name)),
        ));
    }
    match name.parse::<u64>() {
        Ok(0) => out.push_str(matched),
        Ok(group) => {
            return Err(re_error(format!(
                "invalid group reference {group} at position {name_start}"
            )));
        }
        Err(_) => {
            return Err(re_error(format!(
                "bad character in group name {} at position {name_start}",
                python::repr_str(&name)
            )));
        }
    }
    Ok(name_start + length + 1)
}

/// `\1`…`\9` at `start` (the backslash): three octal digits are a character, anything else a
/// group reference, which is an error here. Returns the index after the escape.
fn digit_escape(chars: &[char], start: usize, out: &mut String) -> Result<usize, MediaError> {
    let octal = |c: Option<&char>| c.is_some_and(|c| ('0'..='7').contains(c));
    let first = chars[start + 1];
    let mut end = start + 2;
    if chars.get(end).is_some_and(char::is_ascii_digit) {
        end += 1;
        if first <= '7' && octal(chars.get(end - 1)) && octal(chars.get(end)) {
            end += 1;
            let digits: String = chars[start + 1..end].iter().collect();
            let value = u32::from_str_radix(&digits, 8).expect("octal digits");
            if value > 0o377 {
                return Err(re_error(format!(
                    "octal escape value \\{digits} outside of range 0-0o377 at position {start}"
                )));
            }
            out.push(char::from_u32(value).expect("a byte is a char"));
            return Ok(end);
        }
    }
    let group: String = chars[start + 1..end].iter().collect();
    Err(re_error(format!(
        "invalid group reference {group} at position {}",
        start + 1
    )))
}

/// The replacement `re.sub` makes of `template` for one match of a pattern without groups
/// (`URI="[^"]*"`): `CPython` 3.11's template escapes (`\n`, `\t`, `\\`, octal, `\g<0>`); a
/// group reference or an unknown ASCII-letter escape is the `re.error` Python raises, with
/// its message and position; any other escaped character stays as written.
fn expand_template(template: &str, matched: &str) -> Result<String, MediaError> {
    let chars: Vec<char> = template.chars().collect();
    let mut out = String::with_capacity(template.len());
    let mut index = 0;
    while index < chars.len() {
        let c = chars[index];
        if c != '\\' {
            out.push(c);
            index += 1;
            continue;
        }
        let start = index;
        let Some(&next) = chars.get(index + 1) else {
            return Err(re_error(format!(
                "bad escape (end of pattern) at position {start}"
            )));
        };
        index += 2;
        match next {
            'g' => index = group_reference(&chars, start + 3, matched, &mut out)?,
            '0' => {
                let digits: String = chars[index..]
                    .iter()
                    .take(2)
                    .take_while(|digit| ('0'..='7').contains(*digit))
                    .collect();
                index += digits.len();
                let value = u32::from_str_radix(&format!("0{digits}"), 8).expect("octal digits");
                out.push(char::from_u32(value & 0xff).expect("a byte is a char"));
            }
            '1'..='9' => index = digit_escape(&chars, start, &mut out)?,
            'a' => out.push('\u{7}'),
            'b' => out.push('\u{8}'),
            'f' => out.push('\u{c}'),
            'n' => out.push('\n'),
            'r' => out.push('\r'),
            't' => out.push('\t'),
            'v' => out.push('\u{b}'),
            '\\' => out.push('\\'),
            letter if letter.is_ascii_alphabetic() => {
                return Err(re_error(format!(
                    "bad escape \\{letter} at position {start}"
                )));
            }
            other => {
                out.push('\\');
                out.push(other);
            }
        }
    }
    Ok(out)
}

/// `pattern.sub(template, line, count=1)` for the `URI="…"` pattern.
fn replace_first_uri(line: &str, template: &str) -> Result<String, MediaError> {
    let Some(found) = KEY_URI.find(line) else {
        return Ok(line.to_string());
    };
    let replacement = expand_template(template, found.as_str())?;
    Ok(format!(
        "{}{}{}",
        &line[..found.start()],
        replacement,
        &line[found.end()..]
    ))
}

/// The URL of the first `URI="…"` of a line (`m.group(0)[len('URI="'):-1]`).
fn first_uri(line: &str) -> Option<&str> {
    KEY_URI
        .find(line)
        .map(|found| &found.as_str()[5..found.as_str().len() - 1])
}

/// `_localize_identity_keyformat_lines`: `#EXT-X-KEY` → `KEYFORMAT="identity"` and
/// `URI="<key_uri>"`; `#EXT-X-MAP`'s URI and every segment URI line → `resolve_uri(url)`.
fn localize_identity_keyformat_lines<E: From<MediaError>>(
    playlist_text: &str,
    key_uri: &str,
    mut resolve_uri: impl FnMut(&str) -> Result<String, E>,
) -> Result<String, E> {
    let mut out_lines = Vec::new();
    for line in playlist_text.split('\n') {
        let rewritten = if line.starts_with("#EXT-X-KEY:") {
            let identity = KEYFORMAT.replace_all(line, NoExpand(r#"KEYFORMAT="identity""#));
            replace_first_uri(&identity, &format!("URI=\"{key_uri}\""))?
        } else if line.starts_with("#EXT-X-MAP:") {
            match first_uri(line) {
                Some(url) => {
                    let target = resolve_uri(url)?;
                    replace_first_uri(line, &format!("URI=\"{target}\""))?
                }
                None => line.to_string(),
            }
        } else if is_uri_line(line) {
            resolve_uri(line)?
        } else {
            line.to_string()
        };
        out_lines.push(rewritten);
    }
    Ok(out_lines.join("\n"))
}

/// `media::localize_hls_media_playlist`: the playlist rewritten so ffmpeg decrypts and
/// decodes it against local files — `#EXT-X-KEY` to `KEYFORMAT="identity"` with the resolved
/// key file as its URI, `#EXT-X-MAP` and segment URIs to the resolved paths `resolve_uri`
/// returns; `IV=` untouched. Paths are written as `Path.resolve.as_posix` writes them.
pub fn localize_hls_media_playlist<E: From<MediaError>>(
    playlist_text: &str,
    key_file_path: &Path,
    mut resolve_uri: impl FnMut(&str) -> Result<PathBuf, E>,
) -> Result<String, E> {
    let key_uri = python::path_str(&python::resolve(key_file_path)?)?;
    localize_identity_keyformat_lines(playlist_text, &key_uri, |url| {
        let local = resolve_uri(url)?;
        Ok(python::path_str(&python::resolve(&local)?)?)
    })
}

/// `media::localize_hls_media_playlist_http`: the same transform with HTTP targets
/// (`key_uri` the key route, `resolve_uri` mapping a source URL to a media route).
pub fn localize_hls_media_playlist_http<E: From<MediaError>>(
    playlist_text: &str,
    key_uri: &str,
    resolve_uri: impl FnMut(&str) -> Result<String, E>,
) -> Result<String, E> {
    localize_identity_keyformat_lines(playlist_text, key_uri, resolve_uri)
}

/// `media::localize_hls_master_http`: every `#EXT-X-STREAM-INF` variant URI line and
/// every `#EXT-X-MEDIA` `URI=` rewritten to `resolve_variant_uri(url)`; every other line kept.
pub fn localize_hls_master_http<E: From<MediaError>>(
    master_text: &str,
    mut resolve_variant_uri: impl FnMut(&str) -> Result<String, E>,
) -> Result<String, E> {
    let lines: Vec<&str> = master_text.split('\n').collect();
    let mut out_lines: Vec<String> = lines.iter().map(|line| (*line).to_string()).collect();
    let mut expect_variant_uri = false;
    for (index, line) in lines.iter().enumerate() {
        if line.starts_with("#EXT-X-STREAM-INF:") {
            expect_variant_uri = true;
            continue;
        }
        if line.starts_with("#EXT-X-MEDIA:") {
            if let Some(url) = first_uri(line) {
                let target = resolve_variant_uri(url)?;
                out_lines[index] = replace_first_uri(line, &format!("URI=\"{target}\""))?;
            }
            continue;
        }
        if expect_variant_uri && is_uri_line(line) {
            out_lines[index] = resolve_variant_uri(line)?;
            expect_variant_uri = false;
        }
    }
    Ok(out_lines.join("\n"))
}
