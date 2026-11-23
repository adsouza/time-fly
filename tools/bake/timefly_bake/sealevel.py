"""Global sea level for the Quaternary bridge: Spratt & Lisiecki (2016), long PC1 stack.

The NOAA NCEI template file (``spratt2016-noaa.txt``) is tab-separated with ``#`` comment lines
and one header row. Ages are calendar ka BP (1 ka steps, 0-798 ka); ``SeaLev_longPC1`` is metres
relative to modern sea level (negative = lower sea). Consumers apply anomalies as
``effective_elev_m = elev_m - anomaly_m``.
"""

from __future__ import annotations

import logging
import math
from pathlib import Path

import numpy as np

from timefly_bake import ages
from timefly_bake.log import traced

AGE_COLUMN = "age_calkaBP"
SEA_LEVEL_COLUMN = "SeaLev_longPC1"

_log = logging.getLogger(__name__)


@traced
def load_spratt2016(path):
    """``(age_myr, anomaly_m)`` float64 arrays in strictly ascending timeline age.

    Timeline ages are the file's ka BP (1950) plus the 76-year game offset. Rejects a missing
    column, a missing (NaN) long-PC1 value and duplicate ages rather than guessing.
    """
    path = Path(path)
    header = None
    rows: list[tuple[float, float]] = []
    for lineno, line in enumerate(path.read_text(encoding="utf-8").splitlines(), start=1):
        line = line.strip("\r\n")
        if not line.strip() or line.startswith("#"):
            continue
        fields = [f.strip() for f in line.split("\t")]
        if header is None:
            header = fields
            missing = [c for c in (AGE_COLUMN, SEA_LEVEL_COLUMN) if c not in header]
            if missing:
                raise ValueError(f"{path.name}: missing column(s) {missing}")
            i_age, i_sea = header.index(AGE_COLUMN), header.index(SEA_LEVEL_COLUMN)
            continue
        try:
            ka, sea = float(fields[i_age]), float(fields[i_sea])
        except (IndexError, ValueError):
            raise ValueError(f"{path.name}:{lineno}: unreadable data line") from None
        if not (math.isfinite(ka) and math.isfinite(sea)):
            raise ValueError(f"{path.name}:{lineno}: missing value at {fields[i_age]} ka")
        rows.append((ka, sea))
    if not rows:
        raise ValueError(f"{path.name}: no data lines")

    rows.sort()
    ka = np.array([r[0] for r in rows], dtype=np.float64)
    sea = np.array([r[1] for r in rows], dtype=np.float64)
    dup = np.flatnonzero(np.diff(ka) == 0)
    if dup.size:
        raise ValueError(f"{path.name}: duplicate age {ka[dup[0]]:g} ka")
    age_myr = ages.myr_from_1950_bp(ka * 1000.0)
    _log.info(
        "sea level: %d knots, %g-%g ka BP, %.1f..%.1f m",
        ka.size,
        ka[0],
        ka[-1],
        float(sea.min()),
        float(sea.max()),
    )
    return age_myr, sea
