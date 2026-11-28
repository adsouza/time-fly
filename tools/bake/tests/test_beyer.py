"""Beyer et al. 2020 past climate: range-read client, extraction, regridding and the bake.

The tests serve a synthetic h5py file with Beyer's layout (four variables of shape
(72, 300, 720), chunks (24, 100, 240), gzip) from a local HTTP server that mimics figshare's
ndownloader: every ranged GET of the public path answers 302 to ``/signed?token=N``, and each
token is good for exactly one request.
"""

from __future__ import annotations

import hashlib
import json
import threading
import io
import urllib.error
import urllib.request
from concurrent.futures import ThreadPoolExecutor
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from urllib.parse import parse_qs, urlsplit

import h5py
import numpy as np
import pytest

from timefly_bake import beyer, fetch, formats

REPO = Path(__file__).resolve().parents[3]
ARTICLE, FILE_ID = 12293345, 22659026
AGES = [float(a) for a in [*range(66), 70, 80, 90, 100, 110, 120]]  # 72 ages, like Beyer's 72


def _md5(path: Path) -> str:
    h = hashlib.md5()
    with path.open("rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


class Figshare:
    """A local stand-in for figshare: the ndownloader redirect, signed URLs and the files API."""

    def __init__(self, path: Path):
        self.path = path
        self.size = path.stat().st_size
        self.md5 = _md5(path)
        self.api_md5 = self.md5
        self.total_override: int | None = None
        self.issued: list[int] = []
        self.used: list[int] = []
        self.lock = threading.Lock()
        outer = self

        class Handler(BaseHTTPRequestHandler):
            protocol_version = "HTTP/1.1"

            def log_message(self, *args):
                pass

            def _send(self, code, headers, body=b""):
                self.send_response(code)
                for k, v in headers.items():
                    self.send_header(k, v)
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                self.wfile.write(body)

            def do_GET(self):
                parts = urlsplit(self.path)
                if parts.path == f"/v2/articles/{ARTICLE}/files":
                    body = json.dumps(
                        [
                            {"id": 1, "name": "other.txt", "size": 5, "computed_md5": "0" * 32},
                            {
                                "id": FILE_ID,
                                "name": "LateQuaternary_Environment.nc",
                                "size": outer.size,
                                "computed_md5": outer.api_md5,
                            },
                        ]
                    ).encode()
                    return self._send(200, {"Content-Type": "application/json"}, body)
                if parts.path == f"/files/{FILE_ID}":
                    with outer.lock:
                        token = len(outer.issued)
                        outer.issued.append(token)
                    return self._send(302, {"Location": f"/signed?token={token}"})
                if parts.path == "/signed":
                    token = int(parse_qs(parts.query)["token"][0])
                    with outer.lock:
                        reused = token in outer.used
                        outer.used.append(token)
                    if reused:
                        return self._send(403, {}, b"token already used")
                    lo, hi = self.headers["Range"].removeprefix("bytes=").split("-")
                    lo, hi = int(lo), min(int(hi), outer.size - 1)
                    with outer.path.open("rb") as f:
                        f.seek(lo)
                        body = f.read(hi - lo + 1)
                    total = outer.total_override or outer.size
                    return self._send(206, {"Content-Range": f"bytes {lo}-{hi}/{total}"}, body)
                return self._send(404, {})

        self.httpd = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self.base = f"http://127.0.0.1:{self.httpd.server_address[1]}"
        threading.Thread(target=self.httpd.serve_forever, daemon=True).start()

    @property
    def url(self) -> str:
        return f"{self.base}/files/{FILE_ID}"

    def reset(self):
        self.issued.clear()
        self.used.clear()
        self.total_override = None
        self.api_md5 = self.md5

    def close(self):
        self.httpd.shutdown()
        self.httpd.server_close()

    def entry(self, **extra) -> dict:
        return {
            "name": "beyer2020",
            "url": self.url,
            "figshare_article": ARTICLE,
            "figshare_file": FILE_ID,
            "file": "LateQuaternary_Environment.nc",
            "bytes": self.size,
            "md5": self.md5,
            "variables": {},
            "licence": "CC BY 4.0",
        } | extra

    def opener(self):
        """Sends the figshare API host to this server; every other URL is left alone."""
        return _Rewrite(self.base)


class _Rewrite:
    def __init__(self, base: str):
        self.base = base
        self.inner = urllib.request.build_opener()

    def open(self, req, timeout=None):
        parts = urlsplit(req.full_url)
        if parts.hostname == "api.figshare.com":
            req = urllib.request.Request(self.base + parts.path, headers=dict(req.header_items()))
        return self.inner.open(req, timeout=timeout)


@pytest.fixture(scope="module")
def beyer_server(tmp_path_factory):
    path = tmp_path_factory.mktemp("beyer") / "LateQuaternary_Environment.nc"
    beyer.write_synthetic_beyer(path, AGES)
    server = Figshare(path)
    yield server
    server.close()


@pytest.fixture
def server(beyer_server):
    beyer_server.reset()
    return beyer_server


def test_range_reader_follows_a_fresh_redirect_per_block(server):
    rf = beyer.RangeFile(server.url, server.size, block=1 << 16)
    head = rf.read(200_000)  # blocks 0-3, missing and adjacent: one ranged GET
    rf.seek(server.size // 2)
    rf.read(10)  # a block far away: a second GET, with its own redirect
    assert len(head) == 200_000
    assert rf.requests == len(server.issued) == 2
    assert rf.fetched == 5 * (1 << 16)
    rf.seek(100)
    assert rf.read(10) == head[100:110]  # cached: no new request
    assert rf.requests == 2
    assert sorted(server.used) == server.used == [0, 1]  # each token used once


def test_range_reader_fetches_only_what_h5py_reads(server):
    rf = beyer.RangeFile(server.url, server.size, block=1 << 16)
    with beyer.open_h5(rf) as h5:
        beyer.read_bio(h5, [0.0, 21.0])
    assert rf.fetched < 0.6 * server.size, (rf.fetched, server.size)
    assert rf.requests == len(server.issued)


def test_reader_refuses_past_the_fetch_cap(server):
    rf = beyer.RangeFile(server.url, server.size, cap_bytes=1 << 20)
    with pytest.raises(beyer.FetchCapError):
        with beyer.open_h5(rf) as h5:
            beyer.read_bio(h5, [0.0])
    assert beyer.MAX_FETCH_BYTES == 100_000_000


def test_reader_checks_the_total_size(server):
    server.total_override = server.size + 7
    rf = beyer.RangeFile(server.url, server.size, block=1 << 16)
    with pytest.raises(beyer.RangeError) as err:
        rf.read(10)
    assert str(server.size) in str(err.value) and str(server.size + 7) in str(err.value)


def test_range_reader_refuses_a_redirect_to_plain_http_elsewhere(server):
    class Evil(_Rewrite):
        def open(self, req, timeout=None):
            raise urllib.error.HTTPError(
                req.full_url, 302, "Found", {"Location": "http://example.com/x"}, io.BytesIO(b"")
            )

    rf = beyer.RangeFile(server.url, server.size, opener=Evil(server.base))
    with pytest.raises(beyer.RangeError, match="https"):
        rf.read(10)


def test_regrid_takes_the_nanmean_of_each_block():
    a = np.array([[1, np.nan], [3, 5]], dtype=np.float32)
    assert beyer.regrid_to_1deg(a).tolist() == [[3.0]]
    out = beyer.regrid_to_1deg(np.full((2, 4, 4), np.nan, dtype=np.float32))
    assert out.shape == (2, 2, 2) and np.isnan(out).all()
    two = np.array([[1, 2, np.nan, np.nan], [3, 4, np.nan, 8]], dtype=np.float32)
    got = beyer.regrid_to_1deg(two)
    assert got[0, 0] == 2.5 and got[0, 1] == 8.0


def _tiny_h5(lat, lon, time_years, names=("Time", "Lat", "Lon", "bio1", "bio12"), fill=None):
    f = h5py.File("tiny.h5", "w", driver="core", backing_store=False)
    t, la, lo, b1, b12 = names
    f[t], f[la], f[lo] = np.asarray(time_years), np.asarray(lat), np.asarray(lon)
    shape = (len(time_years), len(lat), len(lon))
    f[b1] = np.arange(np.prod(shape), dtype=np.float32).reshape(shape)
    f[b12] = np.ones(shape, dtype=np.float32)
    if fill is not None:
        f[b12].attrs["_FillValue"] = np.float32(fill)
        f[b12][0, 0, 0] = np.float32(fill)
    return f


def test_read_bio_places_any_orientation_on_the_global_lattice():
    lat = np.array([-0.25, 0.25, 0.75])  # south first
    lon = np.array([0.25, 179.75, 180.25, 359.75])  # 0..360
    with _tiny_h5(lat, lon, [-3000, -1000, 0]) as h5:
        ages, bio1, bio12 = beyer.read_bio(h5, [0.0, 3.0])
    assert ages.tolist() == [0.0, 3.0]
    assert bio1.shape == (2, 360, 720) and bio1.dtype == np.float32
    # age 0 is time index 2; lat 0.75 is row 178, lat -0.25 is row 180; lon 0.25 is col 360.
    base = 2 * 12
    assert bio1[0, 178, 360] == base + 2 * 4 + 0
    assert bio1[0, 180, 360] == base + 0 * 4 + 0
    assert bio1[0, 178, 0] == base + 2 * 4 + 2  # lon 180.25 -> -179.75
    assert bio1[0, 178, 719] == base + 2 * 4 + 1
    assert bio1[0, 178, 359] == base + 2 * 4 + 3
    assert np.isnan(bio1[0, 0, 0]) and np.isnan(bio12[0, 0, 0])
    assert bio12[0, 178, 360] == 1.0


def test_read_bio_masks_the_fill_value_and_names_what_it_found():
    lat, lon = np.array([0.25]), np.array([0.25, 0.75])
    with _tiny_h5(lat, lon, [0], fill=-9999.0) as h5:
        _, _, bio12 = beyer.read_bio(h5, [0.0])
        assert np.isnan(bio12[0, 179, 360]) and bio12[0, 179, 361] == 1.0
    with _tiny_h5(lat, lon, [0], names=("t", "y", "x", "bio1", "bio12")) as h5:
        with pytest.raises(ValueError, match="latitude") as err:
            beyer.read_bio(h5, [0.0])
        assert "'y'" in str(err.value) and "'x'" in str(err.value)
    with _tiny_h5(lat, lon, [0]) as h5:
        with pytest.raises(ValueError, match="2.5"):
            beyer.read_bio(h5, [2.5])


def test_climate_ages_are_the_thirty_five_the_game_uses():
    ages = beyer.climate_ages_ka()
    assert len(ages) == 35 and ages[:3] == [0.0, 1.0, 2.0] and ages[-1] == 120.0
    assert ages[21:26] == [21.0, 22.0, 24.0, 26.0, 30.0]
    assert ages[-10:] == [float(a) for a in range(30, 121, 10)]


@pytest.fixture(scope="module")
def baked(beyer_server, tmp_path_factory):
    beyer_server.reset()
    out = tmp_path_factory.mktemp("climate")
    manifest = beyer.bake_climate(
        beyer_server.entry(), out, opener=beyer_server.opener(), record_pins=True
    )
    return out, manifest


def test_bake_writes_one_grid_per_selected_age(baked, beyer_server):
    out, manifest = baked
    assert manifest["version"] == 1
    assert manifest["grid"] == {
        "deg": 1.0,
        "rows": 180,
        "cols": 360,
        "channels": ["bio1_c", "bio12_mm"],
    }
    assert manifest["ages_ka"] == beyer.climate_ages_ka()
    assert [f["age_ka"] for f in manifest["files"]] == manifest["ages_ka"]
    assert json.loads((out / "manifest.json").read_text()) == manifest
    assert len(manifest["files"]) == 35
    assert manifest["source"]["figshare_file"] == FILE_ID
    assert set(manifest["source"]["variables_sha256"]) == {"BIO1", "BIO12"}
    assert "CC BY 4.0" in manifest["attribution"]
    for entry in manifest["files"]:
        path = out / entry["file"]
        blob = path.read_bytes()
        assert len(blob) == entry["bytes"]
        assert hashlib.sha256(blob).hexdigest() == entry["sha256"]
        assert entry["sha256"] in entry["file"]
        header, data = formats.read_tfgrid(path)
        assert (header["kind"], header["dtype"], header["channels"]) == ("latlon", "f16", 2)
        assert data.shape == (180, 360, 2) and data.dtype == np.float16
    names = {p.name for p in out.glob("bio_*.tfgrid")}
    assert len(names) == 35 and names == {f["file"] for f in manifest["files"]}
    assert (out / manifest["files"][8]["file"]).name.startswith("bio_8ka.")
    _, g0 = formats.read_tfgrid(out / manifest["files"][0]["file"])
    # The synthetic BIO1 is the latitude, so 1-degree row 0 (89.5 N) is 89.5 on land columns.
    assert float(g0[0, 0, 0]) == 89.5
    assert float(g0[1, 0, 0]) == 88.5
    assert np.isnan(g0[0, 60, 0])  # the synthetic ocean band (0.5-degree columns 100..199)
    assert np.isnan(g0[179, 0, 0])  # south of Beyer's 59.75 S edge has no data
    assert float(g0[120, 0, 0]) == -30.5  # 1-degree row 120 is centred on 30.5 S


def test_extracted_arrays_are_pinned(baked, beyer_server, tmp_path):
    _, manifest = baked
    pins = manifest["source"]["variables_sha256"]
    good = {name: {"sha256": digest} for name, digest in pins.items()}
    again = beyer.bake_climate(
        beyer_server.entry(variables=good), tmp_path / "ok", opener=beyer_server.opener()
    )
    assert again["files"] == manifest["files"]
    bad = {"BIO1": good["BIO1"], "BIO12": {"sha256": "0" * 64}}
    with pytest.raises(ValueError, match="BIO12"):
        beyer.bake_climate(
            beyer_server.entry(variables=bad), tmp_path / "bad", opener=beyer_server.opener()
        )
    assert not (tmp_path / "bad" / "manifest.json").exists()
    with pytest.raises(ValueError, match="BIO1"):
        beyer.bake_climate(
            beyer_server.entry(), tmp_path / "unpinned", opener=beyer_server.opener()
        )


def test_rebaking_changes_no_file(baked, beyer_server):
    out, manifest = baked
    before = {p.name: p.stat().st_mtime_ns for p in out.iterdir()}
    beyer.bake_climate(beyer_server.entry(), out, opener=beyer_server.opener(), record_pins=True)
    assert {p.name: p.stat().st_mtime_ns for p in out.iterdir()} == before


def test_md5_and_size_come_from_the_figshare_api(server, tmp_path):
    meta = beyer.figshare_file_meta(ARTICLE, FILE_ID, opener=server.opener())
    assert meta["size"] == server.size and meta["computed_md5"] == server.md5
    server.api_md5 = "f" * 32
    with pytest.raises(ValueError, match="md5"):
        beyer.bake_climate(server.entry(), tmp_path, opener=server.opener(), record_pins=True)
    server.api_md5 = server.md5
    with pytest.raises(ValueError, match="size"):
        beyer.bake_climate(
            server.entry(bytes=server.size + 1), tmp_path, opener=server.opener(), record_pins=True
        )
    with pytest.raises(ValueError, match="999"):
        beyer.figshare_file_meta(ARTICLE, 999, opener=server.opener())


def test_repo_sources_pin_beyer():
    doc = json.loads((REPO / "data" / "sources.json").read_text())
    (entry,) = doc["range_read"]
    assert entry["name"] == "beyer2020"
    assert entry["url"] == "https://ndownloader.figshare.com/files/22659026"
    assert (entry["figshare_article"], entry["figshare_file"]) == (12293345, 22659026)
    assert entry["file"] == "LateQuaternary_Environment.nc"
    assert entry["bytes"] == 1_333_236_636
    assert entry["md5"] == "d23abb7ce8671433ea36a2cae814ebab"
    assert "CC BY 4.0" in entry["licence"]
    assert set(entry["variables"]) == {"BIO1", "BIO12"}
    for var in entry["variables"].values():
        assert len(var["sha256"]) == 64 and int(var["sha256"], 16) >= 0
    # timefly-fetch reads only "files", so it never downloads the 1.3 GB file.
    paths = {s.path for s in fetch.load_sources(REPO / "data" / "sources.json")}
    assert entry["file"] not in paths and not any("Late" in p for p in paths)


def test_climate_subcommand_passes_the_pinned_entry_and_flags(tmp_path, monkeypatch, capsys):
    from timefly_bake import cli

    seen = {}

    def fake(entry, out, opener=None, record_pins=False):
        seen.update(entry=entry, out=out, record_pins=record_pins)
        return {"source": {"variables_sha256": {"BIO1": "a" * 64, "BIO12": "b" * 64}}}

    monkeypatch.setattr(beyer, "bake_climate", fake)
    sources = tmp_path / "sources.json"
    sources.write_text(json.dumps({"files": [], "range_read": [{"name": "beyer2020", "bytes": 5}]}))
    assert (
        cli.main(
            ["climate", "--sources", str(sources), "--out", str(tmp_path / "o"), "--record-pins"]
        )
        == 0
    )
    assert seen["entry"] == {"name": "beyer2020", "bytes": 5} and seen["record_pins"] is True
    assert json.loads(capsys.readouterr().out) == {"BIO1": "a" * 64, "BIO12": "b" * 64}
    assert cli.main(["climate", "--sources", str(sources), "--out", str(tmp_path / "o")]) == 0
    assert seen["record_pins"] is False
    sources.write_text(json.dumps({"files": []}))
    assert cli.main(["climate", "--sources", str(sources), "--out", str(tmp_path / "o")]) == 1


@pytest.mark.parametrize("case", ["endpoints", "oversize", "overbudget", "retry"])
def test_range_requests_bound_response_and_retry_bytes(monkeypatch, case):
    opened = []
    reads = []

    class Response(io.BytesIO):
        status = 206
        headers = {"Content-Range": "bytes 0-15/16" if case == "endpoints" else "bytes 0-7/16"}

        def read(self, size=-1):
            reads.append(size)
            assert 0 < size <= 9
            if case == "retry":
                raise OSError("connection lost while reading")
            return super().read(size)

    class Opener:
        def open(self, req, timeout):
            opened.append(req)
            return Response(b"x" * 16)

    cap = {"overbudget": 7, "retry": 10}.get(case, 32)
    rf = beyer.RangeFile("https://example.test/data", 16, block=8, cap_bytes=cap, opener=Opener())
    monkeypatch.setattr(beyer.time, "sleep", lambda _: None)
    error = beyer.FetchCapError if case in ("overbudget", "retry") else beyer.RangeError
    with pytest.raises(error):
        rf.read(1)
    assert len(opened) == (0 if case == "overbudget" else 1)
    assert reads == ([9] if case in ("retry", "oversize") else [])
    assert rf.fetched <= cap


def test_concurrent_bakes_publish_complete_manifests(beyer_server, tmp_path, monkeypatch):
    # Both writers stage their manifests before either rename can finish.
    barrier = threading.Barrier(2)
    replace = Path.replace

    def simultaneous_replace(path, target):
        if target.name == "manifest.json":
            barrier.wait(timeout=10)
        return replace(path, target)

    monkeypatch.setattr(Path, "replace", simultaneous_replace)

    def bake(age):
        return beyer.bake_climate(
            beyer_server.entry(ages_ka=[age]),
            tmp_path,
            opener=beyer_server.opener(),
            record_pins=True,
        )

    with ThreadPoolExecutor(max_workers=2) as pool:
        manifests = list(pool.map(bake, [0, 21]))
    assert json.loads((tmp_path / "manifest.json").read_bytes()) in manifests
    for manifest in manifests:
        (entry,) = manifest["files"]
        assert (
            hashlib.sha256((tmp_path / entry["file"]).read_bytes()).hexdigest() == entry["sha256"]
        )
        _, grid = formats.read_tfgrid(tmp_path / entry["file"])
        assert grid.shape == (180, 360, 2)
        assert grid[0, 0, 0] == 89.5 - 0.25 * entry["age_ka"]
    assert len(list(tmp_path.iterdir())) == 3
