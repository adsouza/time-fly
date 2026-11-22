"""``timefly-fetch``: download the pinned raw files listed in ``data/sources.json``.

Each entry gives ``url``, ``path`` (relative to the raw directory), ``sha256``, ``bytes`` and
``licence``. A file whose checksum already matches is skipped without any network access. A
missing (or mismatching) file is downloaded to a temporary name beside its target, checked for
size and SHA-256, then renamed into place; on any mismatch the temporary file is deleted and the
command exits non-zero naming the file. The whole list is validated before anything is fetched.
Besides the listed URLs, the only network access is the AIA fallback below: for a host that
omits its intermediate certificate, the issuer named in its own leaf certificate.
"""

from __future__ import annotations

import argparse
import hashlib
import http.client
import json
import logging
import os
import re
import ssl
import tempfile
import time
import urllib.error
import urllib.request
from dataclasses import dataclass
from pathlib import Path, PurePosixPath
from urllib.parse import urlsplit

from timefly_bake import log
from timefly_bake.log import traced

_log = logging.getLogger(__name__)

_SHA256 = re.compile(r"[0-9a-f]{64}")
_LOOPBACK = {"127.0.0.1", "localhost", "::1"}
_CHUNK = 1 << 20
_TIMEOUT_S = 60
_ATTEMPTS = 3
_USER_AGENT = "timefly-fetch/0.1"


class SourcesError(ValueError):
    """``sources.json`` is malformed or names an unsafe path or URL."""


class FetchError(RuntimeError):
    """A download failed or did not match its pinned size and checksum."""


@dataclass(frozen=True)
class Source:
    url: str
    path: str
    sha256: str
    bytes: int
    licence: str


def _check_path(path: object) -> str:
    if not isinstance(path, str) or not path or "\\" in path or "\0" in path:
        raise SourcesError(f"bad path {path!r}")
    pure = PurePosixPath(path)
    if pure.is_absolute() or any(part in ("", ".", "..") for part in path.split("/")):
        raise SourcesError(f"path {path!r} must stay inside the raw directory")
    return pure.as_posix()


def _check_url(url: object, path: str) -> str:
    if not isinstance(url, str):
        raise SourcesError(f"{path}: bad url {url!r}")
    parts = urlsplit(url)
    if parts.scheme == "https" and parts.hostname:
        return url
    if parts.scheme == "http" and parts.hostname in _LOOPBACK:
        return url  # local test servers only
    raise SourcesError(f"{path}: url must be https: {url!r}")


def _parse_entry(raw: object) -> Source:
    if not isinstance(raw, dict):
        raise SourcesError(f"entry is not an object: {raw!r}")
    path = _check_path(raw.get("path"))
    url = _check_url(raw.get("url"), path)
    sha = raw.get("sha256")
    if not isinstance(sha, str) or not _SHA256.fullmatch(sha):
        raise SourcesError(f"{path}: sha256 must be 64 lowercase hex digits")
    size = raw.get("bytes")
    if type(size) is not int or size <= 0:
        raise SourcesError(f"{path}: bytes must be a positive integer")
    licence = raw.get("licence")
    if not isinstance(licence, str) or not licence.strip():
        raise SourcesError(f"{path}: licence is required")
    return Source(url=url, path=path, sha256=sha, bytes=size, licence=licence)


