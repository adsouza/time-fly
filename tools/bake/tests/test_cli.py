"""The bake CLI end to end on the committed tiny raw tree (tests/fixtures/raw_tiny)."""

import hashlib
import json
import struct
from pathlib import Path

import numpy as np
import pytest
import zstandard

from timefly_bake import cli, formats
from timefly_bake import log as bake_log
from timefly_bake.paleodem import PaleoDemFrame

FIXTURE = Path(__file__).parent / "fixtures" / "raw_tiny"
CUBE = 8


def read_tfrot(path):
    raw = Path(path).read_bytes()
    assert raw[:8] == b"TFROT002"
    n_plates, n_times, zlen = struct.unpack("<III", raw[8:20])
    body = zstandard.ZstdDecompressor().decompress(raw[20 : 20 + zlen], max_output_size=1 << 26)
    ids = np.frombuffer(body, "<u4", n_plates, 0)
    off = 4 * n_plates
    times = np.frombuffer(body, "<f8", n_times, off)
    off += 8 * n_times
    quats = np.frombuffer(body, "<f4", n_times * n_plates * 4, off).reshape(n_times, n_plates, 4)
    off += 16 * n_times * n_plates
    valid = np.frombuffer(body, "u1", n_times * n_plates, off).reshape(n_times, n_plates)
    assert off + n_times * n_plates == len(body)
    return ids, times, quats, valid


def read_tfplates(path):
    raw = Path(path).read_bytes()
    assert raw[:8] == b"TFPLAT01"
    cube_size, n_entries, zlen = struct.unpack("<III", raw[8:20])
    body = zstandard.ZstdDecompressor().decompress(raw[20 : 20 + zlen], max_output_size=1 << 26)
    cells = 6 * cube_size * cube_size
    offsets = np.frombuffer(body, "<u4", cells + 1, 0)
    entry = np.dtype([("plate_index", "<u4"), ("younger", "<f8"), ("older", "<f8")])
    entries = np.frombuffer(body, entry, n_entries, 4 * (cells + 1))
    return cube_size, offsets, entries


@pytest.fixture(scope="module")
def tiny_bake(tmp_path_factory):
    out = tmp_path_factory.mktemp("globe")
    try:
        assert cli.main(["--raw", str(FIXTURE), "--out", str(out), "--cube-size", str(CUBE)]) == 0
    finally:
        bake_log.configure(None)
    return out, json.loads((out / "manifest.json").read_text())


def test_cli_builds_manifest_from_tiny_inputs(tiny_bake):
    out, manifest = tiny_bake
    assert manifest["version"] == 2
    assert manifest["cube_size"] == CUBE
    frames = manifest["frames"]
    assert [f["age_myr"] for f in frames] == [0.0, 0.000076, 0.030, 540.0]
    assert [f["source"] for f in frames] == ["ice6g", "ice6g", "paleodem", "paleodem"]
    # Files are numbered by first occurrence; age 0 reuses the youngest ICE-6G_C image.
    for frame, slot in zip(frames, (0, 0, 1, 2), strict=True):
        digest = hashlib.sha256((out / frame["file"]).read_bytes()).hexdigest()
        assert frame["file"] == f"frames/f{slot:04d}.{digest}.tfgrid"
    assert len({f["file"] for f in frames}) == 3
    # PaleoDEMs carry their source reconstruction ages; ICE-6G_C frames carry none.
    assert [f.get("source_rotation_age_myr") for f in frames] == [None, None, 0.0, 540.0]
    for key, stem, ext in [("footprints", "plates", "tfplates"), ("rotations", "rotations", "tfrot")]:
        digest = hashlib.sha256((out / manifest[key]).read_bytes()).hexdigest()
        assert manifest[key] == f"{stem}.{digest}.{ext}"
    assert len(manifest["attribution"]) == 4
    text = " ".join(manifest["attribution"])
    for name in ("Scotese", "ICE-6G_C", "Argus", "Peltier", "Spratt"):
        assert name in text


