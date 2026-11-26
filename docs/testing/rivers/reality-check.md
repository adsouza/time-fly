# Rivers reality check (Task 11)

The hidden `--hydro-check[=OUT_DIR]` runs the continental pass at 0 ka on the real bake and
compares it with a 1′ reference pass over two boxes, five lakes and 22 river outlets
(`crates/timefly/src/hydro_check.rs`, bars in `CheckSpec::standard`). It also prints a
discharge table for 18 gauges (informative, not a bar).

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

## Result, 2026-11-26 (Task 11d: the world's major river systems)

Owner, 2026-11-26: "We should do sanity checks on other major river systems of the world". The
check now follows 18 more rivers from a cited start through cited places to a cited mouth, with
the tolerances unchanged (start 30 km, each via 15 km, mouth 150 km). It also compares the
game's discharge at one cited gauge per river with the published long-term mean.

The binary was the `dev` profile. Pass time at 0 ka: 1.46 s and 1.44 s on two runs (0.23 and
0.22 s sampling), against 1.30 s in Task 11b's run and 1.41 s on this task's first run (canals
out, no gorges yet). The lattice is n = 900, with 118,759 river nodes (118,792 before) and 861
lakes (861).

Longest steps (`real_continental_pass_timing`, dev build), three runs while other work loaded the
machine (load average 4.8–8.6):
- 0 ka, at 6 ms: 3.03, 2.07 and 1.81 ms, all route.
- 0 ka, at 12 ms: 2.80 and 2.48 ms (route) and 2.77 ms (runoff→walk).
- 21 ka: 2.19–2.65 ms (walk and route), and one 8.21 ms step in sample at 12 ms.

Task 11b recorded 1.39–1.64 ms at 0 ka and 1.52–2.03 ms at 21 ka. None of the longest steps
lies in the two stages this task changed (two more straits entries, 31 points, and seven fewer
courses). The runs vary by 1.2 ms between themselves, so they do not show the 2 ms bound either
way. A quiet machine's run is still owed.

**hydro check: FAIL.** 16 of the 22 outlets pass; 6 new ones fail (below, for the owner). The
Indus box still fails on its links only, as in Task 11b. Every earlier bar reads exactly as in
Task 11b's run:
- Indus box: 81.6%, 73/84 links, worst 77.5 km.
- Amazon flats: 91.6%, 458/482, worst 145.9 km.
- The five lakes, at their table or map levels.
- The Danube, Jhelum, Indus and Amazon outlets, at the same margins.
- Arid basins: game 0 of 3, reference 0 of 2.
- The burn's depth line: 30,738 burnt nodes, max 1,362 m, p99 439 m, 4,452 over 50 m.

### What changed in the check

- **Several vias per outlet**, each judged (`OutletBar::vias`, in order). The bar traces the
  river once per via.
- **A lake end** (`OutletBar::ends_in`: `ocean` or `lake`). The Volga's sea is the Caspian, a
  lake: its bar needs the trace to end in a lake (a lake with no outflow) within 150 km of its
  mouth. `an_outlet_ends_in_the_water_its_bar_names` pins both kinds and the vias.
- **A source per outlet** (`OutletBar::source`). `the_major_rivers_have_cited_outlets` checks:
  - Every entry names its sources.
  - Every start lies within 30 km, and every via and gauge within its tolerance, of a burnt
    course.
  - No start, via or gauge lies inside a lake of the lake map.
  - The lake-ended Volga's mouth lies within its allowance of its course and names a
    level-table lake (the Caspian).

  On its first run the test found two points that broke these rules:
  - Jinja's own point lies in the lake map's Lake Victoria, so the Nile now starts at the Owen
    Falls Dam, Jinja's outlet ("Nalubaale Hydroelectric Power Station").
  - The Volga's mouth is in no lake-map lake: the Caspian is a level-table lake.
- **The discharge table** (`DischargeGauge`, `discharge_row`; informative). The game's value is
  the largest discharge among the river nodes within 30 km of the gauge, as an outlet's start.
  A ratio outside [1/3, 3], or no river at the gauge, is flagged.
  `the_discharge_table_flags_ratios_outside_three` pins the bounds.
- **A trace fix: a lake's outflow can leave by a far diagonal neighbour.** `trace_outlet`
  crossed a lake to the river leaving it, searching 1.5 mean spacings from the lake's exit. The
  outflow goes to one of the exit's lattice neighbours, and near a cube face's centre a diagonal
  neighbour stands up to about 1.9 mean spacings away.
  - The Niger's Lake Débo (lake 523, at 15.31° N, 4.00° W) drains to a diagonal about 19 km
    away, beyond the 15.4 km search. So the trace stopped in the lake: "ends in a lake (needs
    the ocean) at (15.31, −4.00)".
  - The search now also takes the river nodes at the exit's neighbours
    (`an_outlet_trace_leaves_a_lake_by_a_far_diagonal_neighbour`).
  - It changes no earlier outlet. The game's network is unchanged: this was the check's own
    bug.

### Outlets

Each start, via and mouth is an English Wikipedia article's coordinates (its own, or its
infobox's mouth), read 2026-11-26. In the "Vias" column, the margin under 15 km follows each
distance in brackets. "End" gives where the trace ends, its distance from the cited mouth and
the margin under 150 km.

