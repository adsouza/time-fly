"""AIA fallback in ``timefly-fetch``: completing a chain whose server omits the intermediate.

A test PKI (root -> intermediate -> leaf for 127.0.0.1) is generated per test. The HTTPS server
sends only the leaf; the leaf's AIA URL points at a local plain-HTTP server holding the
intermediate. "System roots" are stood in for by monkeypatching ``fetch._base_context``.
"""

import datetime
import hashlib
import ipaddress
import json
import ssl
import threading
import urllib.error
from functools import partial
from http.server import SimpleHTTPRequestHandler, ThreadingHTTPServer

import pytest
from cryptography import x509
from cryptography.hazmat.primitives import hashes, serialization
from cryptography.hazmat.primitives.asymmetric import ec
from cryptography.x509.oid import AuthorityInformationAccessOID, ExtendedKeyUsageOID, NameOID

from timefly_bake import fetch

PAYLOAD = b"ice sheet slice\n" * 200


# --- test PKI ---------------------------------------------------------------------------------


def _name(cn):
    return x509.Name([x509.NameAttribute(NameOID.COMMON_NAME, cn)])


def _cert(subject, key, issuer, issuer_key, *, ca, aia_url=None, san_ip=None):
    now = datetime.datetime.now(datetime.timezone.utc)
    b = (
        x509.CertificateBuilder()
        .subject_name(_name(subject))
        .issuer_name(_name(issuer))
        .public_key(key.public_key())
        .serial_number(x509.random_serial_number())
        .not_valid_before(now - datetime.timedelta(days=1))
        .not_valid_after(now + datetime.timedelta(days=30))
        .add_extension(x509.BasicConstraints(ca=ca, path_length=None), critical=True)
        .add_extension(x509.SubjectKeyIdentifier.from_public_key(key.public_key()), critical=False)
        .add_extension(
            x509.AuthorityKeyIdentifier.from_issuer_public_key(issuer_key.public_key()),
            critical=False,
        )
    )
    if ca:
        b = b.add_extension(
            x509.KeyUsage(
                digital_signature=True, content_commitment=False, key_encipherment=False,
                data_encipherment=False, key_agreement=False, key_cert_sign=True,
                crl_sign=True, encipher_only=False, decipher_only=False,
            ),
            critical=True,
        )
    else:
        b = b.add_extension(
            x509.KeyUsage(
                digital_signature=True, content_commitment=False, key_encipherment=False,
                data_encipherment=False, key_agreement=False, key_cert_sign=False,
                crl_sign=False, encipher_only=False, decipher_only=False,
            ),
            critical=True,
        ).add_extension(x509.ExtendedKeyUsage([ExtendedKeyUsageOID.SERVER_AUTH]), critical=False)
    if san_ip:
        b = b.add_extension(
            x509.SubjectAlternativeName([x509.IPAddress(ipaddress.ip_address(san_ip))]),
            critical=False,
        )
    if aia_url:
        b = b.add_extension(
            x509.AuthorityInformationAccess(
                [
                    x509.AccessDescription(
                        AuthorityInformationAccessOID.OCSP,
                        x509.UniformResourceIdentifier("http://ocsp.invalid/"),
                    ),
                    x509.AccessDescription(
                        AuthorityInformationAccessOID.CA_ISSUERS,
                        x509.UniformResourceIdentifier(aia_url),
                    ),
                ]
            ),
            critical=False,
        )
    return b.sign(issuer_key, hashes.SHA256())


def _der(cert):
    return cert.public_bytes(serialization.Encoding.DER)


def _pem(cert):
    return cert.public_bytes(serialization.Encoding.PEM)


class Pki:
    def __init__(self, aia_url):
        self.root_key = ec.generate_private_key(ec.SECP256R1())
        self.int_key = ec.generate_private_key(ec.SECP256R1())
        self.leaf_key = ec.generate_private_key(ec.SECP256R1())
        self.root = _cert("Test Root", self.root_key, "Test Root", self.root_key, ca=True)
        self.inter = _cert("Test Intermediate", self.int_key, "Test Root", self.root_key, ca=True)
        self.leaf = _cert(
            "127.0.0.1", self.leaf_key, "Test Intermediate", self.int_key,
            ca=False, aia_url=aia_url, san_ip="127.0.0.1",
        )


