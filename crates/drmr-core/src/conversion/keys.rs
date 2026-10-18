//! Explicit input and output key roles. Secret bytes are wiped when their owner is dropped.
use super::ConversionError;
use std::fmt;
use zeroize::Zeroizing;

pub struct ContentKey {
    pub(crate) id: [u8; 16],
    pub(crate) bytes: Zeroizing<[u8; 16]>,
}
impl fmt::Debug for ContentKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ContentKey([redacted])")
    }
}
impl ContentKey {
    pub fn from_hex(id: &str, key: &str) -> Result<Self, ConversionError> {
        let mut bytes = Zeroizing::new([0; 16]);
        let mut kid = [0; 16];
        if id.len() != 32 || hex::decode_to_slice(id, &mut kid).is_err() {
            return Err(ConversionError("invalid-key-id"));
        }
        if key.len() != 32 || hex::decode_to_slice(key, bytes.as_mut()).is_err() {
            return Err(ConversionError("invalid-content-key"));
        }
        Ok(Self { id: kid, bytes })
    }
    pub(crate) fn key_hex(&self) -> Zeroizing<String> {
        use std::fmt::Write as _;
        let mut value = Zeroizing::new(String::with_capacity(32));
        for byte in self.bytes.iter() {
            write!(value, "{byte:02x}").expect("writing a String cannot fail");
        }
        value
    }
    pub(crate) fn id_hex(&self) -> String {
        hex::encode(self.id)
    }
}
#[derive(Default, Debug)]
pub struct InputKeys {
    pub(crate) keys: Vec<ContentKey>,
}
impl InputKeys {
    pub fn new(keys: Vec<ContentKey>) -> Result<Self, ConversionError> {
        if keys.len() > 64 {
            return Err(ConversionError("key-count-limit"));
        }
        let mut ids = std::collections::BTreeSet::new();
        for key in &keys {
            if !ids.insert(key.id) {
                return Err(ConversionError("duplicate-key-id"));
            }
        }
        Ok(Self { keys })
    }
    pub(crate) fn contains(&self, id: &str) -> bool {
        self.keys.iter().any(|key| key.id_hex() == id)
    }
}
#[derive(Debug)]
pub struct OutputKey(pub(crate) ContentKey);
impl OutputKey {
    pub fn from_hex(id: &str, key: &str) -> Result<Self, ConversionError> {
        ContentKey::from_hex(id, key).map(Self)
    }
}

/// A demuxer may read every track during stream discovery, even for a selected output stream.
#[derive(Clone, Copy)]
pub(super) enum ProbeKeys<'a> {
    One(&'a ContentKey),
    All(&'a InputKeys),
}
pub(super) struct KeyedArguments {
    pub argv: Zeroizing<Vec<String>>,
    material: Vec<Zeroizing<String>>,
}
impl KeyedArguments {
    pub fn new(argv: Vec<String>, keys: Option<ProbeKeys<'_>>) -> Self {
        use std::fmt::Write as _;
        let mut argv = Zeroizing::new(argv);
        let keys: &[ContentKey] = match keys {
            None => &[],
            Some(ProbeKeys::One(key)) => std::slice::from_ref(key),
            Some(ProbeKeys::All(keys)) => &keys.keys,
        };
        let mut material: Vec<_> = keys.iter().map(ContentKey::key_hex).collect();
        if !keys.is_empty() {
            let mut dictionary = Zeroizing::new(String::new());
            for (index, (key, secret)) in keys.iter().zip(&material).enumerate() {
                if index != 0 {
                    dictionary.push(':');
                }
                write!(dictionary, "{}=", key.id_hex()).expect("writing a String cannot fail");
                dictionary.push_str(secret);
            }
            let at = argv.iter().position(|value| value == "-i").unwrap_or(1);
            // The owned compound value and each retained argument are wiped on drop.
            argv.insert(at, "-decryption_keys".into());
            argv.insert(at + 1, dictionary.to_string());
            material.push(dictionary);
        }
        Self { argv, material }
    }
    pub fn secrets(&self) -> Vec<&str> {
        self.material.iter().map(|s| s.as_str()).collect()
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn dictionary_has_every_kid_without_a_default_key_and_redacts_each_value_and_compound() {
        let keys = InputKeys::new(vec![
            ContentKey::from_hex(
                "000102030405060708090a0b0c0d0e0f",
                "101112131415161718191a1b1c1d1e1f",
            )
            .unwrap(),
            ContentKey::from_hex(
                "303132333435363738393a3b3c3d3e3f",
                "202122232425262728292a2b2c2d2e2f",
            )
            .unwrap(),
        ])
        .unwrap();
        for argv in [
            vec!["ffprobe".into(), "source.mp4".into()],
            vec!["ffmpeg".into(), "-i".into(), "source.mp4".into()],
        ] {
            let args = KeyedArguments::new(argv, Some(ProbeKeys::All(&keys)));
            assert!(!args.argv.iter().any(|arg| arg == "-decryption_key"));
            let at = args
                .argv
                .iter()
                .position(|arg| arg == "-decryption_keys")
                .unwrap();
            assert_eq!(
                args.argv[at + 1],
                "000102030405060708090a0b0c0d0e0f=101112131415161718191a1b1c1d1e1f:303132333435363738393a3b3c3d3e3f=202122232425262728292a2b2c2d2e2f"
            );
            assert_eq!(args.secrets().len(), 3);
            assert!(args.secrets().contains(&args.argv[at + 1].as_str()));
            for key in &keys.keys {
                assert!(args.secrets().contains(&key.key_hex().as_str()));
            }
        }
        let clear = KeyedArguments::new(vec!["ffprobe".into(), "clear.mp4".into()], None);
        assert_eq!(clear.argv.len(), 2);
        assert_eq!(clear.secrets(), [] as [&str; 0]);
        assert_eq!(
            format!("{keys:?}"),
            "InputKeys { keys: [ContentKey([redacted]), ContentKey([redacted])] }"
        );
    }
}