| River | Start | Vias (km off, margin) | Mouth (cited) | End, km off (margin) | Verdict |
|---|---|---|---|---|---|
| Ganges | "Kanpur" (26.44992, 80.33187) | "Varanasi" (25.31889, 83.01278): 0.1 (14.9); "Farakka Barrage" (24.80444, 87.93306): 1.2 (13.8) | "Ganges", infobox (22.0, 90.9) | (22.94, 90.57), 110 (40) | PASS |
| Brahmaputra | "Zangmu Dam" (29.185, 92.51667), on the Yarlung Tsangpo above the Great Bend | "Pasighat" (28.07, 95.33): 5.7 (9.3); "Guwahati" (26.17222, 91.74583): 2.7 (12.3) | the Ganges's (its own infobox mouth is the Ganges confluence) | (22.94, 90.57), 110 (40) | PASS |
| Yangtze | "Chongqing" (29.5637, 106.5504) | "Three Gorges" (31.01778, 110.22861): 1.3 (13.7); "Yichang" (30.692, 111.287): 0.0 (15.0); "Wuhan" (30.595, 114.2975): 3.4 (11.6) | "Yangtze", infobox (31.39361, 121.98306) | (34.13, 120.35), 341 (−191) | **FAIL** |
| Yellow River | "Guide County" (35.989, 101.471) | "Lanzhou" (36.0606, 103.8268): 0.4 (14.6); Zhengzhou at "Huayuankou, Henan" (34.87694, 113.67278): 1.4 (13.6) | "Yellow River", infobox (37.763, 119.162) | (38.13, 118.12), 100 (50) | PASS |
| Pearl | "Guiping" (23.4, 110.08333) | "Wuzhou" (23.4767, 111.279): 4.4 (10.6) | "Pearl River", the article's coordinates (the infobox gives none) (22.76667, 113.63333) | (22.59, 113.70), 20 (130) | PASS |
| Mekong | "Vientiane" (17.98, 102.63) | "Phnom Penh" (11.56944, 104.92111): 7.6 (7.4) | "Mekong", infobox (10.19, 106.75) | (10.04, 104.99), 193 (−43) | **FAIL** |
| Nile | "Nalubaale Hydroelectric Power Station" (0.44361, 33.185), the Owen Falls Dam at Jinja | "Khartoum" (15.6, 32.5): 1.8 (13.2); "Aswan" (24.08889, 32.89972): 5.6 (9.4) | "Nile", infobox (30.1725, 31.14), the delta's head | (31.28, 31.75), 136 (14), through 2 lakes | PASS (after the Victoria Nile gorge) |
| Congo | "Kindu" (−2.95, 25.95) | "Kisangani" (0.51667, 25.2): 2.0 (13.0); "Kinshasa" (−4.32194, 15.31194): 3.2 (11.8) | "Congo River", infobox (−6.075, 12.45) | (−6.00, 12.59), 17 (133) | PASS |
| Niger | "Bamako" (12.63917, −8.00278) | "Inner Niger Delta" (15.2, −4.1): 10.3 (4.7); Timbuktu at "Kabara, Mali" (16.70695, −2.9853): 6.7 (8.3); "Lokoja" (7.80194, 6.74417): 8.6 (6.4) | "Niger River", infobox (5.32222, 6.46917) | (4.99, 5.40), 124 (26), through Lake Débo | PASS (after the trace fix) |
| Mississippi | "Minneapolis" (44.98194, −93.26917) | "St. Louis" (38.62722, −90.19778): 0.6 (14.4); "New Orleans" (29.9761, −90.0783): 51.8 (−36.8) | "Mississippi River", infobox (29.15111, −89.25333) | (30.16, −90.57), 170 (−20) | **FAIL** |
| Hudson | "Glens Falls, New York" (43.31222, −73.64833) | "Albany, New York" (42.6525, −73.75722): 3.8 (11.2) | "Hudson River", infobox (40.69667, −74.02833) | (40.52, −73.83), 26 (124) | PASS |
| Yenisei | "Kyzyl" (51.71667, 94.45) | "Krasnoyarsk" (56.00889, 92.87194): 1.8 (13.2) | "Yenisei", infobox (71.83333, 82.66667), the Yenisey Gulf | (71.02, 82.76), 90 (60) | PASS (after the Krasnoyarsk gorge) |
| Ob | "Barnaul" (53.34861, 83.77639) | "Novosibirsk" (55.05, 82.95): 168.9 (−153.9) | "Ob", infobox (66.53389, 71.39472), the Gulf of Ob | (66.58, 69.43), 87 (63), through 2 lakes | **FAIL** (the via) |
| Orinoco | "Caicara del Orinoco" (7.65, −66.16667) | "Ciudad Bolívar" (8.13793, −63.54827): 0.1 (14.9) | "Orinoco", infobox (8.61667, −62.25), the delta's head | (9.78, −61.19), 174 (−24) | **FAIL** (tolerance only) |
| Paraná | "Corrientes" (−27.48333, −58.81667) | "Rosario" (−32.9575, −60.63944): 3.1 (11.9) | "Paraná River", infobox (−34.00139, −58.39361), the Paraná Guazú | (−34.07, −58.34), 9 (141) | PASS |
| Lena | "Kirensk" (57.78333, 108.08333) | "Yakutsk" (62.03, 129.73): 0.4 (14.6) | "Lena River", infobox (72.4087, 126.6847) | (73.28, 128.69), 117 (33) | PASS |
| Volga (to a lake) | "Nizhny Novgorod" (56.32694, 44.0075) | "Volgograd" (48.70861, 44.51472): 157.3 (−142.3) | "Volga", infobox (45.695, 47.8975), the delta on the Caspian | the ocean (needs a lake) at (47.16, 39.16), 689 (−539) | **FAIL** |
| Mackenzie | "Fort Providence" (61.35472, −117.66) | "Fort Simpson" (61.86083, −121.35278): 1.9 (13.1) | "Mackenzie River", infobox (68.93972, −136.17278) | (68.86, −136.16), 9 (141) | PASS |

Points that are not the place's own coordinates:
- **Zhengzhou → Huayuankou.** Zhengzhou's own point stands 15.4 km from Natural Earth's Yellow
  River, so the via is Huayuankou, its riverside district and gauge.
- **Timbuktu → Kabara.** Timbuktu stands 13.1 km from the river, so the via is Kabara, its
  port, 5.3 km from it.
- **Jinja → the Owen Falls Dam.** Jinja's own point is in the lake map's Lake Victoria, so the
  Nile starts at the dam at its outlet.
