use super::{
    syntax::{Attribute, Decimal},
    time::{self, DateError},
    timed::Ranges,
};
use crate::NotCheckedReason;
use sha2::{Digest as _, Sha256};

enum Value<'a> {
    Quoted(&'a str),
    Token(&'a str),
    Number(&'a str, &'a str),
    Hex(&'a str),
    Date(i128, &'a str),
}

fn canonical(attribute: Attribute<'_>) -> Result<Value<'_>, NotCheckedReason> {
    if matches!(attribute.name, "START-DATE" | "END-DATE") {
        if !attribute.quoted {
            return Err(NotCheckedReason::InvalidPrerequisite);
        }
        let date = time::date(attribute.value).map_err(|error| match error {
            DateError::Invalid => NotCheckedReason::InvalidPrerequisite,
            DateError::Unsupported => NotCheckedReason::UnsupportedFeature,
        })?;
        if !date.zoned {
            return Err(NotCheckedReason::UnsupportedFeature);
        }
        return Ok(Value::Date(
            date.seconds,
            date.fraction.trim_end_matches('0'),
        ));
    }
    if attribute.quoted {
        return Ok(Value::Quoted(attribute.value));
    }
    if matches!(attribute.name, "DURATION" | "PLANNED-DURATION")
        || attribute.name.starts_with("X-") && Decimal::parse(attribute.value).is_some()
    {
        let number =
            Decimal::parse(attribute.value).ok_or(NotCheckedReason::InvalidPrerequisite)?;
        return Ok(Value::Number(
            number.whole,
            number.fraction.trim_end_matches('0'),
        ));
    }
    if hex(attribute.value) {
        return Ok(Value::Hex(&attribute.value[2..]));
    }
    Ok(Value::Token(attribute.value))
}

pub(super) fn equivalent(
    left: Attribute<'_>,
    right: Attribute<'_>,
) -> Result<bool, NotCheckedReason> {
    Ok(match (canonical(left)?, canonical(right)?) {
        (Value::Quoted(left), Value::Quoted(right)) | (Value::Token(left), Value::Token(right)) => {
            left == right
        }
        (Value::Number(a, b), Value::Number(c, d)) => a == c && b == d,
        (Value::Date(a, b), Value::Date(c, d)) => a == c && b == d,
        (Value::Hex(left), Value::Hex(right)) => left.eq_ignore_ascii_case(right),
        _ => false,
    })
}

pub(super) fn fingerprint(ranges: &Ranges<'_>) -> Result<[u8; 32], NotCheckedReason> {
    let mut digest = Sha256::new();
    part(&mut digest, b"hls-date-ranges-v1");
    digest.update((ranges.len() as u64).to_be_bytes());
    for (id, (attributes, _)) in ranges {
        part(&mut digest, id.as_bytes());
        digest.update((attributes.len() as u64).to_be_bytes());
        for (name, attribute) in attributes {
            part(&mut digest, name.as_bytes());
            canonical(*attribute)?.write(&mut digest);
        }
    }
    Ok(digest.finalize().into())
}

impl Value<'_> {
    fn write(&self, digest: &mut Sha256) {
        match self {
            Self::Quoted(value) => {
                part(digest, b"quoted");
                part(digest, value.as_bytes());
            }
            Self::Token(value) => {
                part(digest, b"token");
                part(digest, value.as_bytes());
            }
            Self::Number(whole, fraction) => {
                part(digest, b"number");
                part(digest, whole.as_bytes());
                part(digest, fraction.as_bytes());
            }
            Self::Date(seconds, fraction) => {
                part(digest, b"date");
                part(digest, &seconds.to_be_bytes());
                part(digest, fraction.as_bytes());
            }
            Self::Hex(value) => {
                part(digest, b"hex");
                digest.update((value.len() as u64).to_be_bytes());
                for chunk in value.as_bytes().chunks(64) {
                    let mut uppercase = [0; 64];
                    for (to, from) in uppercase.iter_mut().zip(chunk) {
                        *to = from.to_ascii_uppercase();
                    }
                    digest.update(&uppercase[..chunk.len()]);
                }
            }
        }
    }
}

fn part(digest: &mut Sha256, value: &[u8]) {
    digest.update((value.len() as u64).to_be_bytes());
    digest.update(value);
}

pub(super) fn hex(value: &str) -> bool {
    value
        .strip_prefix("0x")
        .or_else(|| value.strip_prefix("0X"))
        .is_some_and(|digits| {
            !digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_hexdigit())
        })
}
