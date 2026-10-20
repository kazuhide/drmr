//! Independently authored scanners for the pinned DASH attribute datatypes.
//! XML Schema 1.0 patterns match the complete normalized attribute literal.

pub(super) fn language(value: &str) -> bool {
    language_parts(value.trim_matches(xml_space), false)
}

fn language_parts(value: &str, letters_only: bool) -> bool {
    let mut parts = value.split('-');
    parts.next().is_some_and(|part| {
        (1..=8).contains(&part.len()) && part.bytes().all(|byte| byte.is_ascii_alphabetic())
    }) && parts.all(|part| {
        (1..=8).contains(&part.len())
            && part
                .bytes()
                .all(|byte| byte.is_ascii_alphabetic() || (!letters_only && byte.is_ascii_digit()))
    })
}

fn xml_space(character: char) -> bool {
    matches!(character, ' ' | '\t' | '\r' | '\n')
}

pub(super) fn sampling_rates(value: &str) -> bool {
    let mut count = 0;
    for item in value.split(xml_space).filter(|item| !item.is_empty()) {
        count += 1;
        if count > 2 || !unsigned_int_item(item) {
            return false;
        }
    }
    count != 0
}

fn unsigned_int_item(value: &str) -> bool {
    // XML Schema 1.0 unsignedInt permits digits only, unlike its signed ancestors.
    let digits = value;
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return false;
    }
    // Leading zeroes do not enlarge the XML Schema integer value.
    let significant = digits.trim_start_matches('0');
    significant.is_empty() || (significant.len() <= 10 && significant.parse::<u32>().is_ok())
}

pub(super) fn content_type(value: &str) -> bool {
    matches!(
        value,
        "text" | "image" | "audio" | "video" | "application" | "font"
    )
}

pub(super) fn byte_range(value: &str) -> bool {
    let mut hyphen = false;
    value.bytes().all(|byte| {
        if byte == b'-' && !hyphen {
            hyphen = true;
            true
        } else {
            byte.is_ascii_digit()
        }
    })
}

pub(super) fn codecs(value: &str) -> bool {
    // The schema's sibling pattern facets are alternatives. A quote or percent
    // sign in a simple token does not force interpretation as a fancy token.
    if token_list(value, |item| {
        item.bytes()
            .all(|byte| CODEC_TOKEN.contains(char::from(byte)))
    }) {
        return true;
    }
    let mut fields = value.splitn(3, '\'');
    let Some(charset) = fields.next() else {
        return false;
    };
    let Some(language) = fields.next() else {
        return false;
    };
    let Some(identifiers) = fields.next() else {
        return false;
    };
    !charset.is_empty()
        && charset
            .bytes()
            .all(|byte| byte.is_ascii_alphabetic() || byte == b'-')
        && language_parts(language, true)
        && token_list(identifiers, encoded_identifier)
}

fn token_list(value: &str, valid: impl Fn(&str) -> bool) -> bool {
    value.split(',').all(|item| !item.is_empty() && valid(item))
}

fn encoded_identifier(value: &str) -> bool {
    let bytes = value.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            if bytes
                .get(index + 1)
                .is_none_or(|byte| !byte.is_ascii_hexdigit())
                || bytes
                    .get(index + 2)
                    .is_none_or(|byte| !byte.is_ascii_hexdigit())
            {
                return false;
            }
            index += 3;
        } else if CODEC_FANCY.contains(char::from(bytes[index])) {
            // A period is itself an allowed attribute character in this schema.
            index += 1;
        } else {
            return false;
        }
    }
    true
}

pub(super) fn profiles(value: &str) -> bool {
    // Most static MPDs use one ordinary URN. Check its entire literal directly,
    // avoiding the more general URI/list state machine for this common case.
    urn(value) || PROFILE_SCANNER.matches(value)
}

fn urn(value: &str) -> bool {
    let Some(body) = value.strip_prefix("urn:") else {
        return false;
    };
    let Some((namespace, specific)) = body.split_once(':') else {
        return false;
    };
    (2..=32).contains(&namespace.len())
        && namespace.as_bytes()[0].is_ascii_alphanumeric()
        && namespace
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        && !specific.is_empty()
        && specific
            .bytes()
            .all(|byte| URN_SPECIFIC.contains(char::from(byte)))
}