- **Brahmaputra start.** Mainling and Nyingchi stand 33 and 27 km from the burnt Yarlung and
  Dihang, so the start is the Zangmu Dam, 0.5 km from the Yarlung.

The earlier four outlets now carry the same source strings (unchanged points).

### Gorges this task added (narrow passes)

| Entry | Why | Before | After |
|---|---|---|---|
| Victoria Nile (Jinja to Lake Kyoga, 1,114.7 m at the first node to 1,030.1 m) | The lattice's node at the outlet stands at 1,170 m. ETOPO's mean within 5′ is 1,187 m, and the river's lowest 2′ cell is 1,134.6 m. So the 10 m burn leaves the outlet at 1,160 m, above Lake Victoria's eastern rim at 1,152 m (0.26, 34.00): the lake drained at (0.16, 33.91), 80 km east of Jinja, and reached Kyoga by another valley. | "no river within 30 km" | through Lakes Kyoga and Albert to the sea, 136 km from the cited mouth |
| Krasnoyarsk Reservoir (from the Minusinsk basin, 241.0 m to 232.0 m) | Natural Earth files the reservoir as a lake centreline, which the table leaves out, so the burnt Verkhniy Yenisey stops at (53.74, 91.51). The reservoir's lowest 2′ cells are 232–241 m, but ETOPO's mean within 5′ along it is 258–400 m: the 10 km lattice closes the valley. The Minusinsk basin filled to 235.2 m and spilled west to the Chulym, and the Yenisei reached the sea by the Ob. | 101 km from Krasnoyarsk; ended at the Ob's mouth (66.58, 69.43), 781 km off | 1.8 km from Krasnoyarsk; 90 km from its mouth |

Each entry has a test in `timefly_core::hydrology::straits`:
`the_victoria_nile_runs_from_jinja_to_lake_kyoga` and
`the_krasnoyarsk_reservoir_runs_from_the_minusinsk_basin_north`. Each test was run against the
old table first and failed there. Both entries' sources are in `straits.json`: Wikipedia
"Victoria Nile" and "Krasnoyarsk Reservoir" (read 2026-11-26), Natural Earth for the course,
ETOPO 2022 for the levels. Both lie outside the Indus and Amazon boxes. With them, the Yenisei
at Igarka reads 20,051 m³/s (16,105 before) and the Ob at Salekhard 17,977 m³/s (21,923
before), because the Yenisei's upper basin no longer drains to the Ob.

### Failing outlets, for the owner (unchanged)

None of these is a narrow pass. Each is described from a probe of the trace's nodes
(lattice position, discharge, bed, the burnt course each node lies on).

- **Yangtze: Natural Earth's course ends at Zhenjiang.** The 1:10m Yangtze ends at (32.20,
  119.61). Its estuary below, about 300 km to the sea, is not among the rank ≤ 7 rivers (as with
  the Amazon). From the course's end the river follows the burnt Hudi's nodes north, against
  that course's direction, at (32.33, 119.59) to (32.62, 119.39). Then it crosses the northern
  Jiangsu lowland (beds 0.7–3 m) to the Yellow Sea at (34.08, 120.26). That ends 341 km from the
  cited mouth, by the old Yellow River mouth rather than Shanghai.
- **Mekong: the Bassac course ends at Can Tho.** Natural Earth's two delta arms end at (10.24,
  106.12) and (10.01, 105.84), short of the sea. From the Bassac's last burnt node at (10.25,
  105.58) the river turns west-south-west across the delta. It reaches the Gulf of Thailand at
  (10.04, 104.99), 193 km from the cited mouth (43 km over).
- **Mississippi: the river drains into Lake Pontchartrain.** It follows its burnt course past
  Baton Rouge. At (30.06, −90.70), by LaPlace, its node's neighbour is an ocean node: the
  Maurepas–Pontchartrain water lies within one 10 km cell of the river's levee. A course node is
  never burnt below 0 m, so the lower ocean node wins. The river misses New Orleans by 51.8 km
  and ends 170 km from the Head of Passes (20 km over). The burnt course runs on to (28.94,
  −89.41) unused.
- **Ob: the Novosibirsk Reservoir gap.** Natural Earth's Ob river ends at Kamen-na-Obi (53.89,
  81.33). The Ob Sea above Novosibirsk is a lake centreline, left out of the table, and the lake
  map drops reservoirs.
  - At (53.60, 81.94), 30 km short of the course's end, the river leaves west across the
    Kulunda plain (beds 119–97 m). It passes two lakes, 468 (98 m, at (53.0, 79.6)) and 719
    (94 m), and joins the Irtysh at Pavlodar (52.97, 76.11).
  - It misses Novosibirsk by 168.9 km but reaches the Gulf of Ob 87 km from the cited mouth.
  - The reservoir's surface is 109.5–114 m and ETOPO's mean within 5′ along it 114–155 m. A wide
    reservoir, not a narrow pass: the divide to the Kulunda is low and broad.
- **Orinoco: tolerance only.** The river follows its burnt course to the delta. It reaches the
  sea by a northern distributary at (9.68, −61.29), node (9.78, −61.19). The cited mouth (8.617,
  −62.25) is the delta's head at Barrancas, so any delta mouth lies 150–190 km from it: 174 km,
  24 km over 150.
- **Volga: it misses Volgograd, and the Caspian overflows.**
  - Natural Earth files the Volga's reservoirs (Cheboksary, Kuybyshev, Saratov, Volgograd) as a
    lake centreline. The river course has gaps there, and the river runs off its line for long
    stretches.
  - Below Saratov it keeps east of the real river: through Lake Elton (lake 767, −17.5 m, at
    (49.10, 46.66)) into the Caspian depression, 157.3 km from Volgograd.
  - It reaches the Caspian (lake 5, level −28 m, kind Open). But the Caspian's inflow, 11,716
    m³/s, exceeds its evaporation, 9,597 m³/s. So 2,119 m³/s leave over its 41.2 m spill at
    (45.52, 44.45), the Kuma–Manych, down the Manych and the Don to the Sea of Azov at (47.16,
    39.16).
  - The real Caspian has no outflow. Its water balance (lake evaporation against the Volga's
    inflow) is the owner's call. The Volga's own discharge is about right (8,888 m³/s above
    Lake Elton against the published 8,228 at Volgograd).

