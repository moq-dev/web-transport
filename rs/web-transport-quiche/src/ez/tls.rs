use std::io;
use std::sync::Arc;

use boring::ec::EcKey;
use boring::hash::MessageDigest;
use boring::pkey::{PKey, Private};
use boring::rsa::Rsa;
use boring::ssl::{
    AlpnError, ClientHello, NameType, SelectCertError, SslAlert, SslContextBuilder, SslMethod,
    SslVerifyError, SslVerifyMode,
};
use boring::x509::store::X509StoreBuilder;
use boring::x509::X509;
use rustls_pki_types::{CertificateDer, PrivateKeyDer};
use tokio_quiche::quic::ConnectionHook;
use tokio_quiche::settings::TlsCertificatePaths;

/// A certificate chain and private key.
pub struct CertifiedKey {
    pub chain: Vec<CertificateDer<'static>>,
    pub key: PrivateKeyDer<'static>,
}

/// Resolves certificates dynamically based on server name (SNI).
pub trait CertResolver: Send + Sync {
    fn resolve(&self, server_name: Option<&str>) -> Option<CertifiedKey>;
}

/// How a server authenticates clients (mTLS).
///
/// The chain a client presents is only validated against these roots; mapping it
/// to an application identity is left to the caller, via
/// [Connection::peer_certificates](super::Connection::peer_certificates).
#[derive(Default)]
pub enum ClientAuth {
    /// Don't request a client certificate.
    #[default]
    None,

    /// Request a client certificate and verify it against these roots.
    ///
    /// Clients that present no certificate are still accepted; a certificate
    /// that doesn't chain to one of these roots fails the handshake.
    Optional(Vec<CertificateDer<'static>>),

    /// Require every client to present a certificate chaining to these roots.
    Required(Vec<CertificateDer<'static>>),
}

impl ClientAuth {
    /// Check this policy by applying it to a throwaway context.
    ///
    /// A [ConnectionHook] can only report failure by returning `None`, which
    /// falls back to a context with no client authentication at all. Bad roots
    /// have to fail here, where the error reaches the caller, rather than
    /// downgrading the policy at handshake time.
    pub(crate) fn validate(&self) -> io::Result<()> {
        let mut probe = SslContextBuilder::new(SslMethod::tls()).map_err(io::Error::other)?;
        apply_client_auth(&mut probe, self)
    }
}

/// Request and verify client certificates according to `auth`.
fn apply_client_auth(builder: &mut SslContextBuilder, auth: &ClientAuth) -> io::Result<()> {
    let (roots, mode) = match auth {
        ClientAuth::None => return Ok(()),
        ClientAuth::Optional(roots) => (roots, SslVerifyMode::PEER),
        ClientAuth::Required(roots) => (
            roots,
            SslVerifyMode::PEER | SslVerifyMode::FAIL_IF_NO_PEER_CERT,
        ),
    };

    if roots.is_empty() {
        // An empty store rejects every chain, so this would silently refuse all
        // (or, when optional, all certificate-bearing) clients.
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "client authentication requires at least one root certificate",
        ));
    }

    let mut store = X509StoreBuilder::new().map_err(io::Error::other)?;
    for der in roots {
        let root = X509::from_der(der.as_ref()).map_err(io::Error::other)?;
        store.add_cert(root).map_err(io::Error::other)?;
    }

    builder.set_cert_store_builder(store);
    builder.set_verify(mode);

    Ok(())
}

fn der_to_boring_key(key: &PrivateKeyDer) -> Result<PKey<Private>, boring::error::ErrorStack> {
    match key {
        PrivateKeyDer::Pkcs8(d) => PKey::private_key_from_der(d.secret_pkcs8_der()),
        PrivateKeyDer::Pkcs1(d) => Ok(PKey::from_rsa(Rsa::private_key_from_der(
            d.secret_pkcs1_der(),
        )?)?),
        PrivateKeyDer::Sec1(d) => Ok(PKey::from_ec_key(EcKey::private_key_from_der(
            d.secret_sec1_der(),
        )?)?),
        _ => {
            tracing::warn!("unsupported private key format");
            Err(boring::error::ErrorStack::get())
        }
    }
}

/// Select the first server protocol also offered by the client (in ALPN wire format).
/// Returns a slice into `client` so the lifetime is correct for the ALPN select callback.
fn alpn_select<'a>(server: &[Vec<u8>], client: &'a [u8]) -> Option<&'a [u8]> {
    for server_proto in server {
        let mut rest = client;
        while !rest.is_empty() {
            let len = rest[0] as usize;
            if len == 0 || 1 + len > rest.len() {
                break;
            }
            let proto = &rest[1..1 + len];
            rest = &rest[1 + len..];
            if proto == server_proto.as_slice() {
                return Some(proto);
            }
        }
    }
    None
}

pub(crate) struct StaticCertHook {
    pub chain: Vec<CertificateDer<'static>>,
    pub key: PrivateKeyDer<'static>,
    pub alpn: Vec<Vec<u8>>,
    pub client_auth: ClientAuth,
}