# --- servers ----------------------------------------------------------------------------------


class _Handler(SimpleHTTPRequestHandler):
    def __init__(self, *args, requests, **kwargs):
        self._requests = requests
        super().__init__(*args, **kwargs)

    def do_GET(self):  # noqa: N802
        self._requests.append(self.path)
        super().do_GET()

    def log_message(self, format, *args):
        pass


def _serve(httpd):
    t = threading.Thread(target=httpd.serve_forever, kwargs={"poll_interval": 0.02}, daemon=True)
    t.start()
    return t


@pytest.fixture(autouse=True)
def _clear_aia_cache():
    fetch._AIA_CONTEXTS.clear()
    yield
    fetch._AIA_CONTEXTS.clear()


@pytest.fixture
def setup(tmp_path, monkeypatch):
    """An HTTPS server sending only its leaf, plus an HTTP server holding the intermediate."""
    www = tmp_path / "www"
    www.mkdir()
    (www / "payload.bin").write_bytes(PAYLOAD)
    (www / "payload2.bin").write_bytes(PAYLOAD * 2)
    aia_requests: list[str] = []
    aia_httpd = ThreadingHTTPServer(
        ("127.0.0.1", 0), partial(_Handler, directory=str(www), requests=aia_requests)
    )
    aia_url = f"http://127.0.0.1:{aia_httpd.server_address[1]}/issuer.crt"
    pki = Pki(aia_url)
    (www / "issuer.crt").write_bytes(_der(pki.inter))
    (tmp_path / "leaf.pem").write_bytes(_pem(pki.leaf))
    (tmp_path / "leaf.key").write_bytes(
        pki.leaf_key.private_bytes(
            serialization.Encoding.PEM,
            serialization.PrivateFormat.PKCS8,
            serialization.NoEncryption(),
        )
    )
    (tmp_path / "root.pem").write_bytes(_pem(pki.root))
    tls_requests: list[str] = []
    https = ThreadingHTTPServer(
        ("127.0.0.1", 0), partial(_Handler, directory=str(www), requests=tls_requests)
    )
    server_ctx = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
    server_ctx.load_cert_chain(tmp_path / "leaf.pem", tmp_path / "leaf.key")  # leaf only
    https.socket = server_ctx.wrap_socket(https.socket, server_side=True)
    threads = [_serve(aia_httpd), _serve(https)]
    # "System roots": the test root only.
    monkeypatch.setattr(
        fetch, "_base_context", lambda: ssl.create_default_context(cafile=tmp_path / "root.pem")
    )
    # The first, plain attempt uses the real system store, which doesn't know the test root
    # either, so it fails with "unable to get local issuer certificate" like the real host.
    base = f"https://127.0.0.1:{https.server_address[1]}"
    try:
        yield {"base": base, "pki": pki, "aia": aia_requests, "tls": tls_requests,
               "tmp": tmp_path}
    finally:
        for h in (aia_httpd, https):
            h.shutdown()
            h.server_close()
        for t in threads:
            t.join()


def _source(base, name="payload.bin", data=PAYLOAD):
    return fetch.Source(
        url=f"{base}/{name}", path=f"ice6g/{name}", sha256=hashlib.sha256(data).hexdigest(),
        bytes=len(data), licence="test",
    )


# --- end to end -------------------------------------------------------------------------------


def test_missing_intermediate_is_fetched_via_aia_and_verified_to_root(setup, caplog):
    caplog.set_level("WARNING", logger="timefly_bake")
    raw = setup["tmp"] / "raw"
    assert fetch.fetch_file(_source(setup["base"]), raw) == "downloaded"
    assert (raw / "ice6g" / "payload.bin").read_bytes() == PAYLOAD
    assert setup["aia"] == ["/issuer.crt"]
    sha = hashlib.sha256(_der(setup["pki"].inter)).hexdigest()
    assert "CN=Test Intermediate" in caplog.text and sha in caplog.text
    # Cached per host for the run: the next file neither re-fetches nor re-fails.
    assert fetch.fetch_file(_source(setup["base"], "payload2.bin", PAYLOAD * 2), raw) == (
        "downloaded"
    )
    assert setup["aia"] == ["/issuer.crt"]


