"""Regenerate the committed cross-language fixtures read by crates/timefly_core/tests."""

from pathlib import Path

import numpy as np

from timefly_bake import formats

OUT = Path(__file__).resolve().parents[3] / "crates/timefly_core/tests/fixtures"


def main() -> None:
    OUT.mkdir(parents=True, exist_ok=True)

    cube = np.zeros((6, 2, 2, 2), dtype=np.float16)
    for f in range(6):
        for r in range(2):
            for c in range(2):
                for ch in range(2):
                    cube[f, r, c, ch] = f * 100 + r * 10 + c + ch * 0.5
    formats.write_tfgrid(OUT / "cube2_rg16f.tfgrid", "cube", cube)

    quats = np.zeros((2, 3, 4), dtype=np.float32)
    quats[:, :, 3] = 1.0
    quats[1, 2] = (0, 0.7071068, 0, 0.7071068)
    valid = np.ones((2, 3), dtype=np.uint8)
    valid[1, 1] = 0
    formats.write_tfrot(OUT / "rot_small.tfrot", [101, 202, 303], [0.0, 1.0], quats, valid)

    # 2x2 cube: cell 0 empty, cell 1 one membership, cell 7 two memberships, rest empty.
    offsets = np.zeros(25, dtype=np.uint32)
    offsets[2:] = 1
    offsets[8:] = 3
    entries = [(1, 0.0, 50.0), (0, 100.0, 200.0), (2, 0.0, 540.0)]
    formats.write_tfplates(OUT / "footprints_small.tfplates", 2, offsets, entries)


if __name__ == "__main__":
    main()
