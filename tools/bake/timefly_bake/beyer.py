"""Past climate from Beyer, Krapp & Manica (2020), read by HTTP range requests.

The figshare file (``LateQuaternary_Environment.nc``, 1.33 GB, HDF5 inside) holds 72 time steps
of 0.5 degree bioclimatic variables. The bake needs two of them, BIO1 (annual mean temperature,
degrees C) and BIO12 (annual precipitation, mm/yr), about 38 MB stored, so ``RangeFile`` lets
h5py read just those chunks over HTTP and writes nothing to disk.

figshare's ndownloader answers every ranged GET with a redirect to a signed URL that lives about
ten seconds, so each request starts from the public URL and follows exactly one redirect.
Downloaded bytes are only ever parsed by h5py as data; nothing in them is executed.
"""

from __future__ import annotations

import collections
import contextlib
import hashlib
import io
import json
import logging
import re
import tempfile
import time
import urllib.error
import urllib.request
from pathlib import Path
from urllib.parse import urljoin, urlsplit

import h5py
import numpy as np

from timefly_bake import formats
from timefly_bake.log import traced

_log = logging.getLogger(__name__)

MAX_FETCH_BYTES = 100_000_000
MANIFEST_VERSION = 1
MANIFEST_NAME = "manifest.json"
CHANNELS = ["bio1_c", "bio12_mm"]
VARIABLES = ("BIO1", "BIO12")
CLIMATE_BLOCK = 64 << 10  # small blocks keep the HDF5 index reads cheap
ATTRIBUTION = (
    "Beyer, R.M., Krapp, M. & Manica, A. (2020). An empirical evaluation of bioclimatic and "
    "vegetation reconstructions for the last 120,000 years. Scientific Data 7, 236, "
    "doi:10.1038/s41597-020-0552-1; data: figshare doi:10.6084/m9.figshare.12293345. "
    "CC BY 4.0. Changes: BIO1 and BIO12 regridded to 1 degree."
)

_LOOPBACK = {"127.0.0.1", "localhost", "::1"}
_REDIRECTS = (301, 302, 303, 307, 308)
_LRU_BLOCKS = 64
_RUN_BLOCKS = 32  # most blocks one GET (and one read piece) may span
_ATTEMPTS = 3
_TIMEOUT_S = 60
_USER_AGENT = "timefly-bake/0.1"
_API = "https://api.figshare.com/v2/articles/{article}/files"
_AGE_TOL_KA = 1e-3
_F16_MAX = 60000.0


class FetchCapError(RuntimeError):
    """The range reader would pass its byte cap."""


class RangeError(RuntimeError):
    """The server's answer to a range request was not usable."""


class _NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, *args, **kwargs):
        return None


def _status(resp) -> int:
    return getattr(resp, "status", None) or resp.code


def _safe_url(url: str) -> bool:
    parts = urlsplit(url)
    return bool(parts.hostname) and (
        parts.scheme == "https" or (parts.scheme == "http" and parts.hostname in _LOOPBACK)
    )


