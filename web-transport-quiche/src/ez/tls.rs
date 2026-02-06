use std::sync::Arc;

use boring::ec::EcKey;
use boring::pkey::{PKey, Private};
use boring::rsa::Rsa;
use boring::ssl::{
    AlpnError, ClientHello, NameType, SelectCertError, SslContextBuilder, SslMethod,
};
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
            Err(PKey::<Private>::private_key_from_der(&[]).unwrap_err())
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
}

impl ConnectionHook for StaticCertHook {
    fn create_custom_ssl_context_builder(
        &self,
        _settings: TlsCertificatePaths<'_>,
    ) -> Option<SslContextBuilder> {
        let mut builder = SslContextBuilder::new(SslMethod::tls())
            .inspect_err(|err| tracing::warn!(%err, "failed to create SSL context"))
            .ok()?;

        // Set the leaf certificate.
        let leaf = X509::from_der(self.chain.first()?.as_ref())
            .inspect_err(|err| tracing::warn!(%err, "failed to parse leaf certificate DER"))
            .ok()?;
        builder
            .set_certificate(&leaf)
            .inspect_err(|err| tracing::warn!(%err, "failed to set leaf certificate"))
            .ok()?;

        // Set intermediate certificates.
        for cert_der in self.chain.iter().skip(1) {
            let cert = X509::from_der(cert_der.as_ref())
                .inspect_err(|err| tracing::warn!(%err, "failed to parse intermediate certificate DER"))
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
                alpn_select(alpn.as_slice(), client).ok_or(AlpnError::NOACK)
            });
        }

        Some(builder)
    }
}

pub(crate) struct DynamicCertHook {
    pub resolver: Arc<dyn CertResolver>,
    pub alpn: Vec<Vec<u8>>,
}

impl ConnectionHook for DynamicCertHook {
    fn create_custom_ssl_context_builder(
        &self,
        _settings: TlsCertificatePaths<'_>,
    ) -> Option<SslContextBuilder> {
        let mut builder = SslContextBuilder::new(SslMethod::tls())
            .inspect_err(|err| tracing::warn!(%err, "failed to create SSL context"))
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
                    .inspect_err(|err| tracing::warn!(%err, "failed to parse intermediate certificate DER"))
                    .map_err(|_| SelectCertError::ERROR)?;
                ssl.add_chain_cert(&cert)
                    .inspect_err(|err| tracing::warn!(%err, "failed to add intermediate certificate"))
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
                alpn_select(alpn.as_slice(), client).ok_or(AlpnError::NOACK)
            });
        }

        Some(builder)
    }
}
