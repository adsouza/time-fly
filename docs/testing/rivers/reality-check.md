# Rivers reality check (Task 11)

The hidden `--hydro-check[=OUT_DIR]` runs the continental pass at 0 ka on the real bake and
compares it with a 1′ reference pass over two boxes, five lakes and four river outlets
(`crates/timefly/src/hydro_check.rs`, bars in `CheckSpec::standard`).

## How to run it

```sh
# The references (each well under 10 minutes; both files 0.6 MB):
cd tools/bake
uv run timefly-hydro-ref --box indus --etopo …/ETOPO_2022_v1_60s_N90W180_surface.nc \
  --climate …/crates/timefly/assets/data/climate \
  --out ../../crates/timefly/tests/fixtures/hydro_reference/indus.json
uv run timefly-hydro-ref --box amazon …   # same, amazon.json

# The check (headless, no window):
TIMEFLY_LOG_DIR="$(mktemp -d)" BEVY_ASSET_ROOT="$ROOT" target/debug/timefly --hydro-check=OUT
```

It exits 0 on PASS and 3 (`FAILED_RESULT_EXIT_CODE`) on FAIL. `$ROOT/assets/data` is the main
checkout's real data.

Reference timings (M4, Python): Indus 6.7 s (870 × 960 nodes), Amazon 25 s (1620 × 1920 nodes).
`indus.json` is 95,668 bytes and `amazon.json` 503,031 bytes.

## Result, 2026-11-26 (rivers-and-lakes after Task 10, plus this task's gorges)

The binary was the `dev` profile (opt-level 1 for the workspace, 3 for dependencies), not
`target/release`: release was not built, to save disk. Pass time at 0 ka: 1.22 s (0.20 s sampling).
The lattice is n = 900, with 121,263 river nodes and 861 lakes.

**hydro check: FAIL.** The Indus box fails. Every other bar passes.

| Bar | Needs (brief, or † set in this task) | Relaxed (judged) | Coarse graph | Verdict |
|---|---|---|---|---|
| Indus box | ≥ 80% within 10 km, every link | 76.4%, 70/84 links, worst 134.6 km | 75.0%, 69/84, worst 137.5 km | **FAIL** |
| Amazon flats (< 300 m) | ≥ 80% within 25 km | 91.7%, 456/482 links, worst 144.3 km | 91.1%, 454/482, worst 142.6 km | PASS |
| Caspian Sea (42.0, 50.5) | within 5 m of the table (−28.0) | −28.0 m | | PASS |
| Dead Sea (31.5, 35.5) | within 5 m of the map's fine surface (−427.0) | −427.0 m | | PASS |
| Titicaca (−15.8, −69.4) | the same (3,808.5) | 3,808.5 m | | PASS |
| Great Salt Lake (41.1, −112.5) | the same (1,278.0) | 1,278.0 m | | PASS |
| Victoria (−1.0, 33.0) | the same (1,133.0) | 1,133.0 m | | PASS |
| Danube from Budapest | through the Iron Gate (44.67, 22.5) to the Black Sea; † within 15 km of the gate, ocean within 150 km of the delta | 0.5 km from the gate; ends at (45.16, 29.23), 37 km from the delta | | PASS |
| Jhelum from Srinagar | out of the Kashmir valley at Baramulla (34.2, 74.35); † within 15 km of it, then the sea within 150 km of the Indus mouth | 4.0 km from Baramulla; through Mangla, the Indus, to the sea 144 km from its mouth | | PASS |
| Indus from Skardu | its sea; † ocean within 150 km of (24.0, 67.4) | ends at (24.16, 68.80), 144 km from (24.0, 67.4) | | PASS |
| Amazon from Iquitos | its sea; † ocean within 150 km of (−0.5, −49.5) | through one floodplain lake; ends at (−0.54, −50.76), 140 km from (−0.5, −49.5) | | PASS |
| Arid basins | no lake ≥ 1,000 km² where the basin averages < 250 mm/yr, in either network († basin = lake plus upstream river nodes) | game 0 of 3 (750, 1,942 and 386 mm/yr); reference 0 of 2 | | PASS |

The relaxed and coarse figures differ by about a point. The relaxation cannot fix what is left:
the misses are 40–135 km apart, which are differences in the river's route, not in its valley.

SVGs (reference blue, game red, equirectangular, 100 km scale bar):

- [hydro-check-indus.svg](hydro-check-indus.svg)
- [hydro-check-amazon.svg](hydro-check-amazon.svg)

### What the Indus box misses

The check prints each unmatched reference link. The longest ones:

| Reference link | Length | Within 10 km | Why |
|---|---|---|---|
| Lower Indus, (29.11, 70.69) to the sea | 939 km | 18% | The reference follows the real course west by Sehwan (26.4, 67.9). The game's Indus runs about 90 km east of Sehwan, straight down the Sindh plain. This is a flat plain, not a gorge. |
| Shyok, Ladakh to Skardu | 332 km | 20% | It takes another valley at 10 km. |
| Lower Jhelum, Mangla to the Chenab | 329 km | 34% | It runs on another line across the Punjab doab. |
| Arghandab (Afghanistan) | 317 km | 0% | The game's river runs in a parallel valley up to 56 km off. |
| 10 more | 2–178 km | 0–39% | Mountain valleys and the Thar's edge. |

Before this task's Upper Indus gorge, the box read 74.3% with 69/84 links. Then the Indus's
Tibetan headwaters (32.5, 80.8) flowed east through three Tibetan lakes to the Bay of Bengal
(15.98, 95.65).

