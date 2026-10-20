use super::syntax::Decimal;
use std::cmp::Ordering;

#[derive(Clone)]
pub(super) struct Point<'a> {
    pub seconds: i128,
    pub fraction: &'a str,
    pub zoned: bool,
}
pub(super) enum DateError {
    Invalid,
    Unsupported,
}

struct Calendar {
    seconds: i128,
    end_of_day: bool,
}

pub(super) fn date(value: &str) -> Result<Point<'_>, DateError> {
    if !value.is_ascii() {
        return Err(DateError::Invalid);
    }
    let calendar = calendar(value)?;
    let bytes = value.as_bytes();
    let mut at = 19;
    let fraction = if bytes.get(at) == Some(&b'.') {
        at += 1;
        let begin = at;
        while bytes.get(at).is_some_and(u8::is_ascii_digit) {
            at += 1;
        }
        if at == begin {
            return Err(DateError::Invalid);
        }
        &value[begin..at]
    } else {
        ""
    };
    if calendar.end_of_day && fraction.bytes().any(|byte| byte != b'0') {
        return Err(DateError::Invalid);
    }
    let (offset, zoned) = zone(&value[at..])?;
    Ok(Point {
        seconds: calendar.seconds - offset,
        fraction,
        zoned,
    })
}

fn calendar(value: &str) -> Result<Calendar, DateError> {
    let bytes = value.as_bytes();
    if bytes.len() < 19 {
        return Err(if bytes.first().is_some_and(u8::is_ascii_digit) {
            DateError::Unsupported
        } else {
            DateError::Invalid
        });
    }
    if bytes[4] != b'-'
        || bytes[7] != b'-'
        || !matches!(bytes[10], b'T' | b't')
        || bytes[13] != b':'
        || bytes[16] != b':'
    {
        return Err(if bytes[..4].iter().all(u8::is_ascii_digit) {
            DateError::Unsupported
        } else {
            DateError::Invalid
        });
    }
    let digits = |start, end| -> Result<u32, DateError> {
        let value = value.get(start..end).ok_or(DateError::Invalid)?;
        if !value.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err(DateError::Invalid);
        }
        value.parse().map_err(|_| DateError::Invalid)
    };
    let year = digits(0, 4)?;
    let month = digits(5, 7)?;
    let day = digits(8, 10)?;
    let hour = digits(11, 13)?;
    let minute = digits(14, 16)?;
    let second = digits(17, 19)?;
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let months = [
        31,
        if leap { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    if !(1..=12).contains(&month)
        || day == 0
        || day > months[usize::try_from(month - 1).map_err(|_| DateError::Invalid)?]
        || hour > 24
        || minute > 59
        || second > 60
    {
        return Err(DateError::Invalid);
    }
    if second == 60 {
        return Err(DateError::Unsupported);
    }
    if hour == 24 && (minute != 0 || second != 0) {
        return Err(DateError::Invalid);
    }
    let y = i128::from(year);
    let days = 365 * y + (y + 3) / 4 - (y + 99) / 100
        + (y + 399) / 400
        + months[..usize::try_from(month - 1).map_err(|_| DateError::Invalid)?]
            .iter()
            .map(|day| i128::from(*day))
            .sum::<i128>()
        + i128::from(day - 1);
    Ok(Calendar {
        seconds: days * 86400
            + i128::from(hour) * 3600
            + i128::from(minute) * 60
            + i128::from(second),
        end_of_day: hour == 24,
    })
}

fn zone(suffix: &str) -> Result<(i128, bool), DateError> {
    if matches!(suffix, "" | "Z" | "z") {
        return Ok((0, !suffix.is_empty()));
    }
    let sign = match suffix.as_bytes()[0] {
        b'+' => 1,
        b'-' => -1,
        _ => return Err(DateError::Invalid),
    };
    let rest = &suffix[1..];
    let (hours, minutes) = match rest.len() {
        2 => (rest, "00"),
        4 => (&rest[..2], &rest[2..]),
        5 if rest.as_bytes()[2] == b':' => (&rest[..2], &rest[3..]),
        _ => return Err(DateError::Invalid),
    };
    if !hours
        .bytes()
        .chain(minutes.bytes())
        .all(|byte| byte.is_ascii_digit())
    {
        return Err(DateError::Invalid);
    }
    let hours: i128 = hours.parse().map_err(|_| DateError::Invalid)?;
    let minutes: i128 = minutes.parse().map_err(|_| DateError::Invalid)?;
    if hours > 23 || minutes > 59 {
        return Err(DateError::Invalid);
    }
    Ok((sign * (hours * 3600 + minutes * 60), true))
}

impl Point<'_> {
    pub fn compare(&self, other: &Self) -> Ordering {
        self.seconds
            .cmp(&other.seconds)
            .then_with(|| fraction_compare(self.fraction, other.fraction))
    }
    pub fn plus_equals(&self, duration: Decimal<'_>, end: &Self) -> bool {
        self.plus_compare(duration, end).is_eq()
    }
    pub fn plus_compare(&self, duration: Decimal<'_>, end: &Self) -> Ordering {
        let whole = if duration.whole.is_empty() {
            Some(0)
        } else {
            duration.whole.parse::<u64>().ok()
        };
        let Some(whole) = whole else {
            return Ordering::Greater;
        };
        let width = self
            .fraction
            .len()
            .max(duration.fraction.len())
            .max(end.fraction.len());
        let mut carry = 0;
        let mut fraction = Ordering::Equal;
        for at in (0..width).rev() {
            let digit = |value: &str| value.as_bytes().get(at).map_or(0, |byte| byte - b'0');
            let sum = digit(self.fraction) + digit(duration.fraction) + carry;
            if sum % 10 != digit(end.fraction) {
                fraction = (sum % 10).cmp(&digit(end.fraction));
            }
            carry = sum / 10;
        }
        (self.seconds + i128::from(whole) + i128::from(carry))
            .cmp(&end.seconds)
            .then(fraction)
    }
}

fn fraction_compare(left: &str, right: &str) -> Ordering {
    (0..left.len().max(right.len()))
        .find_map(|at| {
            let a = left.as_bytes().get(at).copied().unwrap_or(b'0');
            let b = right.as_bytes().get(at).copied().unwrap_or(b'0');
            (a != b).then(|| a.cmp(&b))
        })
        .unwrap_or(Ordering::Equal)
}
