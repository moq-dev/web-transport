//! TLS provider selection for caller-created iroh endpoints.
//!
//! ```no_run
//! use web_transport_iroh::{Client, crypto, iroh::{Endpoint, endpoint::presets}};
//!
//! # async fn example() -> Result<(), Box<dyn std::error::Error>> {
//! let endpoint = Endpoint::builder(presets::Empty)
//!     .crypto_provider(crypto::default_provider()?)
//!     .bind()
//!     .await?;
//! let client = Client::new(endpoint);
//! # client.close().await;
//! # Ok(())
//! # }
//! ```
//!
//! With no installed provider, rind supports x86_64, aarch64 and 32-bit ARM.
//! aarch64 requires NEON; missing CPU support returns [`CryptoError::UnsupportedCpu`]
//! before any rind cryptography runs. Other architectures, including WASM, cannot
//! compile rind. An installed provider is used without invoking rind.

use std::sync::Arc;

use rustls::crypto::CryptoProvider;

use crate::CryptoError;

/// Return the installed rustls provider, otherwise rind's provider after checking CPU support.
pub fn default_provider() -> Result<Arc<CryptoProvider>, CryptoError> {
    if let Some(provider) = CryptoProvider::get_default() {
        return Ok(provider.clone());
    }

    rind_rustls::default_provider()
        .map(Arc::new)
        .map_err(|err| CryptoError::UnsupportedCpu(Arc::new(err)))
}