class RangeFile(io.RawIOBase):
    """A read-only seekable file over ``url``, fetched by ranged GETs.

    Blocks of ``block`` bytes are cached (an LRU of at most 64 in memory). Missing blocks that
    one read needs and that sit side by side are fetched with a single GET, so a big read costs
    one request while small index reads cost one block each. Each GET starts at ``url`` and
    follows at most one redirect, which must go to https (or to loopback http for tests); a
    signed URL is never reused. Past ``cap_bytes`` fetched it raises ``FetchCapError``.
    """

    def __init__(self, url, size, block=1 << 20, cap_bytes=MAX_FETCH_BYTES, opener=None):
        if not _safe_url(url):
            raise RangeError(f"url must be https: {url!r}")
        super().__init__()
        self.url, self.size, self.block, self.cap_bytes = url, int(size), int(block), cap_bytes
        self.opener = opener or urllib.request.build_opener(_NoRedirect())
        self.pos = 0
        self.fetched = 0
        self.requests = 0
        self.retries = 0
        self.failure: Exception | None = None
        self._cache: collections.OrderedDict[int, bytes] = collections.OrderedDict()

    def readable(self):
        return True

    def seekable(self):
        return True

    def tell(self):
        return self.pos

    def seek(self, offset, whence=0):
        base = (0, self.pos, self.size)[whence]
        self.pos = max(0, base + offset)
        return self.pos

    def _open(self, url, headers):
        req = urllib.request.Request(url, headers=headers)
        try:
            return self.opener.open(req, timeout=_TIMEOUT_S)
        except urllib.error.HTTPError as err:
            if err.code in _REDIRECTS:
                return err
            raise

    def _get(self, lo, hi):
        expected = hi - lo + 1
        remaining = self.cap_bytes - self.fetched
        if expected > remaining:
            raise FetchCapError(f"range needs {expected} bytes, only {remaining} left under the cap")
        headers = {"Range": f"bytes={lo}-{hi}", "User-Agent": _USER_AGENT}
        resp = self._open(self.url, headers)
        if _status(resp) in _REDIRECTS:
            target = urljoin(self.url, resp.headers.get("Location") or "")
            resp.close()
            if not _safe_url(target):
                raise RangeError(f"redirect must go to https, not {urlsplit(target).scheme!r}")
            resp = self._open(target, headers)
            if _status(resp) in _REDIRECTS:
                resp.close()
                raise RangeError("more than one redirect")
        with resp:
            if _status(resp) != 206:
                raise RangeError(f"expected 206 for bytes {lo}-{hi}, got {_status(resp)}")
            match = re.fullmatch(r"bytes (\d+)-(\d+)/(\d+)", resp.headers.get("Content-Range", ""))
            if not match:
                raise RangeError(f"bad Content-Range {resp.headers.get('Content-Range')!r}")
            got_lo, got_hi, total = map(int, match.groups())
            if total != self.size:
                raise RangeError(
                    f"Content-Range total {total} differs from the expected {self.size}"
                )
            if (got_lo, got_hi) != (lo, hi):
                raise RangeError(f"asked for bytes {lo}-{hi}, got {got_lo}-{got_hi}")
            length = resp.headers.get("Content-Length")
            if length is not None and length != str(expected):
                raise RangeError(f"expected {expected} bytes, got Content-Length {length!r}")
            # Bound even a chunked response. Reserve the read budget first so a transport
            # failure after partial delivery cannot spend the same bytes again on retry.
            limit = min(expected + int(length is None), remaining)
            self.fetched += limit
            data = resp.read(limit)
            self.fetched -= limit - len(data)
            if len(data) != expected:
                raise RangeError(f"asked for bytes {lo}-{hi}, got {len(data)} bytes")
        return data

    def _fetch_run(self, first, last):
        lo, hi = first * self.block, min(self.size, (last + 1) * self.block) - 1
        for attempt in range(_ATTEMPTS):
            try:
                data = self._get(lo, hi)
                break
            except (RangeError, FetchCapError):
                raise
            except (urllib.error.URLError, OSError, TimeoutError) as err:
                if attempt == _ATTEMPTS - 1:
                    raise RangeError(
                        f"bytes {lo}-{hi} failed after {_ATTEMPTS} tries: {err}"
                    ) from err
                self.retries += 1
                time.sleep(0.5 * (attempt + 1))
        self.requests += 1
        for i in range(first, last + 1):
            self._cache[i] = data[(i - first) * self.block : (i - first + 1) * self.block]
            self._cache.move_to_end(i)
        while len(self._cache) > _LRU_BLOCKS:
            self._cache.popitem(last=False)

    def _ensure(self, first, last):
        run = None
        for i in range(first, last + 1):
            if i in self._cache:
                self._cache.move_to_end(i)
                if run:
                    self._fetch_run(*run)
                    run = None
            else:
                run = (run[0], i) if run else (i, i)
        if run:
            self._fetch_run(*run)

    def readinto(self, buf):
        try:
            want = min(len(buf), max(0, self.size - self.pos))
            done = 0
            while done < want:
                start = self.pos + done
                piece = min(want - done, _RUN_BLOCKS * self.block - start % self.block)
                first, last = start // self.block, (start + piece - 1) // self.block
                self._ensure(first, last)
                for i in range(first, last + 1):
                    blk = self._cache[i]
                    s = max(start, i * self.block) - i * self.block
                    e = min(start + piece, (i + 1) * self.block) - i * self.block
                    buf[done : done + e - s] = blk[s:e]
                    done += e - s
            self.pos += want
            return want
        except Exception as err:
            self.failure = err
            raise