To pass, the lower Indus would need the Sindh plain routed by its real channel. That means
"burning" a river course across a plain. Gorge entries were not meant for that. The decision on
the bar, or on burning plains, is the owner's.

## Choices made in this task

The brief gives the box, lake and arid bars' numbers. The choices below were made while building
the check, and the controller has ratified them (2026-11-26).

- **Outlet tolerances** (`hydro_check.rs`): the river starts at the largest within
  `OUTLET_START_KM` = 30 km of its point. It must pass within `OUTLET_VIA_KM` = 15 km of its
  gorge, about one and a half lattice spacings. It must end in the ocean within `OUTLET_SEA_KM` =
  150 km of its mouth. The brief only says "reach their real outlets".
  - Why 150 km: the misroutes this bar exists for end in the wrong sea, 1,000 km or more off. The
    Danube reached the Oder mouth, 1,469 km off; the Indus headwaters reached the Bay of Bengal,
    over 3,000 km off. Deltas and estuaries are wide: the Amazon's is about 300 km across.
  - Margins on this run:
    - Danube: 0.5 km from the gate (14.5 km margin), 37 km from the delta (113 km).
    - Jhelum: 4.0 km from Baramulla (11 km margin), 144 km from the Indus mouth (6 km).
    - Indus: 144 km from the mouth (6 km margin).
    - Amazon: 140 km from the mouth (10 km margin).
  - The Indus (and the Jhelum, which follows it) ends 144 km east of (24.0, 67.4), at (24.16,
    68.80) by Kori Creek. That is the same Sindh-plain route error the Indus box fails on: the
    game's lower Indus runs east of the real course.
- **The Upper Indus gorge agrees with the reference by construction.** Its course and levels are
  the 1′ reference's own Indus link from (32.49, 80.81) to Nimo, so the game's river there follows
  the reference's line. That link is 459 km of the box's 16,570 km of reference links, about 2.8%.
  The Indus box reads 74.3% without the gorge and 76.4% with it (69 → 70 of 84 links). The other
  two gorges lie outside both boxes.
- **Game rivers near the box, not clipped to it.** The controller's ruling said to compare the
  game's rivers clipped to the box. The check instead keeps every game segment with an end within
  the box plus 0.5° (44–56 km across these boxes' latitudes). Distances are measured only from reference points, which all lie
  inside the box, and the widest tolerance is 25 km. So a game segment outside the margin can never
  count. A segment inside the margin counts only where it truly lies within the tolerance of a
  reference point. Strict clipping would cut a segment that crosses the box edge, and would score a
  reference river along the edge as missed when the game's river runs a few km outside. The SVG
  shows only the box itself.
- **The arid bar's "basin mean precipitation" is a proxy.** It is the area-weighted mean over the
  lake's own nodes and the river nodes (discharge ≥ 20 m³/s) upstream of it. The bar uses the same
  definition in both networks. The game's result keeps no per-node receivers, so its full basin
  cannot be listed; the Python side uses the same set to match. Smaller streams' land is left out
  on both sides.
- **The reference fixture matches the committed table.** `indus.json` was re-baked from the
  committed `straits.json` on 2026-11-26 and came out byte-identical (95,668 bytes).

## Gorges added to `data/hydrology/straits.json`

The table now holds straits (a sill) and gorges (a river surface per point, interpolated between
points). Both the pass and the Python reference read it.

| Entry | Why | Source |
|---|---|---|
| Iron Gate (Golubac to below the Iron Gate I dam, 66 m to 41 m) | Without it, the Pannonian basin spilled through the Moravian Gate. The Danube reached the Baltic by the Oder at (53.79, 14.14), 1,469 km from its delta. | Wikipedia, "Iron Gates" (read 2026-11-26), for the gorges and the dam. Course and levels from ETOPO 2022 60″ (doi:10.25921/fd45-gt74). |
| Danube Bend (Esztergom to Budapest, 105 m to 101.5 m) | Found by this check: without it, the Danube left the Little Hungarian Plain past Lake Balaton and missed Budapest. | Karátson (2015), *Danube Bend: Miocene Half-Caldera and Pleistocene Gorge*, in Lóczy (ed.), *Landscapes and Landforms of Hungary*, Springer, doi:10.1007/978-3-319-08997-3_16. Levels from ETOPO 2022. |
| Upper Indus (32.49, 80.81) to Nimo (34.18, 77.33), 4,495 m to 3,210 m | Without it, the Indus's headwaters went to the Bay of Bengal. | Kumar & Srivastava (2017), *Quaternary Research* 87(3), 363–385, doi:10.1017/qua.2017.19. Course and levels from the 1′ reference's own Indus link. |
| Bosporus (−35 m), now with its sill cited | — | Gregg & Özsoy (2002), *J. Geophys. Res.* 107(C3), 3016, doi:10.1029/2000JC000485, ¶52: South Sill is mostly 30–40 m deep. |

The Danube now passes Budapest and the Iron Gate and reaches the Black Sea. Between them, it
still crosses the Great Hungarian Plain by Szeged rather than by Mohács and Novi Sad. That is the
lattice's routing on a flat plain, not a gorge, so it is not fixed here.

## Constants

No constants were tuned. The misses come from route choice, so the corridor, discharge threshold
and runoff table cannot change them. The hydro golden fixture and the region golden hashes are
unchanged: the full workspace tests pass.
