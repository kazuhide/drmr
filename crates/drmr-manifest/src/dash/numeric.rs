/// The pinned XML Schema 1.0 unsigned types use digits without a sign, after
/// whitespace collapse. This differs from `nonNegativeInteger` and XSD 1.1.
/// <https://www.w3.org/TR/2004/REC-xmlschema-2-20041028/#unsignedLong>
pub(super) fn unsigned(value: &str, maximum: u64) -> Option<u64> {
    let value = value.trim_matches([' ', '\t', '\r', '\n']);
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    value
        .parse::<u64>()
        .ok()
        .filter(|number| *number <= maximum)
}

pub(super) fn boolean(value: &str) -> Option<bool> {
    match value.trim_matches([' ', '\t', '\r', '\n']) {
        "true" | "1" => Some(true),
        "false" | "0" => Some(false),
        _ => None,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Parsed<T> {
    Value(T),
    Unsupported,
    Invalid,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Duration {
    pub numerator: u128,
    pub denominator: u128,
}
impl Duration {
    pub fn checked_sub(self, other: Self) -> Option<Self> {
        let denominator = self.denominator.max(other.denominator);
        let left = self.numerator.checked_mul(denominator / self.denominator)?;
        let right = other
            .numerator
            .checked_mul(denominator / other.denominator)?;
        Some(Self {
            numerator: left.checked_sub(right)?,
            denominator,
        })
    }
    pub fn checked_add(self, other: Self) -> Option<Self> {
        let denominator = self.denominator.max(other.denominator);
        let left = self.numerator.checked_mul(denominator / self.denominator)?;
        let right = other
            .numerator
            .checked_mul(denominator / other.denominator)?;
        Some(Self {
            numerator: left.checked_add(right)?,
            denominator,
        })
    }
    pub fn compare(self, other: Self) -> Option<std::cmp::Ordering> {
        let denominator = self.denominator.max(other.denominator);
        Some(
            self.numerator
                .checked_mul(denominator / self.denominator)?
                .cmp(
                    &other
                        .numerator
                        .checked_mul(denominator / other.denominator)?,
                ),
        )
    }
}

/// Exact nonnegative day/time durations. Calendar units and unrepresentable but
/// syntactically valid precision are unsupported, never reclassified as invalid.
pub(super) fn duration(value: &str) -> Parsed<Duration> {
    let value = value.trim_matches([' ', '\t', '\r', '\n']);
    let (negative, value) = value
        .strip_prefix('-')
        .map_or((false, value), |value| (true, value));
    let Some(value) = value.strip_prefix('P') else {
        return Parsed::Invalid;
    };
    let mut offset = 0;
    let mut time = false;
    let mut last = 0;
    let mut count = 0;
    let mut time_count = 0;
    let mut calendar = false;
    let mut total = Duration {
        numerator: 0,
        denominator: 1,
    };
    let mut unsupported = false;
    while offset < value.len() {
        if value.as_bytes()[offset] == b'T' {
            if time {
                return Parsed::Invalid;
            }
            time = true;
            offset += 1;
            if offset == value.len() {
                return Parsed::Invalid;
            }
            continue;
        }
        let start = offset;
        while offset < value.len()
            && (value.as_bytes()[offset].is_ascii_digit() || value.as_bytes()[offset] == b'.')
        {
            offset += 1;
        }
        if start == offset || offset == value.len() {
            return Parsed::Invalid;
        }
        let number = &value[start..offset];
        let unit = value.as_bytes()[offset];
        offset += 1;
        let (order, factor) = match (time, unit) {
            (false, b'Y') => {
                calendar = true;
                (1, 1)
            }
            (false, b'M') => {
                calendar = true;
                (2, 1)
            }
            (false, b'D') => (3, 86_400),
            (true, b'H') => (4, 3_600),
            (true, b'M') => (5, 60),
            (true, b'S') => (6, 1),
            _ => return Parsed::Invalid,
        };
        if order <= last {
            return Parsed::Invalid;
        }
        last = order;
        count += 1;
        if time {
            time_count += 1;
        }
        let (whole, fraction) = number
            .split_once('.')
            .map_or((number, None), |(whole, fraction)| (whole, Some(fraction)));
        if whole.is_empty()
            || !whole.bytes().all(|byte| byte.is_ascii_digit())
            || fraction.is_some_and(|part| {
                unit != b'S' || part.is_empty() || !part.bytes().all(|byte| byte.is_ascii_digit())
            })
        {
            return Parsed::Invalid;
        }
        match component(whole, fraction, factor).and_then(|value| total.checked_add(value)) {
            Some(sum) => total = sum,
            None => unsupported = true,
        }
    }
    if count == 0 || (time && time_count == 0) || (negative && total.numerator != 0) {
        Parsed::Invalid
    } else if calendar || unsupported {
        Parsed::Unsupported
    } else {
        Parsed::Value(total)
    }
}

pub(super) fn repeat(value: &str) -> Parsed<Option<u128>> {
    let value = value.trim_matches([' ', '\t', '\r', '\n']);
    let digits = value.strip_prefix(['+', '-']).unwrap_or(value);
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return Parsed::Invalid;
    }
    if value.starts_with('-') && digits.bytes().any(|byte| byte != b'0') {
        return Parsed::Value(None);
    }
    digits
        .parse::<u128>()
        .map_or(Parsed::Unsupported, |value| Parsed::Value(Some(value)))
}

fn component(whole: &str, fraction: Option<&str>, factor: u128) -> Option<Duration> {
    let precision = fraction.map_or(0, str::len);
    if precision > 18 {
        return None;
    }
    let denominator = 10_u128.checked_pow(u32::try_from(precision).ok()?)?;
    let numerator = whole
        .parse::<u128>()
        .ok()?
        .checked_mul(denominator)?
        .checked_add(fraction.and_then(|part| part.parse().ok()).unwrap_or(0))?
        .checked_mul(factor)?;
    Some(Duration {
        numerator,
        denominator,
    })
}
