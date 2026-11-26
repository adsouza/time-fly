# Data

Datasets baked by `tools/bake` (rows filled in as the bake tasks land).

| Dataset | Source | License | Notes |
|---|---|---|---|
| Paleo-DEM / topography | PALEOMAP PaleoDEMs, Scotese & Wright (2018); see [PaleoDEMs](#paleodems) | CC BY 4.0 | Task 6 (`timefly_bake.paleodem`); ages in `data/paleodem_ages.csv` |
| Plate reconstruction (rotations, polygons) | PALEOMAP plate model, Scotese & Wright (2018); see [Plate model](#plate-model) | CC BY 4.0 | Task 5 (`timefly_bake.plates`) |
| Ice sheets, recent topography and coastlines (0–26 ka) | ICE-6G_C (VM5a) 1° files, W.R. Peltier; see [ICE-6G_C](#ice-6g_c-vm5a) | No licence stated; cite two papers (file attribute) | Task 7 (`timefly_bake.ice6g`) |
| Global sea level (0–798 ka) | Spratt & Lisiecki (2016) via NOAA NCEI; see [Sea level](#sea-level) | NOAA NCEI public data; cite publication and dataset DOI | Task 7 (`timefly_bake.sealevel`) |
| Terrain and bathymetry (region worlds) | ETOPO 2022 60″ surface, NOAA NCEI; see [Region tiles](#region-tiles-etopo-2022-and-köppen-geiger) | NOAA NCEI public data; cite doi:10.25921/fd45-gt74 | M2 Task 1 (`timefly_bake.etopo`, `tiles`) |
| Climate classes (region worlds) | Köppen-Geiger 1991–2020, Beck et al. (2023); see [Region tiles](#region-tiles-etopo-2022-and-köppen-geiger) | CC BY 4.0 | M2 Task 1 (`timefly_bake.koppen`) |
| Past climate, 0–120 ka (rivers) | Beyer, Krapp & Manica (2020), figshare 12293345; see [Past climate](#past-climate-beyer-et-al-2020) | CC BY 4.0 | Rivers and lakes Task 1 (`timefly_bake.beyer`) |
| Present-day lakes (rivers, recent eras) | Natural Earth 1:10m lakes 5.0.0; see [Present-day lake map](#present-day-lake-map-natural-earth) | Public domain | Rivers and lakes Task 8b (`timefly_bake.lakemap`) |
| Today's major river courses (rivers, recent eras) | Natural Earth 1:10m rivers + lake centerlines 5.1.2; see [Today's river courses](#todays-river-courses-natural-earth) | Public domain | Rivers and lakes Task 11b (`timefly_bake.rivers_today`) |
| Cultural chronology | `data/culture/civilizations.csv` | TBD | Milestone 5 import |

## Pinned downloads (`data/sources.json`)

`data/sources.json` lists every raw file the bake reads, with its URL, path under `data/raw/`,
SHA-256, size in bytes and licence: the PaleoDEM archive, the two plate-model archives, the 27
ICE-6G_C slices, the sea-level file, ETOPO 2022, the Köppen-Geiger archive, the Natural Earth lakes and the Natural Earth rivers (35 files). The URLs and checksums are
the ones in the tables below (`tools/bake/tests/test_fetch.py` checks that they agree). The
PaleoDEM report PDF is reference reading for the age table, not a bake input, so it is not
listed and CI never downloads it.

`cd tools/bake && uv run timefly-fetch --sources ../../data/sources.json --raw ../../data/raw`
skips files that already match, downloads the rest to a temporary name, checks size and
SHA-256, then renames; any mismatch deletes the download and fails naming the file. Certificate
verification is never disabled. The ICE-6G_C host sends only its leaf certificate, without the
Sectigo Public Server Authentication CA OV R36 intermediate, which OpenSSL (Python, Linux CI)
cannot complete on its own (macOS curl can). For exactly that failure ("unable to get local
issuer certificate") the fetcher requires the configured host and matching Sectigo issuer URL.
It refuses redirects, downloads that one intermediate (at most 16 KB, must be a CA,
not self-signed, and the leaf's issuer), and retries with the system roots plus it.
Partial chains are disallowed, so the chain must still end at a system root (here
Sectigo Public Server Authentication Root R46). Hostname checks and the
SHA-256 pins are unchanged (owner decision, 2026-11-22). CI
(`.github/workflows/linux-package.yml`) runs it with `data/raw/` cached under the hash of
`sources.json`.

**Redistribution in the Linux packages (Task 11, checked 2026-11-22).** The `.deb` and Flatpak
ship only the baked globe (`crates/timefly/assets/data/globe`, without `bake.log`), never the
raw files, with every citation in `/usr/share/doc/time-fly/copyright` and the in-game credits:

- PaleoDEMs and plate model: CC BY 4.0 allows redistribution of adapted material with
  attribution, a licence link and a note of changes; `credits.txt` carries all three.
- ICE-6G_C: no licence stated; the project owner approved distributing the baked derivatives
  with citation and credit on 2026-11-22 (see [ICE-6G_C](#ice-6g_c-vm5a)).
- Spratt & Lisiecki sea level: NOAA NCEI public data, redistributable with the requested
  citation of the publication and dataset DOI.

No recorded licence forbids shipping the baked derivatives.

## Plate model

The rotation and static-polygon files that accompany the Scotese & Wright (2018) PaleoDEMs, taken
from the GPlates plate-model registry entry `scotese_and_wright2018`, which declares version
`10.5281/zenodo.5460860`: the same Zenodo record as the PaleoDEMs.

| File | URL | SHA-256 |
|---|---|---|
| `data/raw/plates/Rotations.zip` | https://repo.gplates.org/webdav/pmm/scotese_and_wright2018/Rotations.zip | `73a92b7734b6e7471a15a116703886b9d030f6da7f00172e593abbdf300f2031` |
| `data/raw/plates/StaticPolygons.zip` | https://repo.gplates.org/webdav/pmm/scotese_and_wright2018/StaticPolygons.zip | `27dbc00cbd0bff840198706d306571eecfff62781fe4dfd49d396d73463f77d2` |

The same pins are in `data/sources.json`. The archives hold one file each:

- `Rotations/Scotese_Wright_PlateModel.rot` (79,521 bytes, PLATES4 format). Header: "PALEOMAP Plate
  Model m15g60_v2d3, for use with Plate Polygons ContOCeanPolyv10u_v2d3, by CR Scotese 02/01/2016".
  317 rotation sequences over 257 moving plate IDs, with ages from −250 Ma (future) to 1100 Ma.
- `StaticPolygons/Scotese_Wright_ContinentalPolygons.gpml` (2,596,672 bytes). 240 features, 245
  polygons, 150 plate IDs. Continental polygons only: ocean floor has no plate and shows the ocean
  shell.

Nothing needs unpacking: `timefly-bake` reads the archives where they are and copies only the
members it needs (by base name, size-checked) into a temporary directory. To look inside by hand,
unzip into a scratch directory outside `data/raw/` and only parse the contents. Nothing under
`data/raw/` is committed.

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
- **Footprint dilation:** after polygon assignment, every unowned cell within K cells (8-connected,
  across cube faces) of an owned cell joins the nearest plate by breadth-first distance (ties to
  the lowest plate ID) with that cell's interval list (`--footprint-dilate-cells`, default 2 at
  cube size 128, scaled with the cube size, 0 at the cube-8 test fixture). This closes
  quantisation cracks between plates; dilated cells over water still render as ocean.
- **Coverage mask (PaleoDEM channel G):** 1 where some plate with a valid rotation at the frame's
  source reconstruction age has a footprint cell, valid at that age, under the texel (the
  renderer's "covered" rule, dilated footprints included), else 0. The ocean shell shows the
  frame's own land where G is 0, which fills gaps that open between drifting polygons exactly at
  frame ages; between frames that land morphs rather than drifts.

## PaleoDEMs

The 1° netCDF PaleoDEMs linked from the
[EarthByte PaleoDEM resource](https://www.earthbyte.org/paleodem-resource-scotese-and-wright-2018/)
(Zenodo record [10.5281/zenodo.5460860](https://doi.org/10.5281/zenodo.5460860)), and the report
whose Table 1 gives each map's reconstruction age.

| File | URL | SHA-256 |
|---|---|---|
| `data/raw/paleodem/Scotese_Wright_2018_Maps_1-88_1degX1deg_PaleoDEMS_nc.zip` (9,302,291 bytes) | https://www.earthbyte.org/webdav/ftp/Data_Collections/Scotese_Wright_2018_PaleoDEM/Scotese_Wright_2018_Maps_1-88_1degX1deg_PaleoDEMS_nc.zip | `7014767a168c28d3762f55117ebafcd0f6b6b90029a495e2d14fd23f94abc16d` |
| `data/raw/report/Scotese_Wright2018_PALEOMAP_PaleoDEMs.pdf` (6,840,972 bytes) | https://www.earthbyte.org/webdav/ftp/Data_Collections/Scotese_Wright_2018_PaleoDEM/Scotese_Wright2018_PALEOMAP_PaleoDEMs.pdf | `8a5fca14f9860d148427a56cec241b4fc5b4108ff91bb867e8b68bde1392edac` |

- **Citation:** Scotese, C.R. & Wright, N. (2018). *PALEOMAP Paleodigital Elevation Models
  (PaleoDEMS) for the Phanerozoic.* PALEOMAP Project. https://doi.org/10.5281/zenodo.5460860
- **Licence:** CC BY 4.0 (`License.txt` in the archive, and the Zenodo record). Baked frames may
  ship with the game with the citation in the credits; the raw archive is not redistributed.
- **Unpacking:** not needed; `timefly-bake` copies the `.nc` members (archive folder
  `Scotese_Wright_2018_Maps_1-88_1degX1deg_PaleoDEMS_nc_v2/`) into a temporary directory by base
  name and only parses them.

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

## ICE-6G_C (VM5a)

The author's 1° netCDF slices of the ICE-6G_C (VM5a) glacial isostatic adjustment model, one per
integer ka from 0 to 26 ka BP (the owner chose these over the PMIP4 10′ series: 15.7 MB against
~1 GB, and a 128-texel cube face is ~0.7° per texel anyway).

| Files | URL pattern | SHA-256 |
|---|---|---|
| `data/raw/ice6g/I6_C.VM5a_1deg.<k>.nc.gz`, k = 0…26 (27 files, 15,651,848 bytes) | `https://www.atmosp.physics.utoronto.ca/~peltier/datasets/Ice6G_C_VM5a/I6_C.VM5a_1deg.<k>.nc.gz` | one per file in `data/sources.json` (the source publishes none); e.g. k = 0 `ca6262d23cdae07e3ed286e8b3c6cabcc4327db63caec70fa4096811bb0f6f88`, k = 26 `f219b296032f0850772c3a26b90b0072a140ac307e4e0a0ebe5937ecaf401b95` |

- **Citation** (the files' `Acknowledgement` attribute asks for both): Argus, D.F., Peltier, W.R.,
  Drummond, R. & Moore, A.W. (2014). The Antarctica component of postglacial rebound model
  ICE-6G_C (VM5a) based on GPS positioning, exposure age dating of ice thicknesses, and relative
  sea level histories. *Geophys. J. Int.* 198, 537–563. https://doi.org/10.1093/gji/ggu140 —
  Peltier, W.R., Argus, D.F. & Drummond, R. (2015). Space geodesy constrains ice age terminal
  deglaciation: the global ICE-6G_C (VM5a) model. *J. Geophys. Res. Solid Earth* 120, 450–487.
  https://doi.org/10.1002/2014JB011176
- **Licence/terms:** none stated in the files or found for the download directory; the only stated
  condition is the citation request above. Baked frames ship with both citations in the credits;
  the raw files are not redistributed. The project owner approved distributing the baked
  derivatives on these terms (citation and credit only) on 2026-11-22. (The author's index page could not be fetched to check for
  further terms: its TLS certificate chain does not verify.)
- **Unpacking:** not needed; `timefly_bake.ice6g.load_ice6g` inflates each `.nc.gz` in memory and
  only parses it.

**netCDF schema** (identical in all 27 files, checked by script, Task 7): NETCDF3 classic, global
attributes `Model` "ICE=ICE-6G_C, Viscosity=VM5a", `Date` "November 14,2014", `Author`,
`Acknowledgement`, `PAPER1`, `PAPER2`. Dimensions `lat` 180 × `lon` 360; cell centres `lat`
−89.5…89.5 ascending, `lon` 0.5…359.5 (`modulo` 360), float32, identical in every file. Variables
`(lat, lon)` float32, `_FillValue` 1e20, no fill values present:

| Variable | Units | Meaning | Used |
|---|---|---|---|
| `Topo` | m | "Topography at time=k Ka (point-value altitude)": signed, bathymetry negative, relative to that slice's sea level; grounded ice surface included (Antarctica ~2,760 m at 89.5° S) | R channel |
| `sftlf` | % | land area fraction, 0–100 (fractional on coasts) | G channel, ÷100 |
| `sftgif` | % | ice area fraction, 0–100, grounded and floating (Ross Ice Shelf at 0 ka: `Topo` −642 m, `sftlf` 0, `sftgif` 100) | B channel, ÷100 |
| `orog`, `stgit`, `Topo_Diff` | m | surface altitude over land, ice thickness, `Topo` minus present | not used |

Every variable carries `time` = `'k.00 KBP'`; the loader checks it against the file number. No BP
reference year is stated, so the bake assumes 1950 CE and adds 76 years (slice k sits at
`(1000·k + 76) / 1e6` Myr). The 0 ka file is smaller (235 KB vs ~590 KB gzipped) only because its
`Topo_Diff` is all zeros; it holds the same variables. Sanity checks on the real files: at 21 ka
Bering Strait (65.5° N, 168.5° W) is land at +55 m (−38 m today), Hudson Bay lies under the ice sheet (surface ~2.7 km, `sftgif` 100%),
and the North Sea floor is exposed. Masks and `Topo` agree closely but not exactly
(at 0 ka, 557 cells have `sftlf` ≥ 50% with `Topo` < 0 and 596 the reverse), which is why the
renderer takes land/water from `sftlf`, not from the sign of `Topo`.

## Sea level

| File | URL | SHA-256 |
|---|---|---|
| `data/raw/sealevel/spratt2016-noaa.txt` (51,519 bytes) | https://www.ncei.noaa.gov/pub/data/paleo/contributions_by_author/spratt2016/spratt2016-noaa.txt | `02f56f02f709b70ac70fe5e403326272e19b42bf34ee9b1bba3b8322e1375dc1` |

- **Citation:** Spratt, R.M. & Lisiecki, L.E. (2016). A Late Pleistocene sea level stack.
  *Climate of the Past* 12, 1079–1092. https://doi.org/10.5194/cp-12-1079-2016. Dataset: NOAA
  NCEI, https://doi.org/10.25921/rd66-5820 (landing page
  https://www.ncei.noaa.gov/access/paleo-search/study/19982).
- **Licence/terms:** NOAA NCEI paleoclimate data; the file asks users to cite the publication,
  landing page, dataset DOI and access date. Not redistributed raw; the baked curve ships with the
  citation.
- **Format:** NOAA template v4, `#` comment lines, one tab-separated header row, CRLF. 799 rows,
  `age_calkaBP` 0…798 in 1 ka steps (calendar ka BP; BP = 1950 assumed, +76 years on import).
  The bake uses `SeaLev_longPC1` (metres above modern sea level, 0–798 ka; never missing) and
  ignores the short (0–430 ka) stack and the error columns.
- **Values:** −130.0 to +19.0 m. The stack is not zeroed at present: it reads +8.96 m at 0 ka and
  0 m at 5 ka. The renderer applies the curve only from the PaleoDEM bridge frame (0.030 Myr,
  where it is about −102 m) toward older ages, so the young positive values are never used.

## Baked globe assets

`cd tools/bake && uv run timefly-bake --raw ../../data/raw --out ../../crates/timefly/assets/data/globe`
reads the archives above in place and writes the generated assets to the git-ignored globe directory.

New bakes include the SHA-256 of each payload in its filename. The manifest selects the
active assets and is replaced after all of them have been written. Superseded payloads
remain available to readers using an earlier manifest. Packaging copies only referenced
assets. To reclaim space, remove the old output directory when it is no longer in use.

| Output | Count | Size (bytes) |
|---|---|---|
| `frames/f0000–f0026.<sha256>.tfgrid` ICE-6G_C, `(6,128,128,4)` f16, zstd 19 | 27 images, 28 frames (age 0 reuses 0 ka) | 6,294,972 |
| `frames/f0027–f0135.<sha256>.tfgrid` PaleoDEM, `(6,128,128,2)` f16 | 109 images and frames, 0.030–540 Myr | 9,978,180 |
| `rotations.<sha256>.tfrot` | 149 plates × 551 times, 99.32% valid (plate 617 never) | 744,577 |
| `plates.<sha256>.tfplates` | 35,795 of 98,304 cells covered, 35,797 entries | 45,880 |
| `manifest.json` | 137 frames, 799 sea-level knots, 4 attribution lines | 38,264 |
| **Total** (plus `bake.log`, 460 KB, not shipped) | | **17,101,873** |

Measured on 2026-11-22 (Apple M4, 8.2 s wall). **Compressed transfer estimate:** the payloads are
already zstd level 19, so HTTP compression gains almost nothing: `tar | gzip -9` is 17,096,111
bytes and `brotli -q 11` 16,988,218 bytes, i.e. ~17.0 MB, inside the ~20 MB target. If it is ever
exceeded, `--paleodem-step-myr N` bakes only PaleoDEMs whose reconstruction age is a multiple of
N (plus the present-day bridge and 540 Ma frames); the rotation table keeps every source age
regardless.

**Loss of reconstructed ice outside ICE-6G_C coverage:** older than 26.076 ka (game years) the globe
has no ice overlay. The Quaternary bridge uses modern terrain (the present-day PaleoDEM) shifted by
the sea-level curve, so glacial coastlines are approximated but Pleistocene ice sheets before
26 ka and all pre-Quaternary ice are absent; PaleoDEMs carry their own published sea level.

## Region tiles: ETOPO 2022 and Köppen-Geiger

Inputs (both pinned in `data/sources.json`):

| File | Size (bytes) | SHA-256 |
|---|---|---|
| `data/raw/etopo/ETOPO_2022_v1_60s_N90W180_surface.nc` | 478,290,125 | `74bbc5c23e85188d93f0beba8116babf69e1a8f160e8dc9f053bf8df01b197b6` |
| `data/raw/koppen/koppen_geiger_tif.zip` | 130,618,411 | `bb84453d4541f1a0bc5a804ead83f483c19ce70f16a5197f6d3a7b6a63e65562` |

- **ETOPO 2022** 60 arc-second surface model (NOAA NCEI, doi:10.25921/fd45-gt74), URL
  `https://www.ngdc.noaa.gov/thredds/fileServer/global/ETOPO2022/60s/60s_surface_elev_netcdf/ETOPO_2022_v1_60s_N90W180_surface.nc`.
  Variable `z` (m, EGM2008 height), 10800 × 21600. The axes are **cell centres** (lat
  −89.99167…89.99167, lon −179.99167…179.99167, `node_offset = 1`), so the bake averages the four
  cells around each node onto the 60″ node lattice (the poles average their two nearest cells,
  longitude wraps).
- **Köppen-Geiger** maps, Beck et al. (2023), Figshare article 21789074 (file
  `https://ndownloader.figshare.com/files/61012822`, md5 `7fc2f5a15d4f5fe0ce59c9a9b502aa09`
  per the figshare page). The zip holds a GeoTIFF per period, scenario and resolution plus
  `legend.txt`. Used: **`1991_2020/koppen_geiger_0p00833333.tif`** (uint8, 21600 × 43200, 30″
  cells, north up, origin 90 N / 180 W, LZW so decoding needs `imagecodecs`). Codes 1–30 are the
  Beck classes (`legend.txt`) and 0 is sea / no data. The bake takes the mode of the 2 × 2 cells
  centred on each node (zeros only win when no land class is present; ties go to the smaller
  code).

`cd tools/bake && uv run timefly-bake tiles --raw ../../data/raw --out ../../crates/timefly/assets/data/tiles`
writes 2,592 tiles `r{row:02}c{col:02}.tfgrid` (36 rows × 72 columns, row 0 northernmost, column
0 from lon −180) and the overview in an immutable directory. The version 2 manifest's
`generation` field names this directory. The baker replaces `manifest.json` after all payloads
are ready. Earlier generations remain readable, and version 1 manifests still use the flat layout.
Each tile is a 5° × 5° `latlon` grid of 301 × 301 samples
(both edges included, so neighbours share edge samples and column 71 meets column 0 across the
antimeridian), 2 channels, float16: elevation in metres, then the Köppen code. The manifest lists
every tile with its byte size and SHA-256 and the two source checksums. Sizes:
[`docs/testing/region-m2/performance.md`](../docs/testing/region-m2/performance.md).

## Past climate: Beyer et al. 2020

Beyer, R.M., Krapp, M. & Manica, A. (2020). An empirical evaluation of bioclimatic and
vegetation reconstructions for the last 120,000 years. *Scientific Data* 7, 236,
doi:10.1038/s41597-020-0552-1. Data: figshare doi:10.6084/m9.figshare.12293345 (article
12293345), CC BY 4.0. Changes: BIO1 and BIO12 regridded to 1°.

The data is one 1,333,236,636-byte file, `LateQuaternary_Environment.nc` (figshare file 22659026,
`https://ndownloader.figshare.com/files/22659026`, md5 `d23abb7ce8671433ea36a2cae814ebab` per the
figshare files API), HDF5 inside. It is **not** downloaded whole and is not in the `files` list
of `data/sources.json`, so `timefly-fetch` never touches it. It has its own top-level
`range_read` list, and `timefly-bake climate` reads from it with HTTP range requests through
h5py (`timefly_bake.beyer.RangeFile`), holding 64 blocks in memory and writing nothing to
disk. The ndownloader redirects every ranged GET to a signed URL that lives about ten seconds, so
each request starts from the public URL and follows one redirect, which must go to https. The
reader refuses to fetch more than `MAX_FETCH_BYTES` (100 MB). The API's size and md5 must match
the pins before any block is read, and SHA-256 pins on the two extracted arrays
(`range_read[].variables`) catch a changed file.

What the file holds (read from this run's log and header): 72 time steps (`time`, int32, "years
since present", -120000 to 0 in steps of 2000 up to 22 ka and 1000 below, i.e. ages 0 to 21 ka
by 1, then 22 to 120 ka by 2), `latitude` 300 rows from -59.75 to 89.75 (south first, 0.5°) and
`longitude` 720 columns from -179.75 to 179.75. Land only: sea and Antarctica are NaN or absent.
Each bioclimatic variable is `(72, 300, 720)` float32, chunks `(24, 100, 240)`, gzip, about 19 MB
stored. The variables carry only a `long_name` (no `units` attribute); the values are °C for
`BIO1` and mm/yr for `BIO12` (Amazon about 27 °C and 1,850 mm, Sahara about 23 °C and 20-45 mm),
and the coordinate variables' `units` attributes are mislabeled (latitude says `degrees_east`),
so the bake places values by the numbers, not the labels.

Baked ages (ka): 0 to 21 in steps of 1, then 22, 24, 26, then 30, 40, ..., 120 (35 grids).

`cd tools/bake && uv run timefly-bake climate --sources ../../data/sources.json --out ../../crates/timefly/assets/data/climate`
(`--record-pins` prints the two array digests instead of checking them) writes, flat in that
folder: `manifest.json` (version 1, replaced last) and one `bio_<age>ka.<sha256>.tfgrid` per age.
Each is a `latlon` F16 grid of shape (180, 360, 2): row 0 is centred on 89.5° N, column 0 on
179.5° W, channel 0 is BIO1 (°C) and channel 1 is BIO12 (mm/yr), NaN where Beyer has no land
value. The 1° value is the mean of the 2 × 2 half-degree cells that have one. The manifest lists
every file's age, byte size and SHA-256, the figshare file id, the SHA-256 of each extracted
0.5° array (`source.variables_sha256`) and the attribution. The first run fetched 39,621,532
bytes in 66 requests (about 40 s); the 35 grids total 2,486,321 bytes. The tiny test fixture
`crates/timefly/tests/assets/data/climate/` is written by
`tools/bake/scripts/write_climate_fixture.py` through the same code on a synthetic file.

## Present-day lake map: Natural Earth

| File | Size (bytes) | SHA-256 |
|---|---|---|
| `data/raw/naturalearth/ne_10m_lakes.zip` | 2,349,685 | `0803a06f9c3cb4671d89b68c48b142aad9366ba40f665245e12a913fbc61722a` |

Natural Earth 1:10m physical vectors, lakes, version 5.0.0, URL
`https://naciscdn.org/naturalearth/10m/physical/ne_10m_lakes.zip` (public domain; no permission
needed, credit appreciated). The zip holds a polygon shapefile in lon/lat degrees with 1,355
features: 1,056 `Lake`, 69 `Alkaline Lake` and 230 `Reservoir` (`featurecla`). The Caspian is not
among them (Natural Earth maps it as sea; the lake level table holds it).

The rivers' continental pass checks lakes against it where the terrain is today's (recent eras;
rivers and lakes Task 8b, owner ruling 2026-11-25): an open lake the map does not hold is cut
through, and one standing above its mapped surface drops to it.

`cd tools/bake && uv run timefly-bake lakemap --zip <ne_10m_lakes.zip> --etopo ../../data/raw/etopo/ETOPO_2022_v1_60s_N90W180_surface.nc --out ../../data/hydrology/lake_map.bin`
(`timefly_bake.lakemap`) reads the shapefile straight from the zip with its own small reader,
keeps the lakes (`Lake`, `Alkaline Lake`; reservoirs are dropped, since dams are not in the
terrain and the game spans the past), rasterises them at cell centres onto a 3′ equirectangular
grid (7,200 × 3,600, row 0 north; islands are holes) and gives each lake the median of the ETOPO
2022 60″ surface cells whose centres lie inside it. Lakes that hold no 3′ cell are left out:
1,059 lakes, 54,152 cells, a 74,562-byte file (row run-lengths; the format is in the module's
docstring, and `timefly_core::hydrology::lakemap` decodes it). Levels read: Titicaca 3,808.5 m,
Victoria 1,133 m, the Great Salt Lake 1,278 m, the Dead Sea −427 m (the 10 km overview reads
about −276 m there), Tanganyika 770 m, Malawi 474.5 m, Issyk-Kul 1,606.5 m.

At the present-day snapshot the pass takes every lake from this map (Task 8b round 2): each
mapped lake at its baked surface over its own lattice nodes, the lake level table's lakes at
the table's levels; in past recent eras the map only caps open lakes.

`data/hydrology/straits.json` lists seaways narrower than the pass's 10 km lattice (the
Bosporus, sill about 35 m): with today's terrain the pass lowers their nodes to the sill, moved
with the era's shift, so the Black Sea is sea at the present day and closed when the sea stood
below the sill. Hand-made, like `lake_levels.json`; Task 11's Python reads it too.

## Today's river courses: Natural Earth

| File | Size (bytes) | SHA-256 |
|---|---|---|
| `data/raw/naturalearth/ne_10m_rivers_lake_centerlines.geojson` | 7,307,743 | `bb854a900ecbd3b408df46d5e16e3e0f974ba55993f9d8b5c26e855273c0905a` |

Natural Earth 1:10m physical vectors, rivers + lake centerlines, version 5.1.2, URL
`https://raw.githubusercontent.com/nvkelso/natural-earth-vector/v5.1.2/geojson/ne_10m_rivers_lake_centerlines.geojson`
(public domain; no permission needed, credit appreciated). 1,455 MultiLineString features: 1,202
`River` and 253 `Lake Centerline` (`featurecla`), `scalerank` 0–10.

`cd tools/bake && uv run timefly-bake rivers-today --geojson <ne_10m_rivers_lake_centerlines.geojson> --etopo ../../data/raw/etopo/ETOPO_2022_v1_60s_N90W180_surface.nc --out ../../data/hydrology/rivers_today.json`
(`timefly_bake.rivers_today`) keeps the `River` features of `scalerank` ≤ 7 (lake centrelines are
left out: today's lakes are the lake map's), one polyline per line part (655 features, 1,275
parts). It puts each part upstream first: the end whose lowest ETOPO 2022 60″ cell within 2′ (two
cells either way) stands higher goes first, ties keeping Natural Earth's order (138 parts were
reversed). It simplifies each part to 1 km (Ramer–Douglas–Peucker on great-circle distances) and
rounds points to 0.001°, dropping parts that round to one point: 1,248 courses, 52,896 points
(of 171,153), a 926,552-byte table, one course per line.

Where the terrain is today's (recent eras; rivers and lakes Task 11b, owner ruling 2026-11-26),
the continental pass burns each course into its routing: the running minimum of the era's node
elevations along it, less 10 m (`TODAY_RIVER_BURN_M`), never below sea level on land. The river
graph's bed there is the running minimum itself. Without it the lattice's routes across flat
plains leave the real courses: the lower Indus ran about 90 km east of Sehwan.

## Performance and memory (Task 10)

Full results, method and the Chrome WebGPU steps: [`docs/testing/globe-m1/performance.md`](../docs/testing/globe-m1/performance.md).
In-game credits (`Esc`) list every citation above (`crates/timefly/assets/credits.txt`).

Native Metal, Apple M4, macOS 27.2.1, release build `c249528`, 2560x1440, this bake (137
frames, 136 images, cube size 128, 17,177,338 bytes):

- **Scrub frame times** (20 s each way, warm): median 16.6 ms, p95 18.3 ms (older) / 19.1 ms
  (younger), worst 27.2 ms, no frame over 33.3 ms, bridge included. Presentation is paced at the
  display's 60 Hz, so p95 is pacing jitter (these runs predate the R14 rule below and were judged
  by p95 alone). Informational; the 60 fps target is checked on Linux (Task 11).
- **Cold load to first interactive frame:** 3.95 s and 4.30 s for the first launch right after
  a build, 0.53 s for another first launch, 0.39-0.40 s on relaunch.
- **GPU texels:** 61.1 MiB of decoded frame textures (as estimated, ~61 MiB), plus the bridge:
  28.1 MiB persistent targets and 281.2 MiB of MSAA/depth/main textures for its two cameras at
  2560x1440 (both resident from warm-up on).
- **Process footprint** (Apple silicon, includes GPU): ~1.0-1.07 GiB at 2560x1440, 658 MiB at
  1280x720; about 130 B per window pixel, of which the bridge is ~88 B/px.
- **System spans:** no measurable p95 change; ~3-4% more user CPU. Kept as a default feature.
- **Data size:** 17.2 MB on disk, already zstd-19 (~17.0 MB with HTTP compression), inside the
  ~20 MB target; preloading every frame is affordable on desktop. Browser transfer and memory
  numbers are pending the Chrome run.
- **Web app size** (`web-release` profile, no debuginfo, stripped): 65.8 MB raw, 7.7 MB
  brotli -q 11 (was 100.6 MB / 14.6 MB with the `release` profile).
- **Bench rule:** frames here are display-paced, so the bench judges missed frames (> 1.5x the
  interval, at most 1%) and median (at most 5% over the interval), ruling R14: the re-run passes
  with 0.08-0.17% missed frames. The bench block names the bake as
  `manifest v2, sha256 013f81f437f19736`.

Linux (Vulkan, `.deb` and Flatpak from CI): pending the owner's run of
[`docs/testing/linux-vulkan.md`](../docs/testing/linux-vulkan.md); results are recorded here
when they arrive.

Reference files:

- `reference/draft/bevy-hybrid-lod-architecture-v4.md`: architecture draft (v4).
- `reference/draft/terrain_cdlod.wesl`: draft CDLOD terrain shader.
- `reference/draft/voxel_surface_nets.wesl`: draft voxel surface nets shader.
- `data/culture/civilizations.csv`: source chronology of global human civilizations, copied unchanged.
