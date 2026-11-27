"""packaging/stage_climate.py: copies the climate bake into a Pages site or a Linux package.

The packaged builds must carry every baked Beyer grid, or the game falls back to the zonal
climate for the whole session (``ClimateIndex::fail``). The stage script is the packaging check:
it fails on a missing manifest, on fewer grids than the bake writes, and on a file that does not
match its manifest entry, and it leaves ``bake.log`` behind.
"""

from __future__ import annotations

import hashlib
import json
import subprocess
import sys
from pathlib import Path

from timefly_bake import beyer

REPO = Path(__file__).resolve().parents[3]
SCRIPT = REPO / "packaging" / "stage_climate.py"
TINY = REPO / "crates" / "timefly" / "tests" / "assets" / "data" / "climate"


def stage(source, dest, *extra):
    return subprocess.run(
        [sys.executable, "-I", str(SCRIPT), str(source), str(dest), *extra],
        capture_output=True,
        text=True,
    )


def fake_bake(root: Path, ages) -> Path:
    """A climate folder shaped like `timefly-bake climate`'s output, with tiny made-up grids."""
    root.mkdir(parents=True)
    files = []
    for age in ages:
        blob = f"grid for {age} ka".encode()
        sha = hashlib.sha256(blob).hexdigest()
        name = f"bio_{age:g}ka.{sha}.tfgrid"
        (root / name).write_bytes(blob)
        files.append({"age_ka": age, "file": name, "bytes": len(blob), "sha256": sha})
    manifest = {
        "version": 1,
        "grid": {"deg": 1.0, "rows": 180, "cols": 360, "channels": ["bio1_c", "bio12_mm"]},
        "ages_ka": [f["age_ka"] for f in files],
        "files": files,
    }
    (root / "manifest.json").write_text(json.dumps(manifest, indent=1) + "\n")
    (root / "bake.log").write_text("DEBUG bake log\n")
    return root


def test_stages_every_shipped_grid_and_leaves_the_bake_log(tmp_path):
    ages = beyer.climate_ages_ka()
    assert len(ages) == 35
    source = fake_bake(tmp_path / "climate", ages)
    out = stage(source, tmp_path / "site" / "climate")
    assert out.returncode == 0, out.stderr
    assert out.stdout.strip() == "35"
    staged = sorted(p.name for p in (tmp_path / "site" / "climate").iterdir())
    assert len(staged) == 36
    assert "manifest.json" in staged
    assert "bake.log" not in staged
    assert (tmp_path / "site" / "climate" / "manifest.json").read_bytes() == (
        source / "manifest.json"
    ).read_bytes()


def test_fails_without_a_climate_bake(tmp_path):
    out = stage(tmp_path / "missing", tmp_path / "site" / "climate")
    assert out.returncode != 0
    assert "climate" in out.stderr
    assert not (tmp_path / "site" / "climate" / "manifest.json").exists()


def test_fails_when_the_bake_has_fewer_grids_than_shipped(tmp_path):
    source = fake_bake(tmp_path / "climate", beyer.climate_ages_ka()[:-1])
    out = stage(source, tmp_path / "site" / "climate")
    assert out.returncode != 0
    assert "34" in out.stderr and "35" in out.stderr


def test_fails_when_a_grid_differs_from_its_manifest_entry(tmp_path):
    source = fake_bake(tmp_path / "climate", beyer.climate_ages_ka())
    victim = sorted(source.glob("bio_21ka.*.tfgrid"))[0]
    blob = victim.read_bytes()
    victim.write_bytes(blob[:-1] + b"?")  # same length, different bytes
    out = stage(source, tmp_path / "site" / "climate")
    assert out.returncode != 0
    assert "sha256" in out.stderr


def test_fails_when_a_listed_grid_is_missing(tmp_path):
    source = fake_bake(tmp_path / "climate", beyer.climate_ages_ka())
    sorted(source.glob("bio_0ka.*.tfgrid"))[0].unlink()
    out = stage(source, tmp_path / "site" / "climate")
    assert out.returncode != 0
    assert "bio_0ka" in out.stderr


def test_stages_the_committed_tiny_bake_with_its_own_count(tmp_path):
    out = stage(TINY, tmp_path / "climate", "--expect-grids", "4")
    assert out.returncode == 0, out.stderr
    assert out.stdout.strip() == "4"
    assert len(list((tmp_path / "climate").glob("bio_*.tfgrid"))) == 4