The common thread in the Ob, the Volga and (before its gorge) the Yenisei is that Natural Earth
files reservoirs as lake centrelines. Task 11b left lake centrelines out because the lake map
holds today's lakes, but the lake map drops reservoirs. Burning the lake centrelines that no
lake-map lake covers would close all three gaps without per-river gorges. That would change
Task 11b's selection rule, so it is not done here.

### Discharge table (informative)

The game's discharge is the largest of the river nodes within 30 km of the gauge. The published
value is the long-term mean, from the river's English Wikipedia article (read 2026-11-26) with
the article's own citation, unless named otherwise. Ratios outside [1/3, 3] are flagged.

| River | Gauge (point) | Published mean (period) | Source | Game | Ratio |
|---|---|---|---|---|---|
| Ganges | Farakka Barrage (24.80, 87.93) | 16,648 m³/s | "Ganges", infobox: Kumar, Singh & Sharma (2005), Water Resources of India, *Current Science* 89(5), 794–811 | 10,314 | 0.62 |
| Brahmaputra | Guwahati (26.17, 91.75) | 18,850 (1971–2000) | "Brahmaputra River", infobox: Tilman, Rivers Network, Ganga (Ganges)-Brahmaputra | 9,137 | 0.48 |
| Yangtze | Wuhan, Hankou (30.60, 114.30) | 711.1 km³/yr = 22,534 (1980–2020) | "Yangtze", infobox: Yang et al. (2023), *Front. Mar. Sci.* 10, 1201533, doi:10.3389/fmars.2023.1201533 | 16,655 | 0.74 |
| Yellow River | Huayuankou (34.88, 113.67) | 1,438 (1946–1988) | UNH/GRDC Composite Runoff Fields, station 2180800 (the article names no gauge; its infobox's 2,571 m³/s has no station or citation) | 2,450 | 1.70 |
| Pearl | Wuzhou (23.48, 111.28) | 7,410 | "Xi River", infobox: NASA GES DISC (archived 2008-08-28) | 5,735 | 0.77 |
| Mekong | Kratié (12.48, 106.03) | 13,200 (1960–2004) | "Mekong", Table 2: Mekong River Commission (2005), Overview of the Hydrology of the Mekong Basin | 7,603 | 0.58 |
| Nile | Aswan (24.09, 32.90) | 2,757 | "Nile", infobox: Senay et al. (2014), *Water Resources Research* 50(11), 8625–8650, doi:10.1002/2013WR015231, Table 4 (the article's full entry is commented out) | 14,092 | **5.11 FLAG** |
| Congo | Kinshasa–Brazzaville (−4.32, 15.31) | 40,500 (1902–2019) | "Congo River", infobox: Laraque et al. (2020), *Water* 12(9), 2613, doi:10.3390/w12092613 | 57,852 | 1.43 |
| Niger | Lokoja (7.80, 6.74) | 6,743 (2000–2025) | "Niger River", infobox: Niger-HYCOS, Bilan hydrologique du bassin du Niger année 2023/2024 | 8,819 | 1.31 |
| Mississippi | St. Louis (38.63, −90.20) | 200,500 ft³/s = 5,678 (1933–2024) | "Mississippi River", infobox: USGS Water-Year Summary, site 07010000 | 9,704 | 1.71 |
| Hudson | Green Island (42.74, −73.69) | 17,400 ft³/s = 493 | "Hudson River", infobox: USGS, Hudson River at Green Island NY (01358000) | 412 | 0.84 |
| Yenisei | Igarka (67.47, 86.58) | 19,499 (1971–2015) | "Yenisei", infobox: Stadnyk et al. (2021), Changing freshwater contributions to the Arctic, *Elementa* 9(1), doi:10.1525/elementa.2020.00098 | 20,051 | 1.03 |
| Ob | Salekhard (66.53, 66.60) | 12,889 (1971–2015) | "Ob", infobox: Stadnyk et al. (2021) | 17,977 | 1.39 |
| Orinoco | Ciudad Bolívar (8.14, −63.55) | 32,760 (1926–2011) | "Orinoco", infobox: Córdova & Sanabria, La geografía del agua | 32,776 | 1.00 |
| Paraná | Corrientes (−27.48, −58.82) | 18,979 (1971–2010) | "Paraná River", infobox: Transboundary Diagnostic Analysis for the La Plata Basin | 24,539 | 1.29 |
| Lena | Kyusyur (70.68, 127.37) | 17,773 (1971–2015) | "Lena River", infobox: Stadnyk et al. (2021) | 17,592 | 0.99 |
| Volga | Volgograd (48.71, 44.51) | 8,228 | "Volga", infobox: Tilman, Rivers Network, Volga | no river within 30 km | **FLAG** |
| Mackenzie | Tsiigehtchic, Arctic Red River (67.44, −133.75) | 9,211 (1971–2015) | "Mackenzie River", infobox: NOAA Arctic Report Card, River Discharge | 8,835 | 0.96 |

Flagged for the owner:
- **Nile, 5.1× at Aswan.** The game's Nile gains water where the real one loses it.
  - Through the Sudd the Bahr el Jebel gains (3,905 to 4,079 m³/s) where the real river loses
    about half to the swamps.
  - Below the Sudd, the Bahr el Ghazal adds 3,090 m³/s at Lake No (9.47° N), the Sobat 2,670
    m³/s at Malakal and the Blue Nile 3,317 m³/s at Khartoum.
  - Below Khartoum nothing is lost to the desert: 13,698 m³/s at Khartoum, 14,092 at Aswan.
  - The pass has no wetland evaporation and no channel loss.
  - The published Aswan figure is also below the dam: the regulated river, after Lake Nasser's
    evaporation.
- **Volga at Volgograd.** No river there: the misroute above.

The other 16 lie between 0.48 (the Brahmaputra at Guwahati) and 1.71 (the Mississippi at St.
Louis).

### Canals out of the burn

Natural Earth files canals as `River`. The bake now drops a feature whose name holds a whole
word "canal" or "csatorna" (Hungarian for canal; `ARTIFICIAL_NAME_WORDS`,
`test_select_drops_canals`). The re-baked table holds 1,241 courses, 7 fewer than before:

| Dropped course | Rank | Points | From | To |
|---|---|---|---|---|
| Gharraf Canal (Iraq; Wikipedia "Al-Gharraf River": "an ancient canal") | 4 | 28 | (32.50, 45.81) | (30.96, 46.37) |
| Suez Canal (two parts) | 6 | 8 and 2 | (31.27, 32.31) | (30.18, 32.56) |
| Ismailiya Canal | 6 | 7 | (30.12, 31.24) | (30.59, 32.30) |
| Panama Canal (two parts) | 6 | 2 and 2 | (9.27, −79.92) | (8.96, −79.56) |
| Ferenc Csatorna (the Franz Canal, the Danube–Tisa–Danube Canal; Wikipedia "Danube–Tisa–Danube Canal") | 6 | 9 | (46.20, 18.92) | (45.86, 18.86) |

Every other course is byte-identical. The Hindiyah Channel stays: Wikipedia ("Hindiya Barrage")
describes it as one of the Euphrates' two channels, not a dug canal. No other selected name
reads as artificial (all 606 names were read).

With the canals out, the Indus and Amazon boxes and the four earlier outlets read the same,
digit for digit, as in Task 11b's run. The river graph has 24 fewer river nodes (118,768 before
the gorges, against 118,792).

## Result, 2026-11-26 (Task 11b: today's river courses burnt in)

Task 11b burns Natural Earth's river courses into the pass's routing wherever the terrain is
today's (`data/hydrology/rivers_today.json`, `timefly_core::hydrology::today_rivers`; see "Choices
made in Task 11b" below). The Python reference does not burn: the check now compares two
independent sources of today's rivers, a 1′ DEM and Natural Earth.

The figures below are from Task 11b's fix round 2:
- The river graph alone takes the courses' levels.
- The outlet points are cited (see "Outlet points" below), and the Amazon's allowance is 160 km.
- The regions relax a burnt course's nodes to the course itself (its anchors).
- The graph's beds slope across held stretches.

The binary was the `dev` profile (opt-level 1 for the workspace, 3 for dependencies), not
`target/release`: release was not built, to save disk. Pass time at 0 ka: 1.30 s (0.20 s
sampling), against 1.22 s before. The lattice is n = 900, with 118,792 river nodes (121,263
before) and 861 lakes (861).

**hydro check: FAIL.** The Indus box's share now passes, but 11 links are still missed:
- The Shyok and the Arghandab are not in Natural Earth 1:10m.
- The rest are mostly rivers outside its rank ≤ 7 courses, or short reaches.

Every other bar passes.

| Bar | Needs (brief, or † set in Task 11) | Before (Task 11) | After (Task 11b) | Verdict |
|---|---|---|---|---|
| Indus box, relaxed (judged) | ≥ 80% within 10 km, every link | 76.4%, 70/84 links, worst 134.6 km | 81.6%, 73/84 links, worst 77.5 km | **FAIL** (links only) |
| Indus box, coarse graph | (informative) | 75.0%, 69/84, worst 137.5 km | 80.5%, 72/84, worst 77.7 km | |
| Amazon flats (< 300 m), relaxed | ≥ 80% within 25 km | 91.7%, 456/482 links, worst 144.3 km | 91.6%, 458/482, worst 145.9 km | PASS |
| Amazon flats, coarse graph | (informative) | 91.1%, 454/482, worst 142.6 km | 91.3%, 455/482, worst 142.6 km | |
| Caspian Sea (42.0, 50.5) | within 5 m of the table (−28.0) | −28.0 m | −28.0 m | PASS |
| Dead Sea (31.5, 35.5) | within 5 m of the map's fine surface (−427.0) | −427.0 m | −427.0 m | PASS |
| Titicaca (−15.8, −69.4) | the same (3,808.5) | 3,808.5 m | 3,808.5 m | PASS |
| Great Salt Lake (41.1, −112.5) | the same (1,278.0) | 1,278.0 m | 1,278.0 m | PASS |
| Victoria (−1.0, 33.0) | the same (1,133.0) | 1,133.0 m | 1,133.0 m | PASS |
| Danube from Budapest | through the Iron Gate to the Black Sea; † within 15 km of the gate, ocean within 150 km of the delta | 0.5 km from the gate; ends at (45.16, 29.23), 37 km from the delta | 0.3 km from the cited gate; ends at (45.16, 29.23), 42 km from the cited mouth | PASS |
| Jhelum from Srinagar | out of the Kashmir valley at Baramulla; † within 15 km of it, then the sea within 150 km of the Indus mouth | 4.0 km from Baramulla; the sea 144 km from the Indus mouth | 1.1 km from the cited Baramulla; through one lake, then the Indus to the sea at (23.96, 67.49), 7 km from the cited mouth | PASS |
| Indus from Skardu | its sea; † ocean within 150 km of its mouth | ends at (24.16, 68.80), 144 km off | ends at (23.96, 67.49), 7 km from the cited mouth | PASS |
| Amazon from Iquitos | its sea; † ocean within 150 km of its mouth, 160 km for the Amazon (owner, 2026-11-26) | through one floodplain lake; ends at (−0.54, −50.76), 140 km from (−0.5, −49.5) | through no lake; ends at (−0.25, −51.07), 152 km from the cited North Channel mouth (177 km from the old point) | PASS |
| Arid basins | no lake ≥ 1,000 km² where the basin averages < 250 mm/yr, in either network († basin = lake plus upstream river nodes) | game 0 of 3 (750, 1,942 and 386 mm/yr); reference 0 of 2 | game 0 of 3 (750, 1,942 and 378 mm/yr); reference 0 of 2 | PASS |

`TODAY_RIVER_BURN_M` = 10 m was enough: the lower Indus now follows the reference by Sehwan, so
20 m was not tried.

SVGs (reference blue, game red, equirectangular, 100 km scale bar), from this run:

- [hydro-check-indus.svg](hydro-check-indus.svg)
- [hydro-check-amazon.svg](hydro-check-amazon.svg)

### Outlet points (owner, 2026-11-26: "relocate all mouth points based on cited sources")

Every start, via and mouth point of the outlet bars is now an English Wikipedia article's
coordinates, read 2026-11-26 (the article's own coordinates, or its infobox's mouth). The
tolerances are unchanged: start 30 km, via 15 km, mouth 150 km.

| Point | Old | New | Source |
|---|---|---|---|
| Danube start, Budapest | (47.50, 19.05) | (47.49250, 19.05139) | "Budapest" |
| Danube via, the Iron Gate | (44.67, 22.5) | (44.67111, 22.52972) | "Iron Gates" |
| Danube mouth | (45.2, 29.7) | (45.21750, 29.76139) | "Danube", infobox mouth (Danube Delta) |
| Jhelum start, Srinagar | (34.08, 74.80) | (34.09000, 74.79000) | "Srinagar" |
| Jhelum via, Baramulla | (34.2, 74.35) | (34.20869, 74.34350) | "Baramulla" |
| Jhelum and Indus mouth | (24.0, 67.4) | (23.99500, 67.43500) | "Indus River", infobox mouth (Indus River Delta) |
| Indus start, Skardu | (35.3, 75.6) | (35.29028, 75.64444) | "Skardu" |
| Amazon start, Iquitos | (−3.75, −73.25) | (−3.750, −73.250) | "Iquitos" (unchanged) |
| Amazon mouth | (−0.5, −49.5) | (0.70778, −50.08944) | "Amazon River", infobox mouth: the North Channel, north of Marajó past Macapá |

Margins on this run: the Danube passes 0.3 km from the gate (14.7 km margin) and ends 42 km
from its mouth (108 km); the Jhelum passes 1.1 km from Baramulla (13.9 km) and ends 7 km from
the Indus mouth (143 km), as does the Indus; the Amazon ends 152 km from its mouth (8 km under
its own 160 km allowance).

### The Amazon's outlet (owner, 2026-11-26: "Raise the Amazon's allowance to 160 km")

Natural Earth's Amazonas course ends at Gurupá (−1.58, −52.71), above the estuary; the delta's
channels are not among the rank ≤ 7 rivers. The burnt course carries the Amazon to Gurupá, and the
lattice routes the last 200 km or so: east, then north to the coast by Macapá at (−0.25, −51.07).
That is 152 km from the cited North Channel mouth (0.71, −50.09), 2 km over the 150 km allowed,
and 177 km from the old point (−0.5, −49.5); before the burn the lattice reached the coast at
(−0.54, −50.76), 140 km from the old point. Nothing was tuned for it. The owner raised the
Amazon's own allowance to 160 km; every other outlet keeps 150 km.

### What the Indus box misses

The check prints each unmatched reference link. After fix round 2 (11 links: the 10 of fix round 1,
all missed before the burn too, and the 2 km reach at Muzaffarabad):

| Reference link | Length | Within 10 km | Why |
|---|---|---|---|
| Shyok, (33.94, 78.44) to Skardu (35.23, 75.91) | 332 km | 20% | Not in Natural Earth 1:10m; the lattice takes another valley (unchanged). |
| Arghandab, (33.06, 68.16) to (31.46, 66.01) | 317 km | 0% | Not in Natural Earth 1:10m; a parallel valley up to 56 km off (unchanged). |
| Beas, Pong (31.96, 75.94) to Harike (31.18, 75.09) | 162 km | 18% | Not among the rank ≤ 7 courses. |
| Sindh (Madhya Pradesh), (25.77, 78.29) to (26.44, 79.22) | 151 km | 13% | Not among the rank ≤ 7 courses. |
| Thar's edge to the Rann, (23.59, 71.09) to (24.56, 71.04) | 140 km | 22% | Not among the rank ≤ 7 courses. |
| 5 more | 5–87 km | 0–40% | Ladakh's valleys, the Swat, two short reaches. |
| Jhelum at Muzaffarabad, (34.34, 73.46) to (34.33, 73.47) | 2 km | 33% | Matched before fix round 2 (and before the burn): anchoring the hairpin to Natural Earth's line moved the game's river up to 10.9 km from the reference's confluence reach. |

Now matched: the lower Indus (939 km, 18% before), the lower Jhelum from Mangla to the Chenab
(329 km, 34%), a 178 km link at (36.99, 72.38) to (37.49, 71.51) (7%) and a 2 km reach at
Muzaffarabad (that one missed again after fix round 2's anchors). The first run of the burn
missed the Jhelum from Muzaffarabad to Mangla and a reach of the Chambal, until course nodes kept
their hairpins (see "Choices made in Task 11b").

### Before Task 11b

Task 11's longest misses were the lower Indus, (29.11, 70.69) to the sea (939 km, 18%: about 90 km
east of Sehwan, straight down the Sindh plain), the Shyok (332 km, 20%), the lower Jhelum from
Mangla to the Chenab (329 km, 34%: another line across the Punjab doab) and the Arghandab (317 km,
0%), plus 10 more of 2–178 km.

Before Task 11's Upper Indus gorge, the box read 74.3% with 69/84 links. Then the Indus's
Tibetan headwaters (32.5, 80.8) flowed east through three Tibetan lakes to the Bay of Bengal
(15.98, 95.65).

## Choices made in Task 11

The brief gives the box, lake and arid bars' numbers. The choices below were made while building
the check, and the controller has ratified them (2026-11-26).

- **Outlet tolerances** (`hydro_check.rs`): the river starts at the largest within
  `OUTLET_START_KM` = 30 km of its point. It must pass within `OUTLET_VIA_KM` = 15 km of its
  gorge, about one and a half lattice spacings. It must end in the ocean within `OUTLET_SEA_KM` =
  150 km of its mouth. The brief only says "reach their real outlets".
  - Why 150 km: the misroutes this bar exists for end in the wrong sea, 1,000 km or more off. The
    Danube reached the Oder mouth, 1,469 km off; the Indus headwaters reached the Bay of Bengal,
    over 3,000 km off. Deltas and estuaries are wide: the Amazon's is about 300 km across.
  - Margins on Task 11's run, against the old points (Task 11b's, against the cited points, are
    under "Outlet points"):
    - Danube: 0.5 km from the gate (14.5 km margin), 37 km from the delta (113 km).
    - Jhelum: 4.0 km from Baramulla (11 km margin), 144 km from the Indus mouth (6 km).
    - Indus: 144 km from the mouth (6 km margin).
    - Amazon: 140 km from the mouth (10 km margin).
  - In Task 11's run the Indus (and the Jhelum, which follows it) ended 144 km east of (24.0,
    67.4), at (24.16, 68.80) by Kori Creek: the same Sindh-plain route error the Indus box
    failed on. Task 11b's burn fixed both.
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

