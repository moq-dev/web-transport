import pytest

import web_transport


@pytest.fixture
def self_signed_cert():
    cert, key = web_transport.generate_self_signed(["localhost", "127.0.0.1", "::1"])
    return cert, key


@pytest.fixture
def cert_hash(self_signed_cert):
    cert, _ = self_signed_cert
    return web_transport.certificate_hash(cert)