def test_tiny_bake_references_are_valid_across_formats(tiny_bake):
    out, manifest = tiny_bake
    for f in manifest["frames"]:
        header, data = formats.read_tfgrid(out / f["file"])
        assert header["kind"] == "cube" and header["dtype"] == "f16"
        assert header["width"] == header["height"] == CUBE
        assert header["channels"] == (4 if f["source"] == "ice6g" else 2)
        assert np.all(np.isfinite(data.astype(np.float32)))
    # The ICE-6G_C frame has land, ocean, and ice on both land and water.
    _, ice = formats.read_tfgrid(out / manifest["frames"][0]["file"])
    ice = ice.astype(np.float32)
    assert ice[..., 1].max() == 1.0 and ice[..., 1].min() == 0.0
    assert np.any((ice[..., 2] > 0.5) & (ice[..., 1] < 0.5))  # floating ice
    assert np.any((ice[..., 2] > 0.5) & (ice[..., 1] > 0.5))  # grounded ice

    ids, times, quats, valid = read_tfrot(out / manifest["rotations"])
    assert ids.tolist() == [101, 202]
    assert times[0] == 0.0 and times[-1] == 540.0 and np.all(np.diff(times) > 0)
    for f in manifest["frames"]:
        if f["source"] == "paleodem":
            assert f["source_rotation_age_myr"] in times.tolist()
    assert valid.all()
    np.testing.assert_allclose(np.linalg.norm(quats, axis=-1), 1.0, atol=1e-6)

    cube_size, offsets, entries = read_tfplates(out / manifest["footprints"])
    assert cube_size == CUBE
    assert offsets[0] == 0 and offsets[-1] == len(entries) and len(entries) > 0
    assert entries["plate_index"].max() < len(ids)
    assert set(entries["plate_index"].tolist()) == {0, 1}
    assert np.all(entries["younger"] == 0.0) and np.all(entries["older"] == 540.0)


def test_tiny_bake_sea_level_curve_and_log(tiny_bake):
    out, manifest = tiny_bake
    curve = manifest["sea_level_curve"]
    assert curve[:3] == [[0.000076, 0.5], [0.001076, -1.5], [0.002076, -1.8]]
    assert curve[-1] == [0.031076, -82.5]
    assert [a for a, _ in curve] == sorted(a for a, _ in curve)
    log_text = (out / "bake.log").read_text()
    assert "image 3/3" in log_text
    assert "footprints" in log_text and "rotations" in log_text


def frame(timeline, source):
    return PaleoDemFrame(timeline, Path(f"m{source}.nc"), source, str(source), str(source))


def test_paleodem_step_keeps_bridge_and_endpoint():
    frames = [frame(0.030, 0.0)] + [frame(a + 0.5, float(a)) for a in range(5, 541, 5)]
    assert cli.select_paleodem_frames(frames, None) == frames
    kept = cli.select_paleodem_frames(frames, 10.0)
    assert [f.source_rotation_age_myr for f in kept] == [float(a) for a in range(0, 541, 10)]
    kept = cli.select_paleodem_frames(frames, 50.0)
    assert [f.source_rotation_age_myr for f in kept] == [0.0, *range(50, 501, 50), 540.0]
    assert kept[0].timeline_age_myr == 0.030
    with pytest.raises(ValueError):
        cli.select_paleodem_frames(frames, 0.0)


def test_cli_rejects_missing_raw_inputs(tmp_path):
    try:
        with pytest.raises(SystemExit) as exc:
            cli.main(["--raw", str(tmp_path / "nope"), "--out", str(tmp_path / "out")])
        assert exc.value.code != 0
    finally:
        bake_log.configure(None)
    assert not (tmp_path / "out" / "manifest.json").exists()


def test_rebake_publishes_only_complete_revisions(tmp_path, monkeypatch):
    original = cli.bake(FIXTURE, tmp_path, CUBE, footprint_dilate_cells=0)
    previous = (tmp_path / "manifest.json").read_bytes()
    changed = cli.bake(FIXTURE, tmp_path, CUBE, footprint_dilate_cells=2)
    assert changed["footprints"] != original["footprints"]
    assert (tmp_path / original["footprints"]).is_file()
    current = (tmp_path / "manifest.json").read_bytes()
    assert current != previous

    replace = Path.replace

    def fail_publish(src, dest):
        if dest == tmp_path / "manifest.json":
            raise OSError("publish failed")
        return replace(src, dest)

    monkeypatch.setattr(Path, "replace", fail_publish)
    with pytest.raises(OSError, match="publish failed"):
        cli.bake(FIXTURE, tmp_path, 4)
    assert (tmp_path / "manifest.json").read_bytes() == current
    assert all((tmp_path / f["file"]).is_file() for f in changed["frames"])