## Choices made in Task 11b

The owner chose to burn Natural Earth's courses (2026-11-26); the brief fixed the source, the rank
cut, the 1 km simplification, the 2′ orientation window and `TODAY_RIVER_BURN_M` = 10 m. The
choices below were made while building it.

- **The burn.** Wherever the era's terrain is today's (data lakes, and the node's present-day
  shift known: the gorges' gate), after the straits, each course's nodes (upstream first) take a
  level, the running minimum of the era's node elevations along it (after the straits' burn). The
  node routes at `level − 10 m`. The routing elevation of a node at or above sea level is never
  burnt below 0 m, so the burn never adds a node to the ocean; the river graph's bed there is the
  level, itself never below 0 m on such a node. Ice nodes are not burnt. A node on several courses
  takes the lowest level. Reference (PaleoDEM) eras burn nothing.
- **Routing only.** Only the river graph's beds take the courses' levels, so the regions carve no
  10 m trench. Routing, the flats, the climate and the lakes all read the burnt surface. (Fix
  round 1: the first version raised the burnt nodes back to their levels after routing, before
  the lakes read them. A basin could then hold nodes above its own spill, and a course ending
  inland on a 3 m ridge in a semi-arid past era made a lake at 50.4 m over a 45.9 m spill.
  `a_course_ending_inland_keeps_its_lakes_within_their_basins` pins it.)
