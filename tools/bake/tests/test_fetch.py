"""``timefly-fetch``: pinned raw downloads, served from a local ``http.server`` thread."""

import hashlib
import json
import logging
import re
import threading
from functools import partial
from http.server import SimpleHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

import pytest

from timefly_bake import fetch

REPO = Path(__file__).resolve().parents[3]
PAYLOAD = b"time fly fixture payload\n" * 64


class _CountingHandler(SimpleHTTPRequestHandler):
    def __init__(self, *args, requests, **kwargs):
        self._requests = requests
        super().__init__(*args, **kwargs)

    def do_GET(self):  # noqa: N802 (http.server API)
        self._requests.append(self.path)
        super().do_GET()

    def log_message(self, format, *args):  # keep pytest output quiet
        pass


@pytest.fixture
def server(tmp_path):
    """Serves ``tmp_path/www`` on 127.0.0.1; yields (base URL, list of requested paths)."""
    www = tmp_path / "www"
    (www / "sub").mkdir(parents=True)
    (www / "sub" / "payload.bin").write_bytes(PAYLOAD)
    requests: list[str] = []
    handler = partial(_CountingHandler, directory=str(www), requests=requests)
    httpd = ThreadingHTTPServer(("127.0.0.1", 0), handler)
    thread = threading.Thread(target=httpd.serve_forever, kwargs={"poll_interval": 0.02}, daemon=True)
    thread.start()
    try:
        yield f"http://127.0.0.1:{httpd.server_address[1]}", requests
    finally:
        httpd.shutdown()
        httpd.server_close()
        thread.join()


def write_sources(tmp_path, entries):
    path = tmp_path / "sources.json"
    path.write_text(json.dumps({"files": entries}))
    return path


def entry(base, rel="sub/payload.bin", data=PAYLOAD, path="dir/payload.bin"):
    return {
        "url": f"{base}/{rel}",
        "path": path,
        "sha256": hashlib.sha256(data).hexdigest(),
        "bytes": len(data),
        "licence": "CC BY 4.0",
    }


def run(sources, raw):
    return fetch.main(["--sources", str(sources), "--raw", str(raw)])


def leftovers(raw):
    return sorted(p.relative_to(raw).as_posix() for p in raw.rglob("*") if p.is_file())


def test_fresh_download(tmp_path, server):
    base, requests = server
    raw = tmp_path / "raw"
    sources = write_sources(tmp_path, [entry(base)])
    assert run(sources, raw) == 0
    assert (raw / "dir" / "payload.bin").read_bytes() == PAYLOAD
    assert requests == ["/sub/payload.bin"]
    assert leftovers(raw) == ["dir/payload.bin"]


def test_skips_file_whose_checksum_matches(tmp_path, server, caplog):
    caplog.set_level(logging.INFO, logger="timefly_bake")
    base, requests = server
    raw = tmp_path / "raw"
    (raw / "dir").mkdir(parents=True)
    (raw / "dir" / "payload.bin").write_bytes(PAYLOAD)
    sources = write_sources(tmp_path, [entry(base)])
    assert run(sources, raw) == 0
    assert requests == []
    assert "dir/payload.bin" in caplog.text


def test_checksum_mismatch_leaves_nothing_and_fails(tmp_path, server, caplog):
    base, requests = server
    raw = tmp_path / "raw"
    bad = entry(base)
    bad["sha256"] = hashlib.sha256(b"something else").hexdigest()
    sources = write_sources(tmp_path, [bad])
    assert run(sources, raw) != 0
    assert requests == ["/sub/payload.bin"]
    assert leftovers(raw) == []
    assert "dir/payload.bin" in caplog.text


def test_size_mismatch_leaves_nothing_and_fails(tmp_path, server):
    base, _ = server
    raw = tmp_path / "raw"
    bad = entry(base)
    bad["bytes"] = len(PAYLOAD) - 1
    sources = write_sources(tmp_path, [bad])
    assert run(sources, raw) != 0
    assert leftovers(raw) == []


def test_missing_url_fails_naming_the_file(tmp_path, server, caplog):
    base, _ = server
    raw = tmp_path / "raw"
    sources = write_sources(tmp_path, [entry(base, rel="sub/absent.bin")])
    assert run(sources, raw) != 0
    assert leftovers(raw) == []
    assert "dir/payload.bin" in caplog.text


@pytest.mark.parametrize(
    "bad_path", ["../escape.bin", "dir/../../escape.bin", "/etc/escape.bin", "", "a\\b.bin"]
)
def test_path_escaping_raw_is_rejected(tmp_path, server, bad_path):
    base, requests = server
    raw = tmp_path / "raw"
    sources = write_sources(tmp_path, [entry(base), entry(base, path=bad_path)])
    assert run(sources, raw) != 0
    assert requests == []  # validated before any download
    assert not (tmp_path / "escape.bin").exists()
    assert not raw.exists() or leftovers(raw) == []


@pytest.mark.parametrize(
    ("field", "value"),
    [
        ("sha256", "abc"),
        ("bytes", 0),
        ("bytes", "12"),
        ("licence", ""),
        ("url", "ftp://example.org/x"),
        ("url", "http://example.org/x"),  # plain http only for loopback test servers
    ],
)
def test_malformed_entries_are_rejected(tmp_path, server, field, value):
    base, requests = server
    bad = entry(base)
    bad[field] = value
    sources = write_sources(tmp_path, [bad])
    assert run(sources, tmp_path / "raw") != 0
    assert requests == []


def test_duplicate_paths_are_rejected(tmp_path, server):
    base, requests = server
    sources = write_sources(tmp_path, [entry(base), entry(base)])
    assert run(sources, tmp_path / "raw") != 0
    assert requests == []


def test_repo_sources_agree_with_data_readme():
    """``data/sources.json`` is valid and matches the URLs and checksums in ``data/README.md``."""
    sources = fetch.load_sources(REPO / "data" / "sources.json")
    readme = (REPO / "data" / "README.md").read_text()
    paths = {s.path for s in sources}
    ice = sorted(p for p in paths if p.startswith("ice6g/"))
    assert ice == sorted(f"ice6g/I6_C.VM5a_1deg.{k}.nc.gz" for k in range(27))
    for required in (
        "paleodem/Scotese_Wright_2018_Maps_1-88_1degX1deg_PaleoDEMS_nc.zip",
        "plates/Rotations.zip",
        "plates/StaticPolygons.zip",
        "sealevel/spratt2016-noaa.txt",
    ):
        assert required in paths
    for s in sources:
        assert s.url.startswith("https://")
        if s.path.startswith("ice6g/"):
            k = re.fullmatch(r"ice6g/I6_C\.VM5a_1deg\.(\d+)\.nc\.gz", s.path).group(1)
            assert s.url == (
                "https://www.atmosp.physics.utoronto.ca/~peltier/datasets/Ice6G_C_VM5a/"
                f"I6_C.VM5a_1deg.{k}.nc.gz"
            )
            if k in ("0", "26"):
                assert s.sha256 in readme
        else:
            assert s.url in readme, s.path
            assert s.sha256 in readme, s.path
            assert f"data/raw/{s.path}" in readme, s.path
    assert sum(s.bytes for s in sources if s.path.startswith("ice6g/")) == 15_651_848