@traced
def load_sources(path: Path) -> list[Source]:
    """Parse and validate ``sources.json`` (``{"files": [...]}``); raise ``SourcesError``."""
    try:
        doc = json.loads(Path(path).read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as exc:
        raise SourcesError(f"cannot read {path}: {exc}") from exc
    files = doc.get("files") if isinstance(doc, dict) else None
    if not isinstance(files, list) or not files:
        raise SourcesError(f"{path}: expected a non-empty 'files' list")
    sources = [_parse_entry(item) for item in files]
    seen: set[str] = set()
    for s in sources:
        key = s.path.casefold()
        if key in seen:
            raise SourcesError(f"duplicate path {s.path}")
        seen.add(key)
    return sources


@traced
def sha256_file(path: Path) -> str:
    """Hex SHA-256 of a file."""
    digest = hashlib.sha256()
    with open(path, "rb") as fh:
        while chunk := fh.read(_CHUNK):
            digest.update(chunk)
    return digest.hexdigest()


def _target(raw_dir: Path, source: Source) -> Path:
    root = raw_dir.resolve()
    target = (root / source.path).resolve()
    if not target.is_relative_to(root) or target == root:
        raise SourcesError(f"path {source.path!r} resolves outside {raw_dir}")
    return target


def _download(source: Source, part: Path, context: ssl.SSLContext | None = None) -> str:
    """Stream ``source.url`` into ``part``; return its SHA-256. Raise ``FetchError``."""
    request = urllib.request.Request(source.url, headers={"User-Agent": _USER_AGENT})
    digest = hashlib.sha256()
    count = 0
    with (
        urllib.request.urlopen(request, timeout=_TIMEOUT_S, context=context) as response,
        open(part, "wb") as out,
    ):
        while chunk := response.read(_CHUNK):
            count += len(chunk)
            if count > source.bytes:
                raise FetchError(
                    f"{source.path}: more than the pinned {source.bytes} bytes from {source.url}"
                )
            digest.update(chunk)
            out.write(chunk)
    if count != source.bytes:
        raise FetchError(f"{source.path}: got {count} bytes, expected {source.bytes}")
    return digest.hexdigest()


# --- Missing intermediate certificates (AIA) ------------------------------------------------
#
# Some hosts send only their leaf certificate. Browsers and macOS fetch the missing issuer from
# the leaf's Authority Information Access "CA Issuers" URL; OpenSSL (Python, Linux) does not.
# Only for that exact verification failure we do the same, one hop: read the leaf over a
# throwaway unverified handshake (used for nothing else), download its issuer (size-capped,
# never self-signed, subject must equal the leaf's issuer), and retry the download with the
# system roots plus that certificate, PARTIAL_CHAIN cleared so the chain must still end at a
# system root. Hostname checking stays on, and the file's pinned SHA-256 is the final check.

_UNABLE_TO_GET_ISSUER_CERT_LOCALLY = 20  # X509_V_ERR_UNABLE_TO_GET_ISSUER_CERT_LOCALLY
AIA_MAX_BYTES = 16 * 1024
_AIA_CONTEXTS: dict[tuple[str, int], ssl.SSLContext] = {}


class AiaError(RuntimeError):
    """The missing issuer certificate could not be fetched or was not acceptable."""


@dataclass(frozen=True)
class Intermediate:
    der: bytes
    subject: str
    sha256: str


@traced
def needs_aia(exc: BaseException, url: str) -> bool:
    """True only for an https URL whose verification failed with "unable to get local issuer"."""
    if urlsplit(url).scheme != "https":
        return False
    reason = getattr(exc, "reason", exc) if isinstance(exc, urllib.error.URLError) else exc
    if isinstance(exc, urllib.error.HTTPError):
        return False
    return (
        isinstance(reason, ssl.SSLCertVerificationError)
        and getattr(reason, "verify_code", None) == _UNABLE_TO_GET_ISSUER_CERT_LOCALLY
    )


@traced
def issuer_url(leaf_der: bytes) -> str:
    """The leaf certificate's AIA "CA Issuers" URL (http or https). Raise ``AiaError``."""
    from cryptography import x509
    from cryptography.x509.oid import AuthorityInformationAccessOID

    leaf = x509.load_der_x509_certificate(leaf_der)
    try:
        aia = leaf.extensions.get_extension_for_class(x509.AuthorityInformationAccess).value
    except x509.ExtensionNotFound as exc:
        raise AiaError("leaf certificate has no Authority Information Access extension") from exc
    for desc in aia:
        if desc.access_method == AuthorityInformationAccessOID.CA_ISSUERS and isinstance(
            desc.access_location, x509.UniformResourceIdentifier
        ):
            url = desc.access_location.value
            if urlsplit(url).scheme in ("http", "https"):
                return url
    raise AiaError("leaf certificate names no http(s) CA Issuers URL")


@traced
def parse_intermediate(data: bytes, leaf_der: bytes) -> Intermediate:
    """Accept a DER or PEM issuer certificate for ``leaf_der``; raise ``AiaError`` otherwise.

    Refuses oversized data, self-signed certificates, non-CA certificates and certificates
    whose subject is not the leaf's issuer.
    """
    from cryptography import x509
    from cryptography.hazmat.primitives import serialization

    if len(data) > AIA_MAX_BYTES:
        raise AiaError(f"issuer certificate is larger than {AIA_MAX_BYTES} bytes")
    try:
        if data.lstrip().startswith(b"-----BEGIN CERTIFICATE-----"):
            cert = x509.load_pem_x509_certificate(data)
        else:
            cert = x509.load_der_x509_certificate(data)
    except ValueError as exc:
        raise AiaError(f"issuer certificate is not a DER or PEM certificate: {exc}") from exc
    leaf = x509.load_der_x509_certificate(leaf_der)
    if cert.subject == cert.issuer:
        raise AiaError(f"issuer certificate {cert.subject.rfc4514_string()} is self-signed")
    try:
        ca = cert.extensions.get_extension_for_class(x509.BasicConstraints).value.ca
    except x509.ExtensionNotFound:
        ca = False
    if not ca:
        raise AiaError(f"issuer certificate {cert.subject.rfc4514_string()} is not a CA")
    if cert.subject != leaf.issuer:
        raise AiaError(
            f"issuer certificate {cert.subject.rfc4514_string()} is not the leaf's issuer "
            f"{leaf.issuer.rfc4514_string()}"
        )
    der = cert.public_bytes(serialization.Encoding.DER)
    return Intermediate(der, cert.subject.rfc4514_string(), hashlib.sha256(der).hexdigest())


def _base_context() -> ssl.SSLContext:
    return ssl.create_default_context()


@traced
def context_with_intermediate(intermediate: Intermediate) -> ssl.SSLContext:
    """System roots plus ``intermediate`` as an untrusted chain link (not a trust anchor)."""
    ctx = _base_context()
    ctx.verify_flags &= ~ssl.VERIFY_X509_PARTIAL_CHAIN
    ctx.check_hostname = True
    ctx.verify_mode = ssl.CERT_REQUIRED
    ctx.load_verify_locations(cadata=intermediate.der)
    return ctx


def _leaf_der(host: str, port: int) -> bytes:
    """The server's leaf certificate, from a handshake used only to read it."""
    import socket

    probe = ssl.create_default_context()
    probe.check_hostname = False
    probe.verify_mode = ssl.CERT_NONE
    with socket.create_connection((host, port), timeout=_TIMEOUT_S) as sock:
        with probe.wrap_socket(sock, server_hostname=host) as tls:
            der = tls.getpeercert(binary_form=True)
    if not der:
        raise AiaError(f"{host}:{port} sent no certificate")
    return der


def _get_capped(url: str, cap: int) -> bytes:
    request = urllib.request.Request(url, headers={"User-Agent": _USER_AGENT})
    with urllib.request.urlopen(request, timeout=_TIMEOUT_S) as response:
        data = response.read(cap + 1)
    if len(data) > cap:
        raise AiaError(f"issuer certificate at {url} is larger than {cap} bytes")
    return data


@traced
def aia_context(host: str, port: int) -> ssl.SSLContext:
    """A verifying context for ``host`` with its missing intermediate (cached for the run)."""
    key = (host, port)
    if key in _AIA_CONTEXTS:
        return _AIA_CONTEXTS[key]
    try:
        leaf = _leaf_der(host, port)
        url = issuer_url(leaf)
        intermediate = parse_intermediate(_get_capped(url, AIA_MAX_BYTES), leaf)
    except AiaError:
        raise
    except (OSError, urllib.error.URLError, http.client.HTTPException, ValueError) as exc:
        raise AiaError(f"fetching the issuer certificate for {host} failed: {exc}") from exc
    ctx = context_with_intermediate(intermediate)
    _log.warning(
        "%s does not send its intermediate certificate; using %s from its AIA URL %s "
        "(sha256 %s), verified up to a system root",
        host,
        intermediate.subject,
        url,
        intermediate.sha256,
    )
    _AIA_CONTEXTS[key] = ctx
    return ctx


def _retryable(exc: BaseException) -> bool:
    if isinstance(exc, urllib.error.HTTPError):
        return exc.code >= 500 or exc.code == 429
    if isinstance(getattr(exc, "reason", None), ssl.SSLError) or isinstance(exc, ssl.SSLError):
        return False  # a certificate problem will not fix itself; never retried or bypassed
    return isinstance(
        exc, (urllib.error.URLError, TimeoutError, ConnectionError, http.client.HTTPException)
    )


@traced
def fetch_file(source: Source, raw_dir: Path) -> str:
    """Make ``raw_dir/source.path`` match its pin; return ``"verified"`` or ``"downloaded"``."""
    target = _target(Path(raw_dir), source)
    if target.is_file():
        if target.stat().st_size == source.bytes and sha256_file(target) == source.sha256:
            return "verified"
        _log.warning("%s: present but does not match its pin; downloading again", source.path)
    target.parent.mkdir(parents=True, exist_ok=True)
    parts = urlsplit(source.url)
    host_key = (parts.hostname or "", parts.port or 443)
    context = _AIA_CONTEXTS.get(host_key) if parts.scheme == "https" else None
    attempt = 1
    while True:
        fd, name = tempfile.mkstemp(prefix=f".{target.name}.", suffix=".part", dir=target.parent)
        os.close(fd)
        part = Path(name)
        try:
            got = _download(source, part, context)
            if got != source.sha256:
                raise FetchError(f"{source.path}: SHA-256 {got} does not match {source.sha256}")
            os.chmod(part, 0o644)  # mkstemp creates 0600
            os.replace(part, target)
            return "downloaded"
        except FetchError:
            raise
        except (urllib.error.URLError, OSError, http.client.HTTPException) as exc:
            if context is None and needs_aia(exc, source.url):
                try:
                    context = aia_context(*host_key)
                except AiaError as aia_exc:
                    raise FetchError(
                        f"{source.path}: download from {source.url} failed: {exc}; "
                        f"AIA issuer fetch failed: {aia_exc}"
                    ) from aia_exc
                continue  # one more try with the completed chain; not counted as an attempt
            if attempt < _ATTEMPTS and _retryable(exc):
                _log.warning("%s: attempt %d failed (%s); retrying", source.path, attempt, exc)
                time.sleep(2**attempt)
                attempt += 1
                continue
            raise FetchError(f"{source.path}: download from {source.url} failed: {exc}") from exc
        finally:
            part.unlink(missing_ok=True)


def _parser() -> argparse.ArgumentParser:
    p = argparse.ArgumentParser(
        prog="timefly-fetch", description="Download and verify the pinned raw data files."
    )
    p.add_argument("--sources", type=Path, required=True, help="data/sources.json")
    p.add_argument("--raw", type=Path, required=True, help="raw download tree (data/raw)")
    p.add_argument("--log-level", default="INFO", help="stderr log level (default INFO)")
    return p


@traced
def main(argv=None) -> int:
    """Entry point of ``timefly-fetch``; returns the process exit code."""
    args = _parser().parse_args(argv)
    log.configure(None, args.log_level)
    try:
        sources = load_sources(args.sources)
        for s in sources:
            _target(args.raw, s)
    except SourcesError as exc:
        _log.error("invalid sources: %s", exc)
        return 2
    failed = []
    counts = {"verified": 0, "downloaded": 0}
    for s in sources:
        try:
            status = fetch_file(s, args.raw)
        except (FetchError, OSError) as exc:
            _log.error("FAILED %s: %s", s.path, exc)
            failed.append(s.path)
            continue
        counts[status] += 1
        _log.info("%-10s %s (%d bytes)", status, s.path, s.bytes)
    _log.info(
        "%d verified, %d downloaded, %d failed", counts["verified"], counts["downloaded"], len(failed)
    )
    if failed:
        _log.error("failed: %s", ", ".join(failed))
        return 1
    return 0