- **Course nodes are taut.** As the straits, a course's nodes are the nearest to points a quarter
  spacing apart. A return to an earlier node cuts the loop, and an L-shaped corner is cut: when the
  new node is a diagonal neighbour of the node before the last, the last is dropped. Without that,
  steepest descent took the diagonal past a corner node and left the course (a test shows it). A
  V-shaped turn (two diagonal steps whose ends are side by side) keeps its apex: the first run cut
  every corner, which dropped the Jhelum's hairpin at Muzaffarabad. The valley there is lower than
  a 10 m burn of the course's other nodes, so the river left the course westward and the box
  missed the 136 km link to Mangla (and an 82 km reach of the Chambal). Keeping V apexes gave the
  table above (80.7% and 72/84 links before, 81.4% and 74/84 after).
- **Every era with today's terrain**, not 0 ka only (the brief's ruling): an ice-age river follows
  today's course where it may then have run elsewhere.
- **Time-boxed.** The table is read a course at a time, each course's nodes found in blocks, and
  the burnt nodes lowered in blocks (stage `today_rivers`; fix round 1 removed the `beds` stage:
  the graph build reads the beds). Nothing
  is cached between passes, so every pass does the same units. On the synthetic recent lattice
  (`time_the_today_rivers_stage`, dev build) the stage takes 45 steps of 8,192 units, the longest
  0.53 ms. On the real bake (`real_continental_pass_timing`, dev build; release was not built)
  the time-boxed runs give the same result as `run_parallel`, bit for bit, at 0 and 21 ka. Their
  longest steps are 1.39 ms (walk→rivers) and 1.64 ms (runoff→walk) at 0 ka, at 6 and 12 ms
  budgets, and 1.52 ms (walk) and 2.03 ms (sample) at 21 ka: none in `today_rivers`.
  The dev build runs the workspace at opt-level 1, so release steps are shorter (Task 10: 1.1–1.3
  ms).
