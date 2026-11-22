"""The game's reference-year convention, shared with ``timefly_core::time`` (plan Task 1).

The game counts years before 2026 CE. Sources dated in years BP count from 1950 CE, so their ages
gain 76 years on import: 0 BP is 76 game years ago and 21,000 BP is 21,076.
"""

from __future__ import annotations

import numpy as np

from timefly_bake.log import traced

PRESENT_YEAR_CE = 2026
BP_REFERENCE_YEAR_CE = 1950
BP_OFFSET_YEARS = PRESENT_YEAR_CE - BP_REFERENCE_YEAR_CE


@traced
def myr_from_1950_bp(years_bp):
    """Timeline age in Myr of a source age in years BP (1950): ``(years + 76) / 1e6``.

    One division, so ages such as 0.000076 or 0.026076 are the nearest doubles to those decimals
    and compare equal to literals. Scalars give a float, arrays an array.
    """
    out = (np.asarray(years_bp, dtype=np.float64) + BP_OFFSET_YEARS) / 1e6
    return float(out) if out.ndim == 0 else out