impl ConnectionHook for StaticCertHook {
    fn create_custom_ssl_context_builder(
        &self,
        _settings: TlsCertificatePaths<'_>,
    ) -> Option<SslContextBuilder> {
        let mut builder = SslContextBuilder::new(SslMethod::tls())
            .inspect_err(|err| tracing::warn!(%err, "failed to create SSL context"))
            .ok()?;

        apply_client_auth(&mut builder, &self.client_auth)
            .inspect_err(|err| tracing::warn!(%err, "failed to configure client authentication"))
            .ok()?;

        // Set the leaf certificate.
        let leaf = X509::from_der(
            self.chain
                .first()
                .or_else(|| {
                    tracing::warn!("empty certificate chain");
                    None
                })?
                .as_ref(),
        )
        .inspect_err(|err| tracing::warn!(%err, "failed to parse leaf certificate DER"))
        .ok()?;
        builder
            .set_certificate(&leaf)
            .inspect_err(|err| tracing::warn!(%err, "failed to set leaf certificate"))
            .ok()?;

        // Set intermediate certificates.
        for cert_der in self.chain.iter().skip(1) {
            let cert = X509::from_der(cert_der.as_ref())
                .inspect_err(
                    |err| tracing::warn!(%err, "failed to parse intermediate certificate DER"),
                )
                .ok()?;
            builder
                .add_extra_chain_cert(cert)
                .inspect_err(|err| tracing::warn!(%err, "failed to add intermediate certificate"))
                .ok()?;
        }

        // Set the private key.
        let key = der_to_boring_key(&self.key)
            .inspect_err(|err| tracing::warn!(%err, "failed to parse private key"))
            .ok()?;
        builder
            .set_private_key(&key)
            .inspect_err(|err| tracing::warn!(%err, "failed to set private key"))
            .ok()?;

        // Select the first server ALPN protocol that the client also supports.
        if !self.alpn.is_empty() {
            let alpn = self.alpn.clone();
            builder.set_alpn_select_callback(move |_, client| {
                alpn_select(alpn.as_slice(), client).ok_or(AlpnError::ALERT_FATAL)
            });
        }

        Some(builder)
    }
}

pub(crate) struct DynamicCertHook {
    pub resolver: Arc<dyn CertResolver>,
    pub alpn: Vec<Vec<u8>>,
    pub client_auth: ClientAuth,
}

impl ConnectionHook for DynamicCertHook {
    fn create_custom_ssl_context_builder(
        &self,
        _settings: TlsCertificatePaths<'_>,
    ) -> Option<SslContextBuilder> {
        let mut builder = SslContextBuilder::new(SslMethod::tls())
            .inspect_err(|err| tracing::warn!(%err, "failed to create SSL context"))
            .ok()?;

        apply_client_auth(&mut builder, &self.client_auth)
            .inspect_err(|err| tracing::warn!(%err, "failed to configure client authentication"))
            .ok()?;

        let resolver = self.resolver.clone();

        builder.set_select_certificate_callback(move |mut client_hello: ClientHello<'_>| {
            let sni = client_hello.servername(NameType::HOST_NAME);
            let certified = resolver.resolve(sni).ok_or(SelectCertError::ERROR)?;

            let ssl = client_hello.ssl_mut();

            // Set the leaf certificate.
            let leaf = X509::from_der(
                certified
                    .chain
                    .first()
                    .ok_or(SelectCertError::ERROR)?
                    .as_ref(),
            )
            .inspect_err(|err| tracing::warn!(%err, "failed to parse leaf certificate DER"))
            .map_err(|_| SelectCertError::ERROR)?;
            ssl.set_certificate(&leaf)
                .inspect_err(|err| tracing::warn!(%err, "failed to set leaf certificate"))
                .map_err(|_| SelectCertError::ERROR)?;

            // Set intermediate certificates.
            for cert_der in certified.chain.iter().skip(1) {
                let cert = X509::from_der(cert_der.as_ref())
                    .inspect_err(
                        |err| tracing::warn!(%err, "failed to parse intermediate certificate DER"),
                    )
                    .map_err(|_| SelectCertError::ERROR)?;
                ssl.add_chain_cert(&cert)
                    .inspect_err(
                        |err| tracing::warn!(%err, "failed to add intermediate certificate"),
                    )
                    .map_err(|_| SelectCertError::ERROR)?;
            }

            // Set the private key.
            let key = der_to_boring_key(&certified.key)
                .inspect_err(|err| tracing::warn!(%err, "failed to parse private key"))
                .map_err(|_| SelectCertError::ERROR)?;
            ssl.set_private_key(&key)
                .inspect_err(|err| tracing::warn!(%err, "failed to set private key"))
                .map_err(|_| SelectCertError::ERROR)?;

            Ok(())
        });

        // Select the first server ALPN protocol that the client also supports.
        if !self.alpn.is_empty() {
            let alpn = self.alpn.clone();
            builder.set_alpn_select_callback(move |_, client| {
                alpn_select(alpn.as_slice(), client).ok_or(AlpnError::ALERT_FATAL)
            });
        }