- **Where the courses end.** A course that stops short of the sea or its trunk leaves a pit at its
  end: the water fills it and spills where the lattice says. The Amazon's course ends at Gurupá,
  above the estuary (see the Result).
- **How far the beds lie below the terrain** (fix round 1, measured, not changed). The check
  prints it (informative): over 30,799 burnt nodes at 0 ka the era's node elevation stands above
  the bed by at most 1,304 m, 373 m at the 99th percentile, and over 50 m at 3,693 nodes. The
  worst five are mountain canyons:

  | Course | Node at | Node terrain | Bed | ETOPO 60″ lowest within about 6′ |
  |---|---|---|---|---|
  | Columbia (Hells Canyon) | (45.28, −116.59) | 1,979 m | 676 m | 610 m |
  | Indus | (35.75, 74.89) | 3,718 m | 2,484 m | 1,967 m |
  | Indus | (35.55, 75.25) | 3,647 m | 2,484 m | 2,056 m |
  | Jinsha | (27.66, 103.44) | 2,357 m | 1,328 m | 601 m |
  | Indus | (32.58, 79.49) | 5,231 m | 4,206 m | 4,231 m |

  In four of the five the bed stands above the lowest 60″ cell near the node, and in the fifth
  it is 25 m below it. The depth is mostly the 10 km node's mean over canyon walls, not a trench
  below the real river. 22 nodes on 9 courses are pinned at 0 m after their course passed a node
  below sea level (the Danube's Sulina arm, the Lek and the Waal, the Lena, the Sacramento, the
  Suez Canal, the Sénégal, the Volga and one unnamed course).

  After fix round 2's sloped beds: at most 1,362 m, 439 m at the 99th percentile, and over 50 m
  at 4,446 nodes. The beds now fall across held stretches, below the held level.

  A `region_tour` render of the worst node (Hells Canyon, 45.28° N, 116.59° W, 0 ka, 6,000 m
  above the ground, looking north and 45° down;
  [hells-canyon-burn-down.jpg](hells-canyon-burn-down.jpg)) shows the following:
  - South of the camera the river runs as a narrow, straight, uniformly dark-brown channel along
    the canyon floor, with no water drawn in it.
  - North of the camera the river is blue and meanders.
  - To the north-east a river crosses a ridge in a narrow cut whose floor reads black: a pit or a
    very steep notch.

  No render without the burn was made to compare, and the rule was not changed: the owner
  decides.