#[derive(Clone, Copy)]
struct Characters {
    ascii: u128,
    wildcard: bool,
}
impl Characters {
    const fn literal(byte: u8) -> Self {
        Self {
            ascii: 1u128 << byte,
            wildcard: false,
        }
    }
    const fn bytes(bytes: &[u8]) -> Self {
        let mut result = Self {
            ascii: 0,
            wildcard: false,
        };
        let mut index = 0;
        while index < bytes.len() {
            result.ascii |= 1u128 << bytes[index];
            index += 1;
        }
        result
    }
    const fn range(start: u8, end: u8) -> Self {
        let mut result = Self {
            ascii: 0,
            wildcard: false,
        };
        let mut byte = start;
        while byte <= end {
            result.ascii |= 1u128 << byte;
            byte += 1;
        }
        result
    }
    const fn union(self, other: Self) -> Self {
        Self {
            ascii: self.ascii | other.ascii,
            wildcard: self.wildcard || other.wildcard,
        }
    }
    fn contains(self, character: char) -> bool {
        (self.wildcard && !matches!(character, '\r' | '\n'))
            || (character.is_ascii() && (self.ascii & (1u128 << u32::from(character))) != 0)
    }
}

const ALPHA: Characters = Characters::range(b'a', b'z').union(Characters::range(b'A', b'Z'));
const DIGIT: Characters = Characters::range(b'0', b'9');
const ALNUM: Characters = ALPHA.union(DIGIT);
const HEX: Characters = DIGIT
    .union(Characters::range(b'A', b'F'))
    .union(Characters::range(b'a', b'f'));
const SAFE: Characters = Characters::bytes(b"$-_.+");
const UNRESERVED: Characters = ALNUM.union(SAFE).union(Characters::bytes(b"!*(),\""));
const PATH: Characters = UNRESERVED.union(Characters::bytes(b"%&~;=:@"));
const QUERY: Characters = PATH.union(Characters::bytes(b"/?"));
const USER: Characters = UNRESERVED.union(Characters::bytes(b"%&~;="));
const NAMED_HOST: Characters = ALNUM.union(Characters::bytes(b"%-._~"));
const URN_SPECIFIC: Characters = ALNUM.union(Characters::bytes(b"()+,-.:=@;$_!*'%/?#"));
const CODEC_TOKEN: Characters = ALNUM.union(SAFE).union(Characters::bytes(b"^|'`%!*#\\~&"));
const CODEC_FANCY: Characters = ALNUM.union(SAFE).union(Characters::bytes(b"!#\\^{}|`~&"));

// A fixed, compile-time machine handles the exact URI/list language, including
// commas that can belong to a URI or separate profiles. There is no backtracking,
// dynamic compilation, input-sized state, allocation, or general regex parser.
const MAX_STATES: usize = 512;
const WORDS: usize = MAX_STATES / 64;
type StateSet = [u64; WORDS];
#[derive(Clone, Copy)]
enum State {
    Accept,
    Byte { characters: Characters, next: usize },
    Either { left: usize, right: usize },
}
struct ProfileScanner {
    states: [State; MAX_STATES],
    closures: [StateSet; MAX_STATES],
    count: usize,
    start: usize,
}
impl ProfileScanner {
    const fn add(&mut self, state: State) -> usize {
        assert!(self.count < MAX_STATES);
        let index = self.count;
        self.states[index] = state;
        self.count += 1;
        index
    }
    const fn byte(&mut self, characters: Characters, next: usize) -> usize {
        self.add(State::Byte { characters, next })
    }
    const fn literal(&mut self, bytes: &[u8], next: usize) -> usize {
        let mut state = next;
        let mut index = bytes.len();
        while index > 0 {
            index -= 1;
            state = self.byte(Characters::literal(bytes[index]), state);
        }
        state
    }
    const fn either(&mut self, left: usize, right: usize) -> usize {
        self.add(State::Either { left, right })
    }
    const fn repeat(
        &mut self,
        characters: Characters,
        min: usize,
        max: usize,
        next: usize,
    ) -> usize {
        let mut state = next;
        let mut count = 0;
        while count < max {
            let take = self.byte(characters, state);
            state = if count < max - min {
                self.either(take, next)
            } else {
                take
            };
            count += 1;
        }
        state
    }
    const fn run(&mut self, characters: Characters, required: bool, next: usize) -> usize {
        let take = self.byte(characters, next);
        let choice = self.either(take, next);
        self.states[take] = State::Byte {
            characters,
            next: choice,
        };
        if required { take } else { choice }
    }
    const fn compile(mut self) -> Self {
        let mut start = 0;
        while start < self.count {
            let mut seen = [0u64; WORDS];
            let mut pending = [0usize; MAX_STATES];
            pending[0] = start;
            seen[start / 64] |= 1u64 << (start % 64);
            let mut length = 1;
            while length > 0 {
                length -= 1;
                let index = pending[length];
                match self.states[index] {
                    State::Either { left, right } => {
                        let branches = [left, right];
                        let mut branch = 0;
                        while branch < branches.len() {
                            let child = branches[branch];
                            let bit = 1u64 << (child % 64);
                            if seen[child / 64] & bit == 0 {
                                seen[child / 64] |= bit;
                                pending[length] = child;
                                length += 1;
                            }
                            branch += 1;
                        }
                    }
                    _ => self.closures[start][index / 64] |= 1u64 << (index % 64),
                }
            }
            start += 1;
        }
        self
    }
    fn matches(&self, value: &str) -> bool {
        let mut active = self.closures[self.start];
        for character in value.chars() {
            let mut following = [0u64; WORDS];
            for (word, mut bits) in active.into_iter().enumerate() {
                while bits != 0 {
                    let offset = bits.trailing_zeros() as usize;
                    bits &= bits - 1;
                    if let State::Byte { characters, next } = self.states[word * 64 + offset]
                        && characters.contains(character)
                    {
                        for (output, closure) in following.iter_mut().zip(self.closures[next]) {
                            *output |= closure;
                        }
                    }
                }
            }
            if following.iter().all(|word| *word == 0) {
                return false;
            }
            active = following;
        }
        active[0] & 1 != 0
    }
}