        Some(builder)
    }
}

/// How a client verifies the server's certificate.
pub(crate) enum ClientVerify {
    /// Standard verification against the SSL context's default trust store.
    /// The driver layers `verify_peer` from [super::Settings] on top.
    Default,

    /// Standard verification against an explicit set of root certificates,
    /// replacing the default trust store.
    Roots(Vec<CertificateDer<'static>>),

    /// Accept the server certificate if (and only if) the SHA-256 of its DER
    /// encoding matches one of these. This bypasses CA verification entirely,
    /// mirroring the browser's `serverCertificateHashes` mechanism.
    Hashes(Vec<[u8; 32]>),
}

/// Build the client SSL context: optionally present a client certificate
/// (mTLS) and install the requested server-verification policy.
///
/// Fallible up front (at connect time) so a malformed certificate, key, or
/// root surfaces as a connection error. The [ConnectionHook] returns
/// `Option<SslContextBuilder>` and `None` silently falls back to tokio-quiche's
/// default (unverified) config, so the policy must never be dropped there.
pub(crate) fn build_client_context(
    cert: Option<&(Vec<CertificateDer<'static>>, PrivateKeyDer<'static>)>,
    verify: &ClientVerify,
) -> io::Result<SslContextBuilder> {
    let mut builder = SslContextBuilder::new(SslMethod::tls()).map_err(io::Error::other)?;

    if let Some((chain, key)) = cert {
        apply_client_cert(&mut builder, chain, key)?;
    }

    match verify {
        ClientVerify::Default => {}
        ClientVerify::Roots(roots) => {
            let mut store = X509StoreBuilder::new().map_err(io::Error::other)?;
            for der in roots {
                let cert = X509::from_der(der.as_ref()).map_err(io::Error::other)?;
                store.add_cert(cert).map_err(io::Error::other)?;
            }
            builder.set_cert_store_builder(store);
            builder.set_verify(SslVerifyMode::PEER);
        }
        ClientVerify::Hashes(hashes) => {
            let hashes = hashes.clone();
            // Fully replaces standard verification: accept the peer iff the
            // SHA-256 of its leaf DER is in the allow-list.
            builder.set_custom_verify_callback(SslVerifyMode::PEER, move |ssl| {
                let cert = ssl
                    .peer_certificate()
                    .ok_or(SslVerifyError::Invalid(SslAlert::CERTIFICATE_UNKNOWN))?;
                let digest = cert
                    .digest(MessageDigest::sha256())
                    .map_err(|_| SslVerifyError::Invalid(SslAlert::BAD_CERTIFICATE))?;
                if hashes.iter().any(|h| h.as_slice() == digest.as_ref()) {
                    Ok(())
                } else {
                    Err(SslVerifyError::Invalid(
                        SslAlert::BAD_CERTIFICATE_HASH_VALUE,
                    ))
                }
            });
        }
    }

    Ok(builder)
}

/// Set the leaf, intermediates, and private key on the builder for mTLS.
fn apply_client_cert(
    builder: &mut SslContextBuilder,
    chain: &[CertificateDer<'static>],
    key: &PrivateKeyDer<'static>,
) -> io::Result<()> {
    let leaf_der = chain.first().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "empty client certificate chain",
        )
    })?;
    let leaf = X509::from_der(leaf_der.as_ref()).map_err(io::Error::other)?;
    builder.set_certificate(&leaf).map_err(io::Error::other)?;

    for cert_der in chain.iter().skip(1) {
        let cert = X509::from_der(cert_der.as_ref()).map_err(io::Error::other)?;
        builder
            .add_extra_chain_cert(cert)
            .map_err(io::Error::other)?;
    }

    let key = der_to_boring_key(key).map_err(io::Error::other)?;
    builder.set_private_key(&key).map_err(io::Error::other)?;

    Ok(())
}

/// Client-side TLS hook holding a pre-built, pre-validated SSL context.
///
/// The context is built once at connect time (see [build_client_context]); the
/// hook just hands it over. tokio-quiche calls this once per socket.
pub(crate) struct ClientHook {
    builder: std::sync::Mutex<Option<SslContextBuilder>>,
}

impl ClientHook {
    pub fn new(builder: SslContextBuilder) -> Self {
        Self {
            builder: std::sync::Mutex::new(Some(builder)),
        }
    }
}

impl ConnectionHook for ClientHook {
    fn create_custom_ssl_context_builder(
        &self,
        _settings: TlsCertificatePaths<'_>,
    ) -> Option<SslContextBuilder> {
        let builder = self.builder.lock().unwrap().take();
        if builder.is_none() {
            // Should be unreachable: the hook is invoked once per socket. Falling
            // back to the default config would drop the verification policy, so
            // refuse loudly rather than silently downgrading.
            tracing::error!("client SSL context requested more than once; refusing to reuse");
        }
        builder
    }
}