@traced
@contextlib.contextmanager
def open_h5(rf: RangeFile):
    """Open ``rf`` with h5py; a failure inside the reader surfaces as itself, not as the
    ``OSError`` h5py wraps it in."""
    try:
        h5 = h5py.File(
            io.BufferedReader(rf, buffer_size=rf.block),
            "r",
            rdcc_nbytes=128 << 20,
            rdcc_nslots=10007,
        )
        try:
            yield h5
        finally:
            h5.close()
    except Exception as err:
        if rf.failure is not None and err is not rf.failure:
            raise rf.failure from err
        raise


def _get_json(url, opener):
    opener = opener or urllib.request.build_opener()
    req = urllib.request.Request(url, headers={"User-Agent": _USER_AGENT})
    with opener.open(req, timeout=_TIMEOUT_S) as resp:
        return json.loads(resp.read())


@traced
def figshare_file_meta(article, file_id, opener=None) -> dict:
    """``size`` and ``computed_md5`` of one file of a figshare article, from its files API."""
    listing = _get_json(_API.format(article=article), opener)
    for item in listing:
        if item.get("id") == file_id:
            return {
                "size": int(item["size"]),
                "computed_md5": str(item["computed_md5"]),
                "name": item.get("name"),
            }
    raise ValueError(f"figshare article {article} lists no file {file_id}")


def _find(h5, what, names):
    for key in h5.keys():
        if key.lower() in names:
            return key
    raise ValueError(
        f"no {what} variable (looked for {sorted(names)}); the file has {sorted(h5.keys())!r}"
    )


def _lattice(degrees_from_edge, what):
    """Index of each centre on the 0.5 degree global lattice, given its distance in degrees
    from the first edge (90 - lat going south, lon + 180 going east); centres sit at 0.25."""
    idx = np.asarray(degrees_from_edge, dtype=np.float64) / 0.5
    rows = np.round(idx - 0.5)
    if np.abs(idx - 0.5 - rows).max() > 1e-3:
        raise ValueError(f"{what} centres are not on the 0.5 degree lattice: {list(idx[:4])}")
    return rows.astype(np.int64)


def _clean(var, sel):
    data = np.asarray(var[...], dtype=np.float32)[sel]
    for attr in ("_FillValue", "missing_value"):
        if attr in var.attrs:
            data[data == np.float32(np.asarray(var.attrs[attr]).ravel()[0])] = np.nan
    data[~np.isfinite(data) | (np.abs(data) > 1e30)] = np.nan
    return data


@traced
def read_bio(h5, ages_ka):
    """BIO1 and BIO12 at ``ages_ka`` on the global 0.5 degree lattice, north first.

    Returns ``(ages_ka, bio1, bio12)`` with ages ascending (0 first) and the arrays
    ``(n_ages, 360, 720)`` float32, NaN wherever the file has no value. ``time`` is read as
    years since present, so ``age_ka = -time / 1000``.
    """
    tname = _find(h5, "time", {"time", "t"})
    lat_name = _find(h5, "latitude", {"latitude", "lat"})
    lon_name = _find(h5, "longitude", {"longitude", "lon"})
    var_names = {v: _find(h5, v, {v.lower()}) for v in VARIABLES}
    time = np.asarray(h5[tname][...], dtype=np.float64)
    lat = np.asarray(h5[lat_name][...], dtype=np.float64)
    lon = np.asarray(h5[lon_name][...], dtype=np.float64)
    file_ages = -time / 1000.0
    want = sorted(float(a) for a in ages_ka)
    picked = []
    for age in want:
        hits = np.flatnonzero(np.abs(file_ages - age) < _AGE_TOL_KA)
        if hits.size != 1:
            raise ValueError(
                f"age {age} ka is not in the file once; its ages (ka) are {sorted(file_ages.tolist())}"
            )
        picked.append(int(hits[0]))
    rows = _lattice(90.0 - lat, "latitude")
    cols = _lattice(((lon + 180.0) % 360.0 - 180.0) + 180.0, "longitude")
    if (
        len(set(rows)) != len(rows)
        or len(set(cols)) != len(cols)
        or rows.min() < 0
        or rows.max() > 359
    ):
        raise ValueError("latitude or longitude centres repeat or fall outside the globe")
    units = {}
    out = []
    for name in VARIABLES:
        var = h5[var_names[name]]
        if var.shape != (len(time), len(lat), len(lon)):
            raise ValueError(
                f"{var_names[name]} has shape {var.shape}, expected {(len(time), len(lat), len(lon))}"
            )
        units[name] = var.attrs.get("units", "?")
        small = _clean(var, picked)
        grid = np.full((len(picked), 360, 720), np.nan, dtype=np.float32)
        grid[:, rows[:, None], cols[None, :]] = small
        out.append(grid)
    _log.info(
        "beyer: %d of %d ages, lat %s..%s (%d rows), lon %s..%s (%d cols), units %s",
        len(picked),
        len(time),
        lat[0],
        lat[-1],
        len(lat),
        lon[0],
        lon[-1],
        len(lon),
        units,
    )
    return np.asarray(want, dtype=np.float64), out[0], out[1]