const fn decimal_byte(machine: &mut ProfileScanner, next: usize) -> usize {
    let short = machine.repeat(DIGIT, 1, 2, next);
    let tail = machine.repeat(DIGIT, 2, 2, next);
    let hundreds = machine.literal(b"1", tail);
    let last = machine.byte(DIGIT, next);
    let middle = machine.byte(Characters::range(b'0', b'4'), last);
    let lower = machine.literal(b"2", middle);
    let last = machine.byte(Characters::range(b'0', b'5'), next);
    let upper = machine.literal(b"25", last);
    let large = machine.either(lower, upper);
    let small = machine.either(short, hundreds);
    machine.either(small, large)
}

const fn ipv4(machine: &mut ProfileScanner, next: usize) -> usize {
    let mut state = decimal_byte(machine, next);
    let mut count = 0;
    while count < 3 {
        // The pinned entity uses a wildcard, not an escaped period. Preserve
        // that schema fact rather than substituting an IP-address parser.
        let separator = machine.byte(
            Characters {
                ascii: 0,
                wildcard: true,
            },
            state,
        );
        state = decimal_byte(machine, separator);
        count += 1;
    }
    state
}

const fn ipv6(machine: &mut ProfileScanner, next: usize) -> usize {
    // Ordinary compressed addresses have at most seven explicit groups. Shared
    // suffixes represent group counts without expanding address combinations.
    let mut nonempty_tail = [next; 8];
    let mut tails = [next; 8];
    let mut count = 1;
    while count <= 7 {
        let end = if count == 1 {
            next
        } else {
            let colon = machine.literal(b":", nonempty_tail[count - 1]);
            machine.either(next, colon)
        };
        nonempty_tail[count] = machine.repeat(HEX, 1, 4, end);
        tails[count] = machine.either(next, nonempty_tail[count]);
        count += 1;
    }
    let mut after_group = next;
    count = 7;
    while count > 0 {
        let ordinary = machine.repeat(HEX, 1, 4, after_group);
        let compressed = machine.literal(b":", tails[7 - count]);
        let choice = machine.either(ordinary, compressed);
        after_group = machine.literal(b":", choice);
        count -= 1;
    }
    let full_or_compressed = machine.repeat(HEX, 1, 4, after_group);
    let leading = machine.literal(b"::", tails[7]);
    let ordinary = machine.either(full_or_compressed, leading);

    let zone = machine.run(HEX, true, next);
    let mut link_local = machine.literal(b"%", zone);
    count = 0;
    while count < 4 {
        let group = machine.repeat(HEX, 1, 4, link_local);
        let colon = machine.literal(b":", group);
        link_local = machine.either(link_local, colon);
        count += 1;
    }
    let link_local = machine.literal(b"fe80:", link_local);

    let v4 = ipv4(machine, next);
    let colon = machine.literal(b":", v4);
    let zeros = machine.repeat(Characters::literal(b'0'), 0, 4, colon);
    let mapped = machine.repeat(Characters::bytes(b"fF"), 4, 4, zeros);
    let mapped = machine.either(v4, mapped);
    let mapped = machine.literal(b"::", mapped);
    let mut prefix = machine.literal(b":", v4);
    let mut prefixed = prefix;
    count = 0;
    while count < 4 {
        let colon = machine.literal(b":", prefix);
        prefix = machine.repeat(HEX, 1, 4, colon);
        prefixed = if count == 0 {
            prefix
        } else {
            machine.either(prefixed, prefix)
        };
        count += 1;
    }
    let v4_forms = machine.either(mapped, prefixed);
    let special = machine.either(link_local, v4_forms);
    machine.either(ordinary, special)
}

