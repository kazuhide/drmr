//! Streaming SHA-256 for media evidence and source fingerprints.
//!
//! The existing ring backend selects CPU implementations at runtime and retains
//! a software fallback. Only the digest engine changes; callers retain their
//! own read bounds, cancellation points and source-identity checks.

#[derive(Clone)]
pub(crate) struct Sha256(ring::digest::Context);

impl Sha256 {
    pub(crate) fn new() -> Self {
        Self(ring::digest::Context::new(&ring::digest::SHA256))
    }

    pub(crate) fn update(&mut self, bytes: impl AsRef<[u8]>) {
        self.0.update(bytes.as_ref());
    }

    pub(crate) fn finalize(self) -> [u8; 32] {
        let digest = self.0.finish();
        let mut bytes = [0; 32];
        bytes.copy_from_slice(digest.as_ref());
        bytes
    }
}

#[cfg(test)]
mod tests;