@traced
def regrid_to_1deg(a):
    """Mean of each 2 x 2 block of the last two axes, ignoring NaN (NaN only if all four are)."""
    a = np.asarray(a, dtype=np.float32)
    h, w = a.shape[-2] // 2, a.shape[-1] // 2
    blocks = a[..., : 2 * h, : 2 * w].reshape(*a.shape[:-2], h, 2, w, 2)
    count = np.sum(~np.isnan(blocks), axis=(-3, -1))
    total = np.nansum(blocks, axis=(-3, -1), dtype=np.float64)
    mean = total / np.maximum(count, 1)
    return np.where(count > 0, mean, np.nan).astype(np.float32)


@traced
def climate_ages_ka() -> list[float]:
    """The 35 baked ages: 0-21 ka by 1, then 22, 24, 26, then 30, 40, ... 120."""
    return [float(a) for a in [*range(22), 22, 24, 26, *range(30, 121, 10)]]


def _digest(array) -> str:
    return hashlib.sha256(np.ascontiguousarray(array, dtype="<f4").tobytes()).hexdigest()


def _publish(path: Path, blob: bytes) -> None:
    if path.is_file() and path.read_bytes() == blob:
        return
    with tempfile.TemporaryDirectory(dir=path.parent) as staging:
        tmp = Path(staging) / path.name
        tmp.write_bytes(blob)
        tmp.replace(path)


def _age_label(age: float) -> str:
    return f"{age:g}"


