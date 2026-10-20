import sys
from importlib.metadata import distribution
from pathlib import Path

import pytest

import web_transport


def pytest_addoption(parser):
    parser.addoption(
        "--installed-wheel",
        action="store_true",
        help="Require web_transport imports to belong to the installed wheel",
    )


def pytest_sessionstart(session):
    if not session.config.getoption("--installed-wheel"):
        return
    dist = distribution("web-transport-rs")
    installed = {Path(dist.locate_file(file)).resolve() for file in dist.files or []}
    for name, module in tuple(sys.modules.items()):
        if name == "web_transport" or name.startswith("web_transport."):
            path = Path(module.__file__).resolve()
            if path not in installed or not path.is_relative_to(sys.prefix):
                raise pytest.UsageError(
                    f"{name} loaded outside installed wheel: {path}"
                )


def pytest_report_header(config):
    if config.getoption("--installed-wheel"):
        dist = distribution("web-transport-rs")
        return f"Installed wheel: {dist.metadata['Name']} {dist.version}, {web_transport.__file__}"


@pytest.fixture
def self_signed_cert():
    cert, key = web_transport.generate_self_signed(["localhost", "127.0.0.1", "::1"])
    return cert, key


@pytest.fixture
def cert_hash(self_signed_cert):
    cert, _ = self_signed_cert
    return web_transport.certificate_hash(cert)
