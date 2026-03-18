"""Pure-Python certificate utilities using the cryptography library."""

import datetime
import hashlib
import ipaddress

from cryptography import x509
from cryptography.hazmat.primitives import hashes, serialization
from cryptography.hazmat.primitives.asymmetric import ec
from cryptography.x509.oid import NameOID


def generate_self_signed(
    subject_alt_names: list[str],
) -> tuple[bytes, bytes]:
    """Generate a self-signed certificate and private key.

    Uses ECDSA P-256 and a 14-day validity period (the maximum allowed by
    WebTransport for certificate pinning).

    Args:
        subject_alt_names: SANs for the certificate
            (e.g. ``["localhost", "127.0.0.1", "::1"]``).

    Returns:
        A ``(certificate_der, private_key_der)`` tuple of raw DER bytes.
    """
    if not subject_alt_names:
        raise ValueError("subject_alt_names must not be empty")

    key = ec.generate_private_key(ec.SECP256R1())

    sans: list[x509.GeneralName] = []
    for name in subject_alt_names:
        try:
            addr = ipaddress.ip_address(name)
            sans.append(x509.IPAddress(addr))
        except ValueError:
            sans.append(x509.DNSName(name))

    subject = x509.Name([x509.NameAttribute(NameOID.COMMON_NAME, "web-transport")])
    now = datetime.datetime.now(datetime.timezone.utc)

    cert = (
        x509.CertificateBuilder()
        .subject_name(subject)
        .issuer_name(subject)
        .public_key(key.public_key())
        .serial_number(x509.random_serial_number())
        .not_valid_before(now)
        .not_valid_after(now + datetime.timedelta(days=14))
        .add_extension(x509.SubjectAlternativeName(sans), critical=False)
        .sign(key, hashes.SHA256())
    )

    cert_der = cert.public_bytes(serialization.Encoding.DER)
    key_der = key.private_bytes(
        serialization.Encoding.DER,
        serialization.PrivateFormat.PKCS8,
        serialization.NoEncryption(),
    )
    return cert_der, key_der


def certificate_hash(certificate_der: bytes) -> bytes:
    """Compute the SHA-256 fingerprint of a DER-encoded certificate.

    Args:
        certificate_der: Raw DER-encoded certificate bytes.

    Returns:
        32 bytes (SHA-256 digest).
    """
    return hashlib.sha256(certificate_der).digest()
