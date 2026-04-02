import pytest

import web_transport


def test_generate_self_signed_empty_sans():
    """generate_self_signed([]) -> ValueError."""
    with pytest.raises(ValueError, match="must not be empty"):
        web_transport.generate_self_signed([])


def test_generate_self_signed_returns_bytes():
    cert, key = web_transport.generate_self_signed(["localhost"])
    assert isinstance(cert, bytes)
    assert isinstance(key, bytes)
    assert len(cert) > 0
    assert len(key) > 0


def test_certificate_hash_returns_32_bytes():
    cert, _ = web_transport.generate_self_signed(["localhost"])
    h = web_transport.certificate_hash(cert)
    assert isinstance(h, bytes)
    assert len(h) == 32


def test_certificate_hash_is_deterministic():
    cert, _ = web_transport.generate_self_signed(["localhost"])
    h1 = web_transport.certificate_hash(cert)
    h2 = web_transport.certificate_hash(cert)
    assert h1 == h2


def test_generate_self_signed_with_ip():
    cert, key = web_transport.generate_self_signed(["127.0.0.1", "::1"])
    assert len(cert) > 0
    assert len(key) > 0
