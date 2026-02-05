use std::sync::Arc;

use boring::ec::EcKey;
use boring::pkey::{PKey, Private};
use boring::rsa::Rsa;
use boring::ssl::{ClientHello, NameType, SelectCertError, SslContextBuilder, SslMethod};
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
        PrivateKeyDer::Pkcs1(d) => {
            Ok(PKey::from_rsa(Rsa::private_key_from_der(
                d.secret_pkcs1_der(),
            )?)?)
        }
        PrivateKeyDer::Sec1(d) => {
            Ok(PKey::from_ec_key(EcKey::private_key_from_der(
                d.secret_sec1_der(),
            )?)?)
        }
        _ => Err(PKey::<Private>::private_key_from_der(&[]).unwrap_err()),
    }
}

fn alpn_wire_format(alpn: &[Vec<u8>]) -> Vec<u8> {
    let mut wire = Vec::new();
    for proto in alpn {
        wire.push(proto.len() as u8);
        wire.extend_from_slice(proto);
    }
    wire
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
        let mut builder = SslContextBuilder::new(SslMethod::tls()).ok()?;

        // Set the leaf certificate.
        let leaf = X509::from_der(self.chain.first()?.as_ref()).ok()?;
        builder.set_certificate(&leaf).ok()?;

        // Set intermediate certificates.
        for cert_der in self.chain.iter().skip(1) {
            let cert = X509::from_der(cert_der.as_ref()).ok()?;
            builder.add_extra_chain_cert(cert).ok()?;
        }

        // Set the private key.
        let key = der_to_boring_key(&self.key).ok()?;
        builder.set_private_key(&key).ok()?;

        // Set ALPN protocols.
        if !self.alpn.is_empty() {
            let wire = alpn_wire_format(&self.alpn);
            builder.set_alpn_protos(&wire).ok()?;
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
        let mut builder = SslContextBuilder::new(SslMethod::tls()).ok()?;

        let resolver = self.resolver.clone();
        let alpn = self.alpn.clone();

        builder.set_select_certificate_callback(move |mut client_hello: ClientHello<'_>| {
            let sni = client_hello.servername(NameType::HOST_NAME);
            let certified = resolver
                .resolve(sni)
                .ok_or(SelectCertError::ERROR)?;

            let ssl = client_hello.ssl_mut();

            // Set the leaf certificate.
            let leaf = X509::from_der(
                certified
                    .chain
                    .first()
                    .ok_or(SelectCertError::ERROR)?
                    .as_ref(),
            )
            .map_err(|_| SelectCertError::ERROR)?;
            ssl.set_certificate(&leaf)
                .map_err(|_| SelectCertError::ERROR)?;

            // Set intermediate certificates.
            for cert_der in certified.chain.iter().skip(1) {
                let cert =
                    X509::from_der(cert_der.as_ref()).map_err(|_| SelectCertError::ERROR)?;
                ssl.add_chain_cert(&cert)
                    .map_err(|_| SelectCertError::ERROR)?;
            }

            // Set the private key.
            let key =
                der_to_boring_key(&certified.key).map_err(|_| SelectCertError::ERROR)?;
            ssl.set_private_key(&key)
                .map_err(|_| SelectCertError::ERROR)?;

            // Set ALPN protocols.
            if !alpn.is_empty() {
                let wire = alpn_wire_format(&alpn);
                ssl.set_alpn_protos(&wire)
                    .map_err(|_| SelectCertError::ERROR)?;
            }

            Ok(())
        });

        // Set ALPN on the context level too (for cases where callback isn't invoked).
        if !self.alpn.is_empty() {
            let wire = alpn_wire_format(&self.alpn);
            builder.set_alpn_protos(&wire).ok()?;
        }

        Some(builder)
    }
}