- **Anchors and sloped beds** (fix round 2, after the Hells Canyon investigation).
  - Each burnt node records its anchor: the nearest point to its centre on the course's polyline
    segments that sampled it. It is held within `TODAY_RIVER_ANCHOR_MAX_SPACINGS` = 0.75 mean
    spacings, and the regions relax the node there instead of searching the coarse floor.
  - The anchors are pass data, stored with the river nodes (`EraHydrology::anchor`), so regions
    still agree. The gather's relaxation reach grows from C(1 + √2) to max(C√2, 0.75 spacings) +
    C.
  - Across a held stretch (a run of nodes above the running minimum), the river graph's bed now
    falls linearly by path length to the next lower level. Routing keeps the plain running
    minimum.
  - On the real bake at Hells Canyon:
    - The refined channel now follows the canyon instead of the chord over the plateau.
    - The ground stands 70–324 m above the water along the stretch, not 1,100–1,500 m.
    - The water runs at 1.09 m/s, not 0.30.
    - The ridge notch to the north-east remains: a capture of the eastern river into the course.
      It is cut 826–1,146 m deep and 375–875 m wide. It goes to the owner.
- **The reference does not burn** (the brief's ruling), so the check compares two independent
  sources. The fixtures under `crates/timefly/tests/fixtures/hydro_reference` are unchanged.

## Gorges added to `data/hydrology/straits.json`

The table now holds straits (a sill) and gorges (a river surface per point, interpolated between
points). Both the pass and the Python reference read it.

| Entry | Why | Source |
|---|---|---|
| Iron Gate (Golubac to below the Iron Gate I dam, 66 m to 41 m) | Without it, the Pannonian basin spilled through the Moravian Gate. The Danube reached the Baltic by the Oder at (53.79, 14.14), 1,469 km from its delta. | Wikipedia, "Iron Gates" (read 2026-11-26), for the gorges and the dam. Course and levels from ETOPO 2022 60″ (doi:10.25921/fd45-gt74). |
| Danube Bend (Esztergom to Budapest, 105 m to 101.5 m) | Found by this check: without it, the Danube left the Little Hungarian Plain past Lake Balaton and missed Budapest. | Karátson (2015), *Danube Bend: Miocene Half-Caldera and Pleistocene Gorge*, in Lóczy (ed.), *Landscapes and Landforms of Hungary*, Springer, doi:10.1007/978-3-319-08997-3_16. Levels from ETOPO 2022. |
| Upper Indus (32.49, 80.81) to Nimo (34.18, 77.33), 4,495 m to 3,210 m | Without it, the Indus's headwaters went to the Bay of Bengal. | Kumar & Srivastava (2017), *Quaternary Research* 87(3), 363–385, doi:10.1017/qua.2017.19. Course and levels from the 1′ reference's own Indus link. |
| Bosporus (−35 m), now with its sill cited | — | Gregg & Özsoy (2002), *J. Geophys. Res.* 107(C3), 3016, doi:10.1029/2000JC000485, ¶52: South Sill is mostly 30–40 m deep. |
| Victoria Nile (Jinja to Lake Kyoga, 1,134.6 m to 1,030.1 m; Task 11d) | Without it, Lake Victoria drained 80 km east of Jinja and no river passed Jinja. | Wikipedia, "Victoria Nile" (read 2026-11-26). Course from Natural Earth's Victoria Nile; levels from ETOPO 2022. |
| Krasnoyarsk Reservoir (the Minusinsk basin to Natural Earth's river below, 241 m to 232 m; Task 11d) | Without it, the Yenisei's upper basin spilled west to the Chulym and reached the sea by the Ob. | Wikipedia, "Krasnoyarsk Reservoir" (read 2026-11-26). Course from Natural Earth's lake centreline; levels from ETOPO 2022. |

The Danube now passes Budapest and the Iron Gate and reaches the Black Sea. Between them, it
still crosses the Great Hungarian Plain by Szeged rather than by Mohács and Novi Sad. That is the
lattice's routing on a flat plain, not a gorge, so it is not fixed here.

## Constants

Task 11d tuned none: the tolerances, `TODAY_RIVER_BURN_M` and the rank cut are unchanged.


No constants were tuned. The misses come from route choice, so the corridor, discharge threshold
and runoff table cannot change them. Task 11b adds `TODAY_RIVER_BURN_M` = 10 m (the brief's value; 20 m
was not needed) and `TODAY_RIVER_MAX_SCALERANK` = 7 in the bake. The hydro golden
fixture, the reference fixtures and the region golden hashes are unchanged: the full workspace
tests pass.
