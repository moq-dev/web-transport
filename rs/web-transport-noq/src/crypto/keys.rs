use noq::crypto::{CryptoError, HandshakeTokenKey, HmacKey};
use rind::{aead, hkdf, hmac};

use super::{check_cpu, UnsupportedCpu};

/// A rind HMAC-SHA256 key for Noq stateless resets.
pub struct ResetKey(hmac::Key);

impl ResetKey {
    /// Uses a persistent secret to reproduce reset tokens across endpoint restarts.
    pub fn new(secret: &[u8]) -> Result<Self, UnsupportedCpu> {
        check_cpu()?;
        Ok(Self(hmac::Key::new(&hmac::HMAC_SHA256, secret)))
    }
}

impl HmacKey for ResetKey {
    fn sign(&self, data: &[u8], out: &mut [u8]) {
        out.copy_from_slice(hmac::sign(&self.0, data).as_ref());
    }

    fn signature_len(&self) -> usize {
        32
    }

    fn verify(&self, data: &[u8], signature: &[u8]) -> Result<(), CryptoError> {
        hmac::verify(&self.0, data, signature).map_err(|_| CryptoError)
    }
}

/// A rind HKDF-SHA256 key for Noq Retry and validation tokens.
pub struct TokenKey(hkdf::Prk);

impl TokenKey {
    /// Derives a token key from a secret with at least 32 bytes of entropy.
    pub fn new(secret: &[u8]) -> Result<Self, UnsupportedCpu> {
        check_cpu()?;
        Ok(Self(
            hkdf::Salt::new(&hkdf::HKDF_SHA256, &[]).extract(secret),
        ))
    }

    fn aead(&self, token_nonce: u128) -> aead::Key {
        aead::Key::from(
            self.0
                .expand(&[&token_nonce.to_le_bytes()], &aead::AES_256_GCM),
        )
    }
}

impl HandshakeTokenKey for TokenKey {
    fn seal(&self, token_nonce: u128, data: &mut Vec<u8>) -> Result<(), CryptoError> {
        // Noq supplies a fresh 128-bit random nonce per token, deriving a single-use key.
        self.aead(token_nonce)
            .seal_in_place_append_tag(aead::Nonce::assume_unique_for_key([0; 12]), &[], data)
            .map_err(|_| CryptoError)
    }

    fn open<'a>(&self, token_nonce: u128, data: &'a mut [u8]) -> Result<&'a [u8], CryptoError> {
        self.aead(token_nonce)
            .open_in_place(aead::Nonce::assume_unique_for_key([0; 12]), &[], data)
            .map(|plaintext| &*plaintext)
            .map_err(|_| CryptoError)
    }
}