@traced
def bake_climate(entry, out, opener=None, record_pins=False) -> dict:
    """Bake the Beyer climate grids described by the ``range_read`` ``entry`` into ``out``.

    Checks the figshare API's size and md5 against the entry, reads BIO1 and BIO12 over HTTP
    ranges, checks each extracted array against the entry's ``variables`` SHA-256 pins (or, with
    ``record_pins``, records them in the returned manifest instead), regrids to 1 degree and
    writes one F16 ``(180, 360, 2)`` ``.tfgrid`` per age under a content-hash name, then
    ``manifest.json`` last. ``entry["ages_ka"]`` overrides the default ages (used for fixtures).
    """
    out = Path(out)
    started = time.perf_counter()
    meta = figshare_file_meta(entry["figshare_article"], entry["figshare_file"], opener)
    if meta["size"] != entry["bytes"]:
        raise ValueError(f"figshare size {meta['size']} differs from the pinned {entry['bytes']}")
    if meta["computed_md5"] != entry["md5"]:
        raise ValueError(
            f"figshare md5 {meta['computed_md5']} differs from the pinned {entry['md5']}"
        )
    ages = [float(a) for a in entry.get("ages_ka", climate_ages_ka())]
    rf = RangeFile(entry["url"], entry["bytes"], block=CLIMATE_BLOCK, opener=opener)
    with open_h5(rf) as h5:
        ages_ka, bio1, bio12 = read_bio(h5, ages)
    _log.info(
        "beyer fetch: %d bytes in %d requests (%d retries)", rf.fetched, rf.requests, rf.retries
    )

    digests = {"BIO1": _digest(bio1), "BIO12": _digest(bio12)}
    pinned = entry.get("variables", {})
    for name, digest in digests.items():
        if record_pins:
            _log.info("beyer pin %s sha256 %s", name, digest)
            continue
        want = pinned.get(name, {}).get("sha256")
        if want != digest:
            raise ValueError(f"extracted {name} sha256 {digest} does not match the pin {want}")

    low1, low12 = regrid_to_1deg(bio1), regrid_to_1deg(bio12)
    for name, grid in (("BIO1", low1), ("BIO12", low12)):
        if np.nanmax(np.abs(grid)) > _F16_MAX:
            raise ValueError(f"{name} does not fit in float16")
    out.mkdir(parents=True, exist_ok=True)
    files = []
    total = 0
    for i, age in enumerate(ages_ka.tolist()):
        grid = np.stack([low1[i], low12[i]], axis=-1).astype(np.float16)
        blob = formats.encode_tfgrid("latlon", grid)
        sha = hashlib.sha256(blob).hexdigest()
        name = f"bio_{_age_label(age)}ka.{sha}.tfgrid"
        _publish(out / name, blob)
        files.append({"age_ka": age, "file": name, "bytes": len(blob), "sha256": sha})
        total += len(blob)
    manifest = {
        "version": MANIFEST_VERSION,
        "grid": {"deg": 1.0, "rows": 180, "cols": 360, "channels": list(CHANNELS)},
        "ages_ka": [f["age_ka"] for f in files],
        "files": files,
        "source": {
            "figshare_file": entry["figshare_file"],
            "variables_sha256": digests,
        },
        "attribution": ATTRIBUTION,
    }
    _publish(out / MANIFEST_NAME, (json.dumps(manifest, indent=1) + "\n").encode())
    _log.info(
        "climate done in %.1f s: %d grids, %.2f MB; fetched %.1f MB in %d requests",
        time.perf_counter() - started,
        len(files),
        total / 1e6,
        rf.fetched / 1e6,
        rf.requests,
    )
    return manifest


@traced
def write_synthetic_beyer(path, ages_ka, rows=300, cols=720) -> None:
    """Write an h5py file with Beyer's layout and made-up fields, for tests and the fixture.

    Latitude runs 89.75 down, longitude from -179.75; ``time`` is years since present. BIO1 is
    the latitude minus ``0.25 * age`` (quantised, so it compresses), with an ocean band of NaN
    at 0.5 degree columns 100-199. BIO12 is wet (2500) within 10 degrees of the equator, dry
    (100) from 15 to 30 degrees and 800 elsewhere. Two more variables stand in for the others.
    """
    ages = np.asarray(sorted(float(a) for a in ages_ka), dtype=np.float64)[::-1]  # oldest first
    lat = 89.75 - 0.5 * np.arange(rows)
    lon = -179.75 + 0.5 * np.arange(cols)
    wet = np.where(
        np.abs(lat) < 10, 2500.0, np.where((np.abs(lat) >= 15) & (np.abs(lat) <= 30), 100.0, 800.0)
    )
    shape = (len(ages), rows, cols)
    ocean = np.zeros(cols, dtype=bool)
    ocean[100:200] = True
    with h5py.File(path, "w") as f:
        f.create_dataset("time", data=-ages * 1000.0)
        f.create_dataset("latitude", data=lat)
        f.create_dataset("longitude", data=lon)
        chunks = (min(24, shape[0]), min(100, rows), min(240, cols))
        fields = {
            "BIO1": lat[None, :, None] - 0.25 * ages[:, None, None],
            "BIO12": wet[None, :, None] + 0.0 * ages[:, None, None],
            "BIO5": lat[None, :, None] * 0.5 - 0.25 * ages[:, None, None],
            "BIO6": lat[None, :, None] * 0.25 - 0.25 * ages[:, None, None],
        }
        for name, base in fields.items():
            data = np.broadcast_to(base, shape).astype(np.float32).copy()
            data[:, :, ocean] = np.nan
            ds = f.create_dataset(
                name, data=data, chunks=chunks, compression="gzip", compression_opts=1
            )
            ds.attrs["units"] = "degC" if name != "BIO12" else "mm/year"