def test_intermediate_is_not_a_trust_anchor(setup, monkeypatch):
    """With no matching root, the AIA certificate alone must not make the chain verify."""
    monkeypatch.setattr(fetch, "_base_context", _empty_context)
    raw = setup["tmp"] / "raw"
    with pytest.raises(fetch.FetchError, match="CERTIFICATE_VERIFY_FAILED"):
        fetch.fetch_file(_source(setup["base"]), raw)
    assert setup["aia"] == ["/issuer.crt"]
    assert not any(p.is_file() for p in raw.rglob("*"))


def _empty_context():
    ctx = ssl.SSLContext(ssl.PROTOCOL_TLS_CLIENT)  # no roots at all
    ctx.verify_flags |= ssl.VERIFY_X509_PARTIAL_CHAIN  # as Python 3.13's default context does
    return ctx


def test_partial_chain_would_have_trusted_the_intermediate(setup, tmp_path):
    """Control for the test above: with PARTIAL_CHAIN left on, the same download verifies."""
    ctx = _empty_context()
    ctx.load_verify_locations(cadata=_der(setup["pki"].inter))
    part = tmp_path / "p.part"
    assert fetch._download(_source(setup["base"]), part, ctx) == hashlib.sha256(
        PAYLOAD
    ).hexdigest()


def test_bad_aia_certificate_fails_naming_the_file(setup):
    (setup["tmp"] / "www" / "issuer.crt").write_bytes(_der(setup["pki"].root))  # self-signed
    with pytest.raises(fetch.FetchError, match=r"ice6g/payload.bin.*self-signed"):
        fetch.fetch_file(_source(setup["base"]), setup["tmp"] / "raw")


# --- decision logic ---------------------------------------------------------------------------


def _verify_error(code):
    err = ssl.SSLCertVerificationError(1, "certificate verify failed")
    err.verify_code = code
    return err


@pytest.mark.parametrize(
    ("exc", "url", "expected"),
    [
        (urllib.error.URLError(_verify_error(20)), "https://h/x", True),
        (_verify_error(20), "https://h/x", True),
        (urllib.error.URLError(_verify_error(20)), "http://127.0.0.1/x", False),
        (urllib.error.URLError(_verify_error(10)), "https://h/x", False),  # expired
        (urllib.error.URLError(_verify_error(62)), "https://h/x", False),  # hostname mismatch
        (urllib.error.URLError(_verify_error(19)), "https://h/x", False),  # self-signed in chain
        (urllib.error.URLError(ssl.SSLError(1, "handshake failure")), "https://h/x", False),
        (urllib.error.URLError(TimeoutError()), "https://h/x", False),
        (urllib.error.HTTPError("https://h/x", 404, "nf", {}, None), "https://h/x", False),
        (ConnectionResetError(), "https://h/x", False),
    ],
)
def test_needs_aia_only_for_unable_to_get_local_issuer(exc, url, expected):
    assert fetch.needs_aia(exc, url) is expected


def _pki():
    return Pki("http://ca.example.org/issuer.crt")


def test_issuer_url_reads_ca_issuers():
    assert fetch.issuer_url(_der(_pki().leaf)) == "http://ca.example.org/issuer.crt"


def test_issuer_url_requires_aia():
    with pytest.raises(fetch.AiaError, match="Authority Information Access"):
        fetch.issuer_url(_der(_pki().inter))


def test_issuer_url_ignores_non_http_locations():
    p = Pki("ldap://ca.example.org/issuer")
    with pytest.raises(fetch.AiaError, match="CA Issuers"):
        fetch.issuer_url(_der(p.leaf))


@pytest.mark.parametrize("encode", [_der, _pem])
def test_parse_intermediate_accepts_der_and_pem(encode):
    p = _pki()
    got = fetch.parse_intermediate(encode(p.inter), _der(p.leaf))
    assert got.der == _der(p.inter)
    assert got.subject == "CN=Test Intermediate"
    assert got.sha256 == hashlib.sha256(_der(p.inter)).hexdigest()


