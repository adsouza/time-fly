# Data

Datasets baked by `tools/bake` (rows filled in as the bake tasks land).

| Dataset | Source | License | Notes |
|---|---|---|---|
| Paleo-DEM / topography | TBD | TBD | Task 5 |
| Plate reconstruction (rotations, polygons) | PALEOMAP plate model, Scotese & Wright (2018); see [Plate model](#plate-model) | CC BY 4.0 | Task 5 (`timefly_bake.plates`) |
| Climate / paleo-environment | TBD | TBD | Task 7 |
| Cultural chronology | `data/culture/civilizations.csv` | TBD | Milestone 5 import |

## Plate model

The rotation and static-polygon files that accompany the Scotese & Wright (2018) PaleoDEMs, taken
from the GPlates plate-model registry entry `scotese_and_wright2018`, which declares version
`10.5281/zenodo.5460860`: the same Zenodo record as the PaleoDEMs.

| File | URL | SHA-256 |
|---|---|---|
| `data/raw/plates/Rotations.zip` | https://repo.gplates.org/webdav/pmm/scotese_and_wright2018/Rotations.zip | `73a92b7734b6e7471a15a116703886b9d030f6da7f00172e593abbdf300f2031` |
| `data/raw/plates/StaticPolygons.zip` | https://repo.gplates.org/webdav/pmm/scotese_and_wright2018/StaticPolygons.zip | `27dbc00cbd0bff840198706d306571eecfff62781fe4dfd49d396d73463f77d2` |

The checksums are also in `data/raw/SHA256SUMS`. The archives hold one file each:

- `Rotations/Scotese_Wright_PlateModel.rot` (79,521 bytes, PLATES4 format). Header: "PALEOMAP Plate
  Model m15g60_v2d3, for use with Plate Polygons ContOCeanPolyv10u_v2d3, by CR Scotese 02/01/2016".
  317 rotation sequences over 257 moving plate IDs, with ages from −250 Ma (future) to 1100 Ma.
- `StaticPolygons/Scotese_Wright_ContinentalPolygons.gpml` (2,596,672 bytes). 240 features, 245
  polygons, 150 plate IDs. Continental polygons only: ocean floor has no plate and shows the ocean
  shell.

Unpack into a fresh directory and only parse the contents:
`unzip Rotations.zip -d extracted && unzip StaticPolygons.zip -d extracted` in `data/raw/plates`.
Nothing under `data/raw/` is committed.

- **Citation:** Scotese, C.R. & Wright, N. (2018). *PALEOMAP Paleodigital Elevation Models
  (PaleoDEMS) for the Phanerozoic.* Zenodo. https://doi.org/10.5281/zenodo.5460860
- **Licence and redistribution:** CC BY 4.0 (the Zenodo record). Derived bake outputs may ship with
  the game provided the credits carry the citation above; the raw archives are not redistributed.
- **Reconstruction anchor:** plate 0, the PALEOMAP paleomagnetic frame in which the PaleoDEMs are
  published. Plate 001 ("Hot Spot to PMAG") is identity relative to 0 throughout. Checked on the
  real files (smoke check, Task 5): polygons reconstructed with anchor 0 contain 100%, 93%, 91% and
  95% of PaleoDEM land at 0, 65, 250 and 540 Ma, against 56%, 53%, 65% and 63% for the same
  polygons spun 30° about the pole.
- **What the bake exports:** total rotations (`RotationModel.get_rotation` without `from_time`)
  relative to plate 0 at the 1 Myr samples from 0 to 540 Ma, the PaleoDEM reconstruction ages
  (including 385.2 and 390.5 Ma), every sequence start and end in that range, and the midpoint of
  any span where a plate is missing between two valid samples. Missing circuits are exported as
  invalid, never as identity. Ages outside 0–540 Ma are dropped.
- **Quirks in this release:** plate 198 (Precordillera terrane) has a constant non-identity
  rotation at 0 Ma, so present-day orientation uses the table's age-0 rotation rather than
  identity. Plate 617 has polygons but no rotation sequence, so it is always invalid. Plates 601,
  602, 604, 606, 610 and 614 have no circuit between 305.0 and 305.01 Ma. No cube texel centre at
  sizes 8 or 64 lies in two polygons (or in one plate's polygons over disjoint intervals), so in
  practice each footprint cell holds at most one entry; overlaps in the era frame come from plate
  motion.

Reference files:

- `reference/draft/bevy-hybrid-lod-architecture-v4.md`: architecture draft (v4).
- `reference/draft/terrain_cdlod.wesl`: draft CDLOD terrain shader.
- `reference/draft/voxel_surface_nets.wesl`: draft voxel surface nets shader.
- `data/culture/civilizations.csv`: source chronology of global human civilizations, copied unchanged.
