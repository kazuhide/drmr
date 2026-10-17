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
        Zeroizing::new(hex::encode(self.bytes.as_ref()))
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