def test_parse_intermediate_refuses_self_signed():
    p = _pki()
    with pytest.raises(fetch.AiaError, match="self-signed"):
        fetch.parse_intermediate(_der(p.root), _der(p.leaf))


def test_parse_intermediate_refuses_non_ca():
    p = _pki()
    with pytest.raises(fetch.AiaError, match="not a CA"):
        fetch.parse_intermediate(_der(p.leaf), _der(p.leaf))


def test_parse_intermediate_refuses_a_different_issuer():
    p, other = _pki(), _pki()
    other_int = _cert("Other Intermediate", other.int_key, "Test Root", other.root_key, ca=True)
    with pytest.raises(fetch.AiaError, match="not the leaf's issuer"):
        fetch.parse_intermediate(_der(other_int), _der(p.leaf))


def test_parse_intermediate_refuses_oversize_and_garbage():
    p = _pki()
    with pytest.raises(fetch.AiaError, match="larger than"):
        fetch.parse_intermediate(b"\0" * (fetch.AIA_MAX_BYTES + 1), _der(p.leaf))
    with pytest.raises(fetch.AiaError, match="not a DER or PEM"):
        fetch.parse_intermediate(b"<html>not a cert</html>", _der(p.leaf))


def test_context_clears_partial_chain_and_keeps_verification():
    p = _pki()
    assert ssl.create_default_context().verify_flags & ssl.VERIFY_X509_PARTIAL_CHAIN  # 3.13+
    ctx = fetch.context_with_intermediate(
        fetch.parse_intermediate(_der(p.inter), _der(p.leaf))
    )
    assert not ctx.verify_flags & ssl.VERIFY_X509_PARTIAL_CHAIN
    assert ctx.verify_mode == ssl.CERT_REQUIRED
    assert ctx.check_hostname is True
    assert any(c["subject"] == ((("commonName", "Test Intermediate"),),)
               for c in ctx.get_ca_certs())


def test_issuer_download_is_size_capped(tmp_path):
    (tmp_path / "big.crt").write_bytes(b"x" * (fetch.AIA_MAX_BYTES + 100))
    requests: list[str] = []
    httpd = ThreadingHTTPServer(
        ("127.0.0.1", 0), partial(_Handler, directory=str(tmp_path), requests=requests)
    )
    t = _serve(httpd)
    try:
        url = f"http://127.0.0.1:{httpd.server_address[1]}/big.crt"
        with pytest.raises(fetch.AiaError, match="larger than"):
            fetch._get_capped(url, fetch.AIA_MAX_BYTES)
    finally:
        httpd.shutdown()
        httpd.server_close()
        t.join()


def test_aia_context_is_cached_per_host(monkeypatch):
    p = _pki()
    calls = []
    monkeypatch.setattr(fetch, "_leaf_der", lambda h, port: calls.append(h) or _der(p.leaf))
    monkeypatch.setattr(fetch, "_get_capped", lambda url, cap: _der(p.inter))
    a = fetch.aia_context("h.example", 443)
    b = fetch.aia_context("h.example", 443)
    assert a is b and calls == ["h.example"]
    fetch.aia_context("other.example", 443)
    assert calls == ["h.example", "other.example"]


def test_main_reports_aia_failure_and_exits_nonzero(tmp_path, monkeypatch):
    sources = tmp_path / "s.json"
    sources.write_text(json.dumps({"files": [{
        "url": "https://h.example/x.bin", "path": "ice6g/x.bin",
        "sha256": "0" * 64, "bytes": 1, "licence": "t",
    }]}))

    def fail_download(source, part, context=None):
        raise urllib.error.URLError(_verify_error(20))

    monkeypatch.setattr(fetch, "_download", fail_download)
    monkeypatch.setattr(fetch, "_leaf_der", lambda h, port: b"not a certificate")
    assert fetch.main(["--sources", str(sources), "--raw", str(tmp_path / "raw")]) == 1
