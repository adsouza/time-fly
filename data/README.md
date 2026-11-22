# Data

Datasets baked by `tools/bake` (rows filled in as the bake tasks land).

| Dataset | Source | License | Notes |
|---|---|---|---|
| Paleo-DEM / topography | PALEOMAP PaleoDEMs, Scotese & Wright (2018); see [PaleoDEMs](#paleodems) | CC BY 4.0 | Task 6 (`timefly_bake.paleodem`); ages in `data/paleodem_ages.csv` |
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

## PaleoDEMs

The 1° netCDF PaleoDEMs linked from the
[EarthByte PaleoDEM resource](https://www.earthbyte.org/paleodem-resource-scotese-and-wright-2018/)
(Zenodo record [10.5281/zenodo.5460860](https://doi.org/10.5281/zenodo.5460860)), and the report
whose Table 1 gives each map's reconstruction age.

| File | URL | SHA-256 |
|---|---|---|
| `data/raw/paleodem/Scotese_Wright_2018_Maps_1-88_1degX1deg_PaleoDEMS_nc.zip` (9,302,291 bytes) | archive linked from the EarthByte page above (Zenodo 10.5281/zenodo.5460860) | `7014767a168c28d3762f55117ebafcd0f6b6b90029a495e2d14fd23f94abc16d` |
| `data/raw/report/Scotese_Wright2018_PALEOMAP_PaleoDEMs.pdf` (6,840,972 bytes) | https://www.earthbyte.org/webdav/ftp/Data_Collections/Scotese_Wright_2018_PaleoDEM/Scotese_Wright2018_PALEOMAP_PaleoDEMs.pdf | `8a5fca14f9860d148427a56cec241b4fc5b4108ff91bb867e8b68bde1392edac` |

- **Citation:** Scotese, C.R. & Wright, N. (2018). *PALEOMAP Paleodigital Elevation Models
  (PaleoDEMS) for the Phanerozoic.* PALEOMAP Project. https://doi.org/10.5281/zenodo.5460860
- **Licence:** CC BY 4.0 (`License.txt` in the archive, and the Zenodo record). Baked frames may
  ship with the game with the citation in the credits; the raw archive is not redistributed.
- **Unpacking:** `unzip Scotese_Wright_2018_Maps_1-88_1degX1deg_PaleoDEMS_nc.zip -d extracted` in
  `data/raw/paleodem`, then only parse the contents. The `.nc` files are in
  `extracted/Scotese_Wright_2018_Maps_1-88_1degX1deg_PaleoDEMS_nc_v2/`.

**Archive contents.** 109 `.nc` files (not 88: the map numbering has half steps such as 43.5 and
84.1 and skips 2–2.5 and 4), one per 5 Myr from 0 to 540 Ma. Also GPlates raster caches
(`*.gplates.cache`), `All_Maps.gpml` and `PaleoAtlasTimeIntervalsv22b copy.csv` (the report's
Appendix I with the same columns as Table 1); the bake ignores all three.

**netCDF schema** (identical in all 109 files, checked by script):

- NETCDF4, CF-1.5/COARDS, written by GMT 5.4.1 `grdsample -I1d` from finer grids.
- Variables `lon(lon)` float64, 361 values −180…180, and `lat(lat)` float64, 181 values −90…90
  ascending; both grid-registered at whole degrees with ~2e-11° float noise. `z(lat, lon)` float32,
  `_FillValue` NaN, no units attribute (the report gives metres). No NaNs in any file.
- The 180° column duplicates the −180° column exactly in every file; `load_paleodem` drops it, so
  grids load as 181 × 360 with longitudes −180…179.
- Pole rows are not single-valued: 97 of the 218 vary by more than 100 m (up to 13,880 m, Map60 south
  pole); Map01's south pole row is 0 m. Isolated spikes exist (Map88 has one 9,000 m cell at
  58° S, 163° E). These are source artefacts; the bake samples them as published.

**Age mapping.** `data/paleodem_ages.csv` has one reviewed row per file: `filename`, `map_number`,
`label` (Table 1 stratigraphic age text, verbatim), `timeline_age_myr`, `source_rotation_age_myr`
and `notes`. `timefly_bake.paleodem.list_paleodem_files` requires every `.nc` file in the directory
to have exactly one row, strictly ascending timeline ages, a present-day source first and the
540 Ma frame last.

- **Source rotation age** = Table 1 "Plate Tectonic Model Age", which the report's footnote 3
  defines as the age in the PALEOMAP Global Plate Model v2d3 to use for plate reconstructions:
  exactly 0, 5, 10, … 540 Ma.
- **Timeline age** = Table 1 stratigraphic age (Ogg et al. 2012), e.g. 66 Ma for the KT-boundary
  map reconstructed at 65 Ma. Two exceptions: the present-day map sits at 0.030 Myr, the PaleoDEM
  bridge frame of the globe timeline (plan Task 7), and the Cambrian/Precambrian map (541 Ma
  stratigraphic) sits at 540 Ma, the slider's oldest stop. 86 of 109 frames have a timeline age
  different from their reconstruction age.
- **What filename ages encode:** the plate model age for 107 files (e.g. `KT_Boundary_65Ma` for a
  66 Ma map, `540Ma` for a 541 Ma map), but the stratigraphic age for Map67.5 (`385.2Ma`, plate
  age 385) and Map68 (`390.5Ma`, plate age 390). The bake therefore never parses them. Each file's
  `history` attribute names the finer source grid it was resampled from; those names carry the
  stratigraphic ages (`…KT_Boundary_66Ma.nc`, `…541Ma.nc`), with older estimates for Map03 (4.0),
  Map13 (52.2), Map14 (55.8) and Map15 (60.6) where Table 1 now gives 4.47, 51.9, 56 and 61.
- **Table 1 vs the files:** Table 1 lists 116 maps from 0 to 540 Ma. The 7 without a file are
  rows 2, 2.1–2.4 (Pleistocene, plate age 0, drawn on the modern base map) and 2.5 and 4
  (Pliocene/latest Miocene, plate age 5). So no source lies between 30 ka and 4.47 Ma other than
  the present-day map. The PDF numbers the 315, 320 and 325 Ma maps 60, 61 and 61.5; the filenames
  and Appendix I CSV number them 59.5, 60 and 61. All ages agree.
- The Task 5 rotation export also samples 385.2 and 390.5 Ma; with this mapping the required
  source rotation ages are the 5 Myr series, all already on the 1 Myr grid.

Reference files:

- `reference/draft/bevy-hybrid-lod-architecture-v4.md`: architecture draft (v4).
- `reference/draft/terrain_cdlod.wesl`: draft CDLOD terrain shader.
- `reference/draft/voxel_surface_nets.wesl`: draft voxel surface nets shader.
- `data/culture/civilizations.csv`: source chronology of global human civilizations, copied unchanged.
