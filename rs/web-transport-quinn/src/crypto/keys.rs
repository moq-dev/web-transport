use quinn::crypto::{AeadKey, CryptoError, HandshakeTokenKey, HmacKey};
use rind::{aead, hkdf, hmac};

use super::{check_cpu, UnsupportedCpu};

/// A rind HMAC-SHA256 key for Quinn stateless resets.
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

/// A rind HKDF-SHA256 key for Quinn Retry and validation tokens.
pub struct TokenKey(hkdf::Prk);

impl TokenKey {
    /// Derives a token key from a secret with at least 32 bytes of entropy.
    pub fn new(secret: &[u8]) -> Result<Self, UnsupportedCpu> {
        check_cpu()?;
        Ok(Self(
            hkdf::Salt::new(&hkdf::HKDF_SHA256, &[]).extract(secret),
        ))
    }
}

impl HandshakeTokenKey for TokenKey {
    fn aead_from_hkdf(&self, random_bytes: &[u8]) -> Box<dyn AeadKey> {
        // Quinn supplies fresh random bytes per token, so each key seals only once.
        Box::new(TokenAead(aead::Key::from(
            self.0.expand(&[random_bytes], &aead::AES_256_GCM),
        )))
    }
}

struct TokenAead(aead::Key);

impl AeadKey for TokenAead {
    fn seal(&self, data: &mut Vec<u8>, additional_data: &[u8]) -> Result<(), CryptoError> {
        self.0
            .seal_in_place_append_tag(
                aead::Nonce::assume_unique_for_key([0; 12]),
                additional_data,
                data,
            )
            .map_err(|_| CryptoError)
    }

    fn open<'a>(
        &self,
        data: &'a mut [u8],
        additional_data: &[u8],
    ) -> Result<&'a mut [u8], CryptoError> {
        self.0
            .open_in_place(
                aead::Nonce::assume_unique_for_key([0; 12]),
                additional_data,
                data,
            )
            .map_err(|_| CryptoError)
    }
}
