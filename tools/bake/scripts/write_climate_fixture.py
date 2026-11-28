"""Regenerate the tiny climate fixture read by crates/timefly/tests (no network).

A synthetic file in Beyer's layout (wet tropics, a dry band at 15-30 degrees, an ocean band of
NaN) is served by an in-process opener and baked by ``timefly_bake.beyer.bake_climate`` at ages
0, 10, 21 and 26 ka, so the fixture is made by the same code as the shipped grids.
"""

import hashlib
import io
import json
import shutil
import tempfile
from pathlib import Path
from urllib.parse import urlsplit

from timefly_bake import beyer

OUT = Path(__file__).resolve().parents[3] / "crates/timefly/tests/assets/data/climate"
AGES_KA = [0.0, 10.0, 21.0, 26.0]
URL = "https://ndownloader.figshare.com/files/22659026"


class _Response(io.BytesIO):
    def __init__(self, body: bytes, status: int, headers: dict):
        super().__init__(body)
        self.status, self.headers = status, headers


class _LocalOpener:
    """Answers the figshare API and ranged GETs of the public URL from a local file."""

    def __init__(self, path: Path):
        self.path = path
        self.blob = path.read_bytes()

    def open(self, req, timeout=None):
        if urlsplit(req.full_url).hostname == "api.figshare.com":
            body = json.dumps(
                [
                    {
                        "id": 22659026,
                        "size": len(self.blob),
                        "computed_md5": hashlib.md5(self.blob).hexdigest(),
                    }
                ]
            )
            return _Response(body.encode(), 200, {})
        lo, hi = req.get_header("Range").removeprefix("bytes=").split("-")
        lo, hi = int(lo), min(int(hi), len(self.blob) - 1)
        headers = {"Content-Range": f"bytes {lo}-{hi}/{len(self.blob)}"}
        return _Response(self.blob[lo : hi + 1], 206, headers)


def main() -> None:
    if OUT.exists():
        shutil.rmtree(OUT)
    with tempfile.TemporaryDirectory() as tmp:
        src = Path(tmp) / "synthetic_beyer.nc"
        beyer.write_synthetic_beyer(src, [*AGES_KA, 5.0, 15.0])
        opener = _LocalOpener(src)
        entry = {
            "url": URL,
            "figshare_article": 12293345,
            "figshare_file": 22659026,
            "bytes": len(opener.blob),
            "md5": hashlib.md5(opener.blob).hexdigest(),
            "ages_ka": AGES_KA,
        }
        beyer.bake_climate(entry, OUT, opener=opener, record_pins=True)


if __name__ == "__main__":
    main()
