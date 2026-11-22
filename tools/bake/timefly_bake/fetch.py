"""``timefly-fetch``: download the pinned raw files listed in ``data/sources.json``.

Each entry gives ``url``, ``path`` (relative to the raw directory), ``sha256``, ``bytes`` and
``licence``. A file whose checksum already matches is skipped without any network access. A
missing (or mismatching) file is downloaded to a temporary name beside its target, checked for
size and SHA-256, then renamed into place; on any mismatch the temporary file is deleted and the
command exits non-zero naming the file. The whole list is validated before anything is fetched,
and no URL other than the listed ones is ever opened.
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


def _download(source: Source, part: Path) -> str:
    """Stream ``source.url`` into ``part``; return its SHA-256. Raise ``FetchError``."""
    request = urllib.request.Request(source.url, headers={"User-Agent": _USER_AGENT})
    digest = hashlib.sha256()
    count = 0
    with urllib.request.urlopen(request, timeout=_TIMEOUT_S) as response, open(part, "wb") as out:
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
    for attempt in range(1, _ATTEMPTS + 1):
        fd, name = tempfile.mkstemp(prefix=f".{target.name}.", suffix=".part", dir=target.parent)
        os.close(fd)
        part = Path(name)
        try:
            got = _download(source, part)
            if got != source.sha256:
                raise FetchError(f"{source.path}: SHA-256 {got} does not match {source.sha256}")
            os.replace(part, target)
            return "downloaded"
        except FetchError:
            raise
        except (urllib.error.URLError, OSError, http.client.HTTPException) as exc:
            if attempt < _ATTEMPTS and _retryable(exc):
                _log.warning("%s: attempt %d failed (%s); retrying", source.path, attempt, exc)
                time.sleep(2**attempt)
                continue
            raise FetchError(f"{source.path}: download from {source.url} failed: {exc}") from exc
        finally:
            part.unlink(missing_ok=True)
    raise AssertionError("unreachable")


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