const fn port(machine: &mut ProfileScanner, next: usize) -> usize {
    let short = machine.repeat(DIGIT, 1, 4, next);
    let tail = machine.repeat(DIGIT, 4, 4, next);
    let lower = machine.byte(Characters::range(b'1', b'5'), tail);
    let tail = machine.repeat(DIGIT, 3, 3, next);
    let tail = machine.byte(Characters::range(b'0', b'4'), tail);
    let middle = machine.literal(b"6", tail);
    let tail = machine.repeat(DIGIT, 2, 2, next);
    let tail = machine.byte(Characters::range(b'0', b'4'), tail);
    let upper = machine.literal(b"65", tail);
    let tail = machine.byte(DIGIT, next);
    let tail = machine.byte(Characters::range(b'0', b'2'), tail);
    let high = machine.literal(b"655", tail);
    let tail = machine.byte(Characters::range(b'0', b'5'), next);
    let maximum = machine.literal(b"6553", tail);
    let a = machine.either(short, lower);
    let b = machine.either(middle, upper);
    let c = machine.either(high, maximum);
    let b = machine.either(b, c);
    machine.either(a, b)
}

const fn url(machine: &mut ProfileScanner, next: usize) -> usize {
    let fragment = machine.run(QUERY, false, next);
    let fragment = machine.literal(b"#", fragment);
    let fragment = machine.either(next, fragment);
    let query = machine.run(QUERY, false, fragment);
    let query = machine.literal(b"?", query);
    let suffix = machine.either(fragment, query);

    let rest = machine.either(suffix, suffix);
    let segment = machine.run(PATH, true, rest);
    let after_slash = machine.either(suffix, segment);
    let slash = machine.literal(b"/", after_slash);
    machine.states[rest] = State::Either {
        left: suffix,
        right: slash,
    };
    let first_slash = machine.literal(b"/", segment);
    let path = machine.either(segment, first_slash);

    let port_number = port(machine, rest);
    let port_start = machine.literal(b":", port_number);
    let host_end = machine.either(rest, port_start);
    let named = machine.run(NAMED_HOST, true, host_end);
    let bracket_end = machine.literal(b"]", host_end);
    let v6 = ipv6(machine, bracket_end);
    let v6 = machine.literal(b"[", v6);
    let future = machine.run(
        UNRESERVED.union(Characters::bytes(b"%&~;=:")),
        true,
        bracket_end,
    );
    let future = machine.byte(DIGIT.union(Characters::range(b'a', b'f')), future);
    let future = machine.literal(b"[v", future);
    let bracketed = machine.either(v6, future);
    let host = machine.either(named, bracketed);
    let at = machine.literal(b"@", host);
    let password = machine.run(USER, true, at);
    let password = machine.literal(b":", password);
    let user_end = machine.either(at, password);
    let user = machine.run(USER, true, user_end);
    let authority = machine.either(host, user);
    let authority = machine.literal(b"//", authority);
    let absolute = machine.either(authority, path);
    let scheme_end = machine.literal(b":", absolute);
    let scheme = machine.run(ALNUM.union(Characters::bytes(b"+-.")), false, scheme_end);
    let scheme = machine.byte(ALPHA, scheme);
    machine.either(path, scheme)
}

static PROFILE_SCANNER: ProfileScanner = {
    // Graph construction is confined to this compile-time static initializer.
    // Runtime matching borrows this immutable storage and two small bit sets.
    let mut machine = ProfileScanner {
        states: [State::Accept; MAX_STATES],
        closures: [[0; WORDS]; MAX_STATES],
        count: 1,
        start: 0,
    };
    let start = machine.either(0, 0);
    // The schema's DTD tab entity is XML-normalized to a space inside the
    // pattern attribute. Instance character-reference tabs remain distinct.
    let separator = machine.run(Characters::literal(b' '), false, start);
    let separator = machine.literal(b",", separator);
    let end = machine.either(0, separator);
    let specific = machine.run(URN_SPECIFIC, true, end);
    let colon = machine.literal(b":", specific);
    let namespace = machine.repeat(ALNUM.union(Characters::literal(b'-')), 1, 31, colon);
    let namespace = machine.byte(ALNUM, namespace);
    let urn = machine.literal(b"urn:", namespace);
    let url = url(&mut machine, end);
    machine.states[start] = State::Either {
        left: urn,
        right: url,
    };
    machine.start = start;
    machine.compile()
};
