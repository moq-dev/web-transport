//! CONNECT response metadata survives both session creation and rejection.
#![cfg(any(feature = "aws-lc-rs", feature = "ring"))]

use std::net::Ipv4Addr;

use anyhow::{Context, Result};
use http::{header::SET_COOKIE, HeaderValue, StatusCode};
use rcgen::{CertifiedKey, KeyPair};
use web_transport_noq::{
    noq::rustls::pki_types::{PrivateKeyDer, PrivatePkcs8KeyDer},
    proto::ConnectResponse,
    ClientBuilder, ClientError, ConnectError, ServerBuilder,
};

fn response(status: StatusCode) -> ConnectResponse {
    ConnectResponse::new(status)
        .with_header(SET_COOKIE, HeaderValue::from_static("a=1"))
        .with_header(SET_COOKIE, HeaderValue::from_static("b=2"))
        .with_header(
            http::HeaderName::from_static("x-padding"),
            HeaderValue::from_str(&"x".repeat(
                16 * 1024
                    - (32 + 7 + 3)
                    - (32 + "sec-webtransport-http3-draft".len() + 7)
                    - 2 * (32 + "set-cookie".len() + 3)
                    - (32 + "x-padding".len()),
            ))
            .unwrap(),
        )
}

fn assert_headers(response: &ConnectResponse) {
    assert_eq!(
        response
            .headers
            .get_all(SET_COOKIE)
            .iter()
            .map(|value| value.to_str().unwrap())
            .collect::<Vec<_>>(),
        ["a=1", "b=2"]
    );
}

#[tokio::test]
async fn response_and_rejection_preserve_headers() -> Result<()> {
    #[cfg(all(feature = "aws-lc-rs", feature = "ring"))]
    let _ = web_transport_noq::noq::rustls::crypto::aws_lc_rs::default_provider().install_default();

    for status in [
        StatusCode::OK,
        StatusCode::FORBIDDEN,
        StatusCode::NO_CONTENT,
    ] {
        let CertifiedKey { cert, signing_key } =
            rcgen::generate_simple_self_signed(vec!["localhost".into()])?;
        let chain = vec![cert.der().clone()];
        let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(KeyPair::serialize_der(
            &signing_key,
        )));
        let mut server = ServerBuilder::new()
            .with_addr((Ipv4Addr::LOCALHOST, 0).into())
            .with_certificate(chain.clone(), key)?;
        let url =
            format!("https://127.0.0.1:{}/", server.local_addr()?.port()).parse::<url::Url>()?;
        let client = ClientBuilder::new().with_server_certificates(chain)?;
        let serving = async {
            let request = server.accept().await.context("no request")?;
            if status == StatusCode::OK {
                Ok(Some(request.respond(response(status)).await?))
            } else {
                request.reject(response(status)).await?;
                anyhow::Ok(None)
            }
        };
        let (server_session, connected) = tokio::join!(serving, client.connect(url));
        let server_session = server_session?;
        if status == StatusCode::OK {
            let client_session = connected?;
            assert_headers(client_session.response().context("no response")?);
            assert_headers(
                server_session
                    .context("no server session")?
                    .response()
                    .context("no server response")?,
            );
            client_session.close(0, b"done");
        } else {
            let error = connected.err().context("rejection established a session")?;
            let ClientError::HttpError(ConnectError::ErrorStatus(response)) = error else {
                anyhow::bail!("rejection lost response: {error:?}");
            };
            assert_eq!(response.status, status);
            assert_headers(&response);
        }
    }
    Ok(())
}
