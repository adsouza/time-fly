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

### Fix round 5 (2026-11-26)

**1. The Aral's lost inflow (863 → 239 m³/s) is a reroute inside the Aral basin, not a loss.** The water reaches the same dry sink as before; the probe traces below come from temporary probes, not committed, at round 3 (1216a2e) and now.
- **The Amu Darya is unchanged.** Both times it runs down Natural Earth's western arm (part 267) to a dry sink at (44.42, 58.27), on the West Aral's southern shore. That sink takes 1,992 m³/s in round 3 and 1,989 m³/s now.
- **The Syr Darya changed route.** In round 3 it crossed the dry Aral bed westward and entered lake 857 (the West Aral) at (45.24, 58.75) with 861 m³/s. The lake's outflow, 794 m³/s, then went to the same sink. Now it runs south down the bed's eastern side, from (45.48, 59.81) to (44.35, 59.23), and reaches the sink without passing the lake.
- **Lake 857's own balance is unchanged:** it evaporates 69 m³/s before and 68 m³/s after. The Caspian's surplus is untouched, because the water never leaves the Aral basin.
- **No part is misoriented.** Part 216, the Syr, runs downstream to (46.02, 61.76), and parts 267 and 268, the Amu, run north from Kungrad. On the dry bed the Syr's nodes are `BasinRoute` exceptions: the present-day rewrite of the closed Aral basin routes each node one BFS step toward its sink, and there the successor is not closer. Ruling 1's holds and its chain shift which neighbour that rewrite picks among equally close ones (lowest elevation first).
- **Open items:**
  - The Syr Darya should end in the North Aral (lake 859, 133 m³/s). Its course stops 10 km short of part 215, which enters that lake.
  - The Amu Darya ends in a dry sink beside lake 857 rather than in it. The burn leaves the course's last node 10 m below the lake's 27 m surface, so it is a pit of its own.
  - Both were there in round 3. A proposed rule, not implemented: a course node within reach of a mapped lake routes no lower than the lake's surface, as a coastal node routes no lower than 0 m.

**2. The lake map audit.** A new informative line, which never fails the check, lists every lake-map lake whose baked surface stands more than 20 m (`LAKE_MAP_GAP_M`) below the lowest cell of its shoreline ring.
- **The ring** is the 3′ cells outside every lake that touch the footprint (`LakeMap::rings`, unit-tested on a seam-crossing map). The ring is sampled on the 5′ overview.
- **The line** gives each flagged lake's position, cell count, baked surface, ring lowest and median, the gap, and the pass's lakes whose exit lies in it. It then gives a fixed list of named lakes, flagged or not.
- **149 of 1,059 lakes are flagged.** Most are small mountain lakes (Tibet, the Alps, New Zealand, the Altai), where the 5′ overview smooths steep shores far above the water. Geneva, for example, is baked at 368 m, close to its real 372 m; its ring's lowest cell reads 413.7 m.
- **The named lakes:**

| Lake | Baked | Ring lowest (median) | Gap | Real surface | Verdict |
|---|---|---|---|---|---|
| Baikal | −328.0 m | −49.8 (552.9) | 278 m | 455.5 (table) | **bed: FLAGGED** |
| Superior | 24.1 m | 122.6 (230.3) | 98 m | 183 (table) | **bed: FLAGGED** |
| Michigan | 96.4 m | 142.7 (189.9) | 46 m | 176 (table) | **bed: FLAGGED** |
| Huron | 124.7 m | 151.0 (191.5) | 26 m | 176 (table) | **bed: FLAGGED** |
| Ontario | −4.4 m | 44.7 (93.8) | 49 m | 74 (table) | **bed: FLAGGED** |
| Erie | 155.0 m | 155.1 (187.3) | 0 m | 173 (table) | ok by the bar, but 18 m below its table level: likely a bed too (Erie is shallow) |
| Tanganyika | 770.0 m | 778.8 (969.0) | 9 m | | ok |
| Malawi | 474.5 m | 475.1 (564.4) | 1 m | | ok |
| Great Slave | 155.0 m | 154.7 (172.2) | 0 m | | ok |
| Great Bear | 155.5 m | 156.5 (196.5) | 1 m | | ok |
| Ladoga | 3.5 m | 4.7 (16.0) | 1 m | | ok |
| Issyk-Kul | 1606.5 m | 1611.7 (1771.1) | 5 m | | ok |

- **The Baikal-type errors are the lakes ETOPO 2022 carries bathymetry for:** Baikal and the Great Lakes. The map's median of the 60″ cells inside them is their bed. The level table already overrides all six for the table lakes. But the pass also makes a **Data lake from the map's footprint at the baked bed level**:
  - Baikal's is lake 384 (−328.0 m).
  - Ontario's is lake 15 (−4.4 m).
  - Superior's is lake 375 (24.1 m). Round 4 mislabelled it as the Lake Nipigon area.
  - Michigan's is lake 376 (96.4 m), and Huron's lake 377 (124.7 m).
- **Lake 384 is a fragment of Baikal's map footprint.** Its exit lies in the same map lake (id at (53.42, 107.70)) as the table Baikal (lake 0). It holds the footprint nodes outside the table lake's basin: 1,248 km² against the table lake's 33,137.
- **Other flagged lakes to check when the bake is fixed:**
  - (53.46, 139.77), −106.4 m, near the Amur's mouth.
  - Lakes the bar flags that are not baked at a bed: Assal (−153.5 m, real −155) and the Sea of Galilee (−214.5 m, real about −212), both baked about right, and Loch Ness (28.1 m, real 16 m), baked above its real surface. In each, the overview smooths a steep shore.
- The shipped map was not re-baked, as ruled.

**3. The Ganges delta's parts.**
- **Evidence for the two parts.** Part 1099 runs (23.191, 90.213) → (23.579, 90.087) and part 1101 runs (22.775, 90.394) → (23.191, 90.213): both upstream, away from the sea.
  - In the bake, 1099 was oriented by ETOPO: its ends read 3.1 m and 2.0 m, metre-level delta noise. 1101 then followed it as a chain (1.1 m), overruling its own correct 1.0 m fall.
  - Wikipedia "Padma River" (read 2026-11-26) says the river flows "generally southeast" to its Meghna confluence, at the article's point (23.267, 90.600). 1099's southern end lies 40 km from that point, its northern end 63 km.
- **New: cited orientations** (`data/hydrology/rivers_orient.json`, `read_orientations`, `cited_orientations`). One entry decides 1099 as certain, and 1101 follows by chain.
- **New: closed rings are dropped** (`is_ring`: three or more points, ends within 1 km). Part 1100, the ring at (23.191, 90.213), is gone. So are the Iriri's ring and the Indus delta's ring, and two tiny loops that simplified to 2-point stubs (the Ottawa and the Kotuy).
- **Re-bake.** The command is the brief's, unchanged: 963,951 bytes and 1,423 courses (1,428 before). The orientation counts are topology 606, ETOPO 819, cited 1. Besides the dropped rings, two Ganges parts were reversed by the citation, and one 350 m Indus link by the ring's removal.
- **Ganges and Brahmaputra:** 133 → **95 km** from their mouth, ending at (22.83, 90.70) on the Meghna estuary. Pictures: [Ganges](hydro-check-outlet-ganges.svg) ([round 3](hydro-check-outlet-ganges-before.svg)).

**Tests.**
- Python, each RED first (scratch `task11d-r5/red-py.txt`), then GREEN:
  - `test_select_drops_closed_rings`;
  - `test_a_cited_orientation_decides_a_part_and_its_chain` (it asserts the hazard);
  - `test_read_orientations_refuses_an_uncited_or_unmatched_entry`;
  - `test_the_shipped_orientations_are_cited_and_match_the_table`.
- Rust, written with the code:
  - `a_lakes_ring_is_the_land_around_its_footprint`;
  - `the_lake_map_audit_lists_lakes_baked_below_their_shores`.

**hydro check (`dev`): FAIL, on the Indus box's links only. All 22 outlets pass.** Before is round 4 (0b1ae8c); after is this round.
- **Changed:**
  - Ganges and Brahmaputra 133 → 95 km.
  - Burnt nodes 31,986 → 31,985.
  - Course order: Follows 28,701 → 28,704; SharedNode 91 → 92; NoRiver 2,666 → 2,663; BasinRoute 95 → 93; Other 3, the same loop cuts.
  - Pass 1.74 → 1.65 s. Nothing else was building this time, but the machine was not otherwise quiet.
- **Unchanged, every other bar:**
  - the Indus box (83.0%, 74/84, worst 81.3 km) and the Amazon box (91.8%, 457/482);
  - the five lakes and the arid bar;
  - the other 20 outlets, digit for digit;
  - all 18 discharge gauges (the Nile 5.11 alone flagged);
  - the Caspian's surplus (3,449 of 12,853 m³/s);
  - pinned, 19 nodes on 10 courses;
  - p99 439 m, and 4,611 nodes over 50 m.
- **The lake dump:** all 861 lakes identical to round 4.

### Fix round 4 (2026-11-26)

The controller's rulings, and what was built:

- **1. Course nodes drain to their course successors.**
  - **Mechanism: held receivers.** Routing (`Routing::with_courses`) *holds* each burnt course's interior node to its successor, after the corridor and before the fill. A node is held when it is land, its successor is a lattice neighbour, and the successor is land or a course's end. The flood reaches a held node from its successor alone, so its parent and its receiver are the successor. Its filled level is `max(e, filled(successor))`: it never rises along a receiver, and pop order stays a topological order.
  - **Why not a gutter.** A gutter below the marsh would have to stand below every neighbour's filled level along every course, and below its outlet too, or the gutter itself becomes a basin. Holding needs no elevation change, so the ocean labelling and the 0 m floor are untouched.
  - **Routing elevations over all courses at once** (`BurntNodes::chain_step`). Each interior node keeps one successor. From the ends upstream along the successors, the land chain is `max(level − 10 m, 0 m, chain(successor) + 0.01 m)`, with `level − 10 m` the lowest any course gave the node. A held node therefore stands above its successor, so held nodes form no basin of their own. Where several courses pass a node, the per-course chains disagreed at junctions; this chain cannot.
  - **The rule at shared nodes** (deterministic, documented in `BurntNodes`). The successor is the next node of the course whose own chain is lowest there. That chain is `max(level − 10, 0)`, at least 0.01 m above the next node's, counted from that course's own end, as each course routed alone in round 2. Ties go to the first course in the table. At a fork, or where a distributary leaves a stem, this picks the branch that steepest descent picked before this round. A successor that would close a loop (misoriented parts) is dropped.
  - Two rules were tried first, and the evidence is below. Through-courses before starting courses: the Orinoco went down the Caño Mánamo and the Nile down the Rosetta branch. The lowest chain over the network: the Orinoco went down the Mánamo, and the Lena moved 18 km off.
  - **Basins.** A basin is an 8-connected set of depressed nodes *at one filled level*. Without held nodes, neighbouring depressed nodes always share one level (the old debug assertion). A held node can stand filled to its successor's lake beside a lower pit.
  - **Basin rewrites.** At the present day every basin's receivers are rewritten toward its lake or sink, one BFS step closer each. That rewrite keeps a held node's successor where the successor is a step closer. Elsewhere it overrides the hold, and such nodes are listed (`EraHydrology::basin_routed`).
- **2. The Nile's mouth is Ras El Bar,** (31.5125, 31.82556), from Wikipedia "Ras El Bar" (31°30′45″N 31°49′32″E, read 2026-11-26). The article calls it "at the mouth of the Damietta Nile branch". The allowance stays at 150 km.
- **3. A course's level never reads a lake bed.** A course node reads its height as `max(sample, surface)` when it, or a lattice neighbour within 1.5 mean spacings (one spacing from that node's footprint), is a mapped-lake node (`lake_surface_near`). A course's first node does the same when its first point lies in a mapped lake. That reach alone did not move the Angara, because the lake map's baked surface for Baikal is −328 m (its bed; a probe printed it). So a lake of the level table is read at its table level (`table_surfaces`; Baikal 455.5 m), and the map's surface only otherwise.
- **The units fix.** `Today::step` charged its Done check only when budget was left after the last flow step, so the pass's units differed by one between budgets 1/7 and 2/64/2¹⁸. The bug was already there: at HEAD, budget 1 gave 729,090 and the others 729,091. Done now costs nothing.
- **The check gains two lines.** One lists every pinned course with its first node. The other, `today's courses' order`, sorts each interior node by how it drains against its successor (`course_steps`, `CourseStep`).

**Tests (RED at HEAD with the tests and their helpers added, then GREEN):**
- `a_delta_course_beside_a_marsh_reaches_its_own_end`: a 0–0.2 m plain, marsh at 0.05 m and a lagoon open to the sea. RED: "RiverNode 397076 … does not drain to 397376".
- `the_shipped_courses_drain_to_their_successors`: the shipped table over Synthetic terrain at n = 150, at two sea levels. RED: `Other: 661`. GREEN: no `Other`. Exceptions at offset 300: `SharedNode` 31, `Ocean` 2,034, `PastSeaEnd` 2, `NotBurnt` 1, `NoRiver` 667, `BasinRoute` 114, `Sink` 3.
- `the_time_boxed_pass_matches_run_parallel_on_a_delta`: three courses with an estuary corridor, a tributary and a distributary, at budgets 1, 7 and 64 and sampled in parallel. RED at HEAD on its hazard (`Other` steps along the course). GREEN, with equal units.
- `a_course_leaving_a_deep_lake_starts_at_its_surface`: a lake bed at −300 m, a shore node off the map at −50 m, and the map's surface at the bed with a table level of 120 m. RED at HEAD: 13 pinned beds. GREEN; without the table entry it still pins, which proves the table rule is engaged.
- `a_course_node_reads_a_lake_within_reach_at_its_surface` (unit). `the_nile_ends_at_the_damietta_mouth`, RED: `left: (30.1725, 31.14)`.

**hydro check (`dev`): FAIL, on the Indus box's links only, as before. All 22 outlets pass.** Before is HEAD 1216a2e with the two new report lines; after is this round.

| Bar | Before | After |
|---|---|---|
| Indus box, relaxed | 81.3%, 73/84 links, worst 77.5 km | 83.0%, 74/84, worst **81.3 km** (FAIL, links) |
| Indus box, coarse | 80.1%, 72/84, worst 77.7 km | 82.0%, 73/84, worst **82.4 km** |
| Amazon box, relaxed | 91.6%, 458/482 | 91.8%, **457**/482 (PASS) |
| Amazon box, coarse | 91.3%, 455/482 | 91.4%, **454**/482 |
| The five lakes | exact | exact |
| Arid basins | 0 of 3, 0 of 2 | the same (lake 537 at 1940 mm/yr, was 1942) |
| Danube | 0.3 km; 7 km | the same |
| Jhelum | 1.1 km; 8 km (1 lake) | the same (2 lakes) |
| Indus | 8 km | 8 km |
| Amazon | 152 km | 152 km |
| Ganges | 0.1, 1.2 km; 110 km | 0.1, 1.2 km; **133 km** |
| Brahmaputra | 5.7, 2.7 km; 110 km | **6.4**, 2.7 km; **133 km** |
| Yangtze | 1.3, 0.0, 3.4 km; 121 km | the same vias; 6 km |
| Yellow River | 0.4, 1.4 km; 100 km | the same vias; 13 km |
| Pearl | 4.4 km; 31 km | 2.7 km; 31 km |
| Mekong | 7.6 km; 103 km | 7.6 km; 103 km |
| **Nile** | 1.8, 5.6 km; 161 km from the delta's head, FAIL | 1.8, 5.6 km; **3 km from Ras El Bar, PASS** |
| Congo | 2.0, 3.2 km; 17 km | the same |
| Niger | 10.3, 6.7, 8.6 km; 124 km | **10.9**, 6.7, 3.9 km; 124 km |
| **Mississippi** | 0.6 km; New Orleans 37.2 km, FAIL; 141 km | 0.6 km; **New Orleans 7.6 km, PASS**; 21 km |
| Hudson | 3.8 km; 26 km | the same |
| Yenisei | 1.8 km; 90 km | the same |
| Ob | 9.1 km; 87 km | the same |
| Orinoco | 0.1 km; 14 km | the same |
| Paraná | 3.1 km; 9 km | the same |
| Lena | 0.4 km; 117 km | the same |
| Volga | 13.0 km; 70 km | 3.8 km; 70 km |
| Mackenzie | 1.9 km; 9 km | 1.9 km; **84 km** |
| Discharge | Nile 5.11 flagged; 18 gauges | Nile 5.11 flagged. Moved: Mekong 0.59 → 0.75, Mississippi 1.71 → 1.76, Niger 1.31 → 1.28, Orinoco 1.00 → 0.97, Mackenzie 1.18 → 1.14, Yangtze 0.74 (16,655 → 16,733 m³/s), Pearl 0.77 → 0.76, Yellow 1.70 → 1.69; the rest within 1% |
| Endorheic surplus | the Caspian 3,449 of 12,853 m³/s | 3,449 of 12,853 |
| Burnt nodes | 31,986; p99 446 m; 4,621 over 50 m | 31,986; p99 439 m; 4,611 over 50 m |
| Pinned at 0 m | 32 nodes on 11 courses, the Angara 13 | **19 nodes on 10 courses: the Angara is gone** |
| Course order | Follows 24,111; Other 2,259 | Follows 28,701; Other 3 (below) |
| Pass at 0 ka | 1.31 s | 1.36–1.74 s over four runs, with another implementer building on the machine; not measured quiet |
| River nodes | 118,983 | 120,052; 861 lakes |

**Worse, for the controller (unchanged):**
- Ganges and Brahmaputra 110 → 133 km.
- Mackenzie 9 → 84 km.
- Brahmaputra's Pasighat via 5.7 → 6.4 km, and the Niger's inner-delta via 10.3 → 10.9 km.
- The Amazon box one link fewer (458 → 457 relaxed, 455 → 454 coarse).
- The Indus box's worst distance 77.5 → 81.3 km; its share and links improved.

All still pass. Pictures: [Ganges](hydro-check-outlet-ganges.svg) ([before](hydro-check-outlet-ganges-before.svg)) and [Mackenzie](hydro-check-outlet-mackenzie.svg) ([before](hydro-check-outlet-mackenzie-before.svg)).

**The shared-node rule, three variants on real data:**

| Rule | Orinoco | Nile | Lena | Mekong | Mississippi |
|---|---|---|---|---|---|
| Through before starting, then lowest `level − 10` | **272 km FAIL** (the Caño Mánamo, table order at the fork) | 131 km, down the Rosetta | 117 km | 19 km | 32 km |
| Lowest chain over the network | **272 km FAIL** (the Mánamo is the shortest way to a sea) | 3 km, Damietta | 135 km | 19 km | 21 km |
| **Shipped: the course's own chain** | 14 km | 3 km, Damietta | 117 km | 103 km | 21 km |

**The course order on real data** (`course_steps`, 31,634 interior nodes):
- Follows 28,701.
- SharedNode 91: drains to another course's successor at a node they share.
- Ocean 5: past the corridor's open-sea end.
- InLake 73.
- NoRiver 2,666: too little water to be a river node, so the river graph cannot show it. Held in routing all the same.
- BasinRoute 95: the present-day basin rewrite, where the successor is no step closer.
- **Other 3**, each a loop cut at misoriented parts:
  - the Lena delta's Tumatskaya and Bykovskaya Protoka at (72.45, 126.43), node 1949064. The Bykovskaya part runs from the sea into the delta's apex.
  - the Bell's lake-chain parts at (48.50, −75.53), node 2114164, which round 3 listed as unverified.

**The pinned courses still listed** (19 nodes, each at most 4.2 m above the sea). In every case the course passes an ETOPO channel or polder floor below 0 m, then a delta or estuary node from 0.0 to 4.2 m. The 0 m floor would give that node 0 m in any case:

| Course | Where | What lies below sea level upstream |
|---|---|---|
| Bratul Sulina | (45.19, 29.40), 2 nodes, 1.2 m | the Danube delta |
| Lek | (51.92, 4.47), 1 node, 0.0 m | the Rhine delta's polders |
| Waal | (51.75, 4.77), 1 node, 1.1 m | the Rhine delta's polders |
| Lena | (72.22, 126.83), 2 nodes, 1.1 m | delta channels |
| Rio Grande (Orinoco) | (8.63, −60.41), 1 node, 0.2 m | at Barima Point |
| Sacramento | (38.16, −121.66), 1 node, 0.3 m | the delta's islands |
| Sénégal | (15.76, −16.52), 1 node, 4.2 m | Saint-Louis's channel |
| Volga | (48.70, 44.59), 6 nodes, 3.7 m | at Volgograd, where the course enters the Caspian depression |
| Xingu | (−1.76, −52.32), 2 nodes, 3.7 m | its ria's floor |
| Yangtze estuary | (32.05, 120.63), 2 nodes, 0.8 m | the estuary's floor |

**The lakes, diffed** (`hydro-check-lakes.tsv`):
- 749 of 861 are identical; 112 changed flows, and none changed kind.
- **Baikal** now spills at Listvyanka, (51.79, 104.80), not 160 km down the Angara at (53.11, 103.44): 35,343 → 33,137 km², inflow 3,968 → 3,572 m³/s. Its outflow passes the −328 m data flat beside Listvyanka (lake 384: 25 → 3,597 m³/s in), then the Angara.
- Large moves come where a held course now carries the water another way:

| Lake | Change, m³/s |
|---|---|
| A data lake by Dongting (29.33, 112.95) | 11 → 12,588 (the Yangtze now passes through it) |
| Its neighbour (28.96, 111.97) | 1,965 → 135 |
| Two Athabasca-delta lakes | 101 → 2,082 and 1,997 → 722 |
| The Ordos lake on the Yellow River (40.99, 108.95) | 3 → 1,155 |
| The Aral Sea (44.51, 58.31) | 863 → 239 |
| Flathead | 921 → 354 |
| Winnebago | 417 → 151 |
| Lake Nipigon area (48.89, −88.18) | 644 → 258 |
| Taymyr | 313 → 470 |

  Flathead's and Winnebago's new inflows are nearer their rivers' gauged means (about 330 and 120 m³/s, from memory, not cited). The Aral is unverified.
- 11 exits moved and 2 areas changed.

Pictures, refreshed: [Mississippi](hydro-check-outlet-mississippi.svg) (round 3's [before](hydro-check-outlet-mississippi-before.svg)), [Nile](hydro-check-outlet-nile.svg), [Yangtze](hydro-check-outlet-yangtze.svg), [Volga](hydro-check-outlet-volga.svg), [Danube](hydro-check-outlet-danube.svg), and the two box pictures. No outlet fails, so there are no misroute pictures.

### Fix round 3 (2026-11-26)

The controller's rulings:

- **1. The course corridor.** This is the owner's "corridor now, mask later".
  - A burnt course's interior nodes (every node but its last) that the ocean rule would make sea become land, at their land chain's routing elevation (`Routing::with_corridor`, applied when the ocean is settled, before the fill).
  - No interior course node takes an ocean receiver. That holds in the steepest-descent choice and in `pick_receiver`'s parent fallback. The flood does not grow the ocean into an interior node either; a course end may still take the sea.
  - A below-zero interior node routes on the course's ε chain toward its end, so it never becomes a basin.
  - Polders and coastlines off the courses are unchanged.
- **The guard.** A course's corridor ends at its first interior node that is open sea: ocean by the unmodified rule and sampled below `OPEN_SEA_M` (−10 m). That node ends the course, and the nodes after it follow the ocean rule. Natural Earth lines that run out past the coast build no spit.
  - ETOPO's channel and lagoon floors up estuaries sample −0.2 to −7 m. Open sea is deeper.
- **2. Rule B's junction hole.** Only a node that is no course's interior node is exempt from the inland rule. A course that ends on another's interior node (a tributary's mouth beside a lagoon) no longer opens the through course to the sea.
- **3. River parts are oriented like lake parts** (`orient_by_connections`, one rule set for every part). Each part's candidates are taken surest first, by a priority queue:
  - **ETOPO:** as sure as the ends' height difference. The end heights read a mapped lake's cells as its surface (the table's level for the lakes in `lake_levels.json`), so the Angara no longer reads Baikal's bed at Listvyanka.
  - **A tributary's mouth** (an end on another part's interior vertex): as sure as 1 m (`MOUTH_VOTE_M`).
  - **A junction,** once every other part meeting there is decided:
    - an end where a part starts and none ends flows into it;
    - an end where all the others end is their outflow;
    - a fork or a confluence with a known inflow gives no vote.

    Where two parts meet (a chain), the vote is as sure as the last decision there. Where three or more meet, a part's role is only inferred, so its vote is capped at `MOUTH_VOTE_M`.
  - **Ends within the table's 1 km simplification (`SIMPLIFY_M`) are one junction,** and a part shorter than that joins nothing. Natural Earth leaves streams unjoined by 30 m (the Danube's Sulina arm) to 350 m (the Xun at Guiping).
  - **The hand-added channels in `rivers_extra.json` join as fixed, certain parts.** They are cited upstream first, so they orient the arms that feed them, such as the Mekong's Tiền.

**Fixes beyond the rulings, all in the orientation.**

| Change | Case | Why |
|---|---|---|
| A junction votes only once every other part meeting there is decided. | The Mekong at Phnom Penh. | The Bassac arm was decided first while the stem's short link part was still unknown, so the fork showed an outflow and no inflow, and the Tiền arm counted as a tributary and ran backwards. |
| Junction ends are clustered, and inferred roles are capped (both above). | The Chilia arm and the You/Yong/Yu. | Each was reversed by a junction that looked like a chain: the Chilia flowed in from the Black Sea, and the You/Yong/Yu ran 127 m uphill from Guiping past Nanning to Baise. |

Twenty river parts whose direction is known from geography were used to compare the variants: 12, 11 and 16 right for the intermediate rules, 19 for the rule shipped. The one still wrong is the Lena delta's Olenek channel, where ETOPO's ends differ by 2.8 m of noise. The list was assembled during this work, so it is evidence, not an independent test.

**Orientation counts.** Topology 607, ETOPO 844, ambiguous 0. River parts: 432, 836 and 0. Lake parts: 175, 8 and 0 (round 2: 149, 27 and 7). The baked table reverses 208 Natural Earth parts (216 before). Against round 2's table, 26 parts changed direction:
- Now right: St. Clair (Port Huron to Lake St. Clair), the Albert Nile out of Lake Albert, the Columbia at Pasco, the Niger's delta arm, the Tarim (west to east), the Green through Flaming Gorge, the Angara from Listvyanka, and two Volga parts.
- The Volga's Saratov–Balakovo part now runs downstream, (52.07, 47.77) → (51.74, 46.78). Both its ends read the Saratov reservoir's surface, so ETOPO is flat there and its connections decide.
- Now wrong: one Ganges-delta part near Chandpur, (22.78, 90.39) → (23.19, 90.21). It follows by chain a part ETOPO misorients by 1.1 m on flat ground.
- Unverified: 2- to 5-point lake-chain parts of the Churchill (4), the Albany (2), the Caniapiscau (2) and the Bell (2), and one part each of the Athabasca, the Yuan, the Wujia, the Yangtze and the Indus.

The table is 964,421 bytes (unchanged). Run: `dev` profile. Pass time at 0 ka 1.45 s (0.20 s sampling); 118,983 river nodes (118,963 before the round), 861 lakes.

**The guard on real data.** 33 courses have corridor nodes, and the corridor makes 167 nodes land. The guard cuts 3 courses, each at its first below-sea interior node, which is open sea, so no corridor land comes before it:

| Course | Where | Sampled |
|---|---|---|
| Sénégal | Saint-Louis (16.10, −16.52) | −11.1 m |
| Pánuco | Tampico (22.21, −97.91) | −10.5 m |
| Clarence | New Zealand (−42.14, 173.97) | −16.8 m |

**Seas.** The Black Sea, the Baltic (and the Gulf of Bothnia), the Mediterranean, the Aegean and the Sea of Azov are still ocean; the Caspian is not. This was probed at one node each, with a temporary probe that is not committed. The reference fixtures `indus.json` and `amazon.json` re-bake byte-identical, because `hydro_ref.py` burns no courses, so its `ocean_mask` is unchanged.

**hydro check: FAIL.** 20 of 22 outlets pass:
- **Yangtze:** now passes.
- **Nile:** now fails, on its tolerance only.
- **Mississippi:** still fails, on its New Orleans via.
- **Indus box:** fails on its links, as before.

| Bar | Before this round (round 2) | After | Verdict |
|---|---|---|---|
| Indus box, relaxed | 81.3%, 73/84, worst 77.5 km | the same | FAIL (links only) |
| Amazon flats | 91.6%, 458/482 | the same | PASS |
| The five lakes | exact | exact | PASS |
| Arid basins | 0 of 3, 0 of 2 | the same | PASS |
| Danube | 0.3 km; 42 km | 0.3 km; 7 km (the Chilia arm runs to the sea) | PASS |
| Jhelum / Indus | 1.1 km; 21 km | 1.1 km; 8 km | PASS |
| Ganges / Brahmaputra | 95 km | 110 km | PASS |
| Pearl | 20 km | 31 km | PASS |
| Orinoco | 11 km | 14 km | PASS |
| Amazon, Yellow, Mekong, Congo, Niger, Hudson, Yenisei, Ob, Paraná, Lena, Volga, Mackenzie | — | the same distances | PASS |
| **Yangtze** | 162 km | **121 km** | **PASS** |
| **Nile** | 138 km | **161 km** | **FAIL** (11 km over) |
| **Mississippi** | 137 km; New Orleans 23.1 km | 141 km; New Orleans 37.2 km | **FAIL** (the via) |
| Discharge | Nile 5.11 flagged | identical, all 18 gauges | |
| Endorheic surplus | the Caspian 3,449 of 12,853 m³/s | the same | |
| Burnt nodes | 31,987; p99 439 m; 4,638 over 50 m | 31,986; p99 446 m; 4,621 over 50 m | |
| Pinned at 0 m | 19 nodes on 10 courses | 32 nodes on 11 courses (the Angara added; the Chilia, pinned in this round's first run, left) | |

- **The Yangtze** reaches the coast at (31.85, 120.82), 121 km from its cited outer mouth. Its estuary course is no longer ocean.
- **The Nile** now runs down the Damietta branch to the sea at (31.49, 31.84). Before, Lake Manzala's lagoon was ocean, so the trace stopped 26 km short. Its cited "mouth" is the delta's head (the infobox's 30.17 N, 31.14 E), 161 km from the sea, so reaching the sea fails the 150 km allowance. It fails on its tolerance only and goes to the owner.
- **The Mississippi** still leaves its course 37 km short of New Orleans, by a mechanism the corridor cannot reach (probe below). Its course is no longer ocean anywhere.

**The lakes, diffed** against round 2's dump. 853 of 861 are identical. 8 changed flows, none its kind:
- **Baikal:** 35,544 → 35,343 km², inflow 3,956 → 3,968 m³/s. Its footprint changed at the Angara's outlet.
- **A flat at −328 m in Baikal's northern bed** (55.36, 109.83): inflow 28.4 → 25.3.
- **A data lake at −0.1 m beside New Orleans** (29.78, −90.19): inflow 2.6 → 12.9. The Mississippi's course node at New Orleans drains into it.
- **Michigan–Huron, Erie and Ontario:** under 0.2 m³/s each, and Michigan–Huron's area +78 km². The St. Clair was reversed to its true direction.
- **Two data lakes at 207 m in the Athabasca delta:** about 1 m³/s each.

**The Mississippi, probed.** Node by node near New Orleans, every course node is land. None drains straight to the ocean: the inland rule holds.
- The course's routing elevation is its land chain: 0 m on land plus 0.01 m per node from its end. It stands 0.19, 0.18, 0.17 … 0.08 m over its last 12 nodes here.
- The marsh beside it samples 0.06–0.11 m. At (30.06, −90.70) the course node (0.17 m) has an off-course neighbour at 0.11 m, the steepest drop, so the river leaves the course there. It reaches the sea at (29.87, −90.45), Barataria's ocean component.
- Keeping the course off the ocean cannot fix this, because the course is higher than the land beside it.
- **Options for the owner:**
  - Interior course nodes take their next course node as their receiver.
  - The corridor's beds may go below 0 m, down to `level − 10 m`, since corridor nodes can no longer become ocean.
  - Accept it, as a via tolerance.

Pictures: [Mississippi](hydro-check-outlet-mississippi.svg), [Nile](hydro-check-outlet-nile.svg), [Yangtze, now passing](hydro-check-outlet-yangtze.svg) (round 1's [before](hydro-check-outlet-yangtze-before.svg)), and the [Danube](hydro-check-outlet-danube.svg). The Indus box picture is refreshed.

**Pinned at 0 m, the Angara.** Now that it is oriented right, its first part starts at Listvyanka on Baikal's shore. That point is not in the lake map, but its 5′ sample mixes in Baikal's bed (the 1′ cells within 5′ have a mean of 126 m and a minimum of −978 m). The running minimum falls below 0 m, so the course down to Irkutsk is burnt at 0 m. Reading mapped lakes at their surface in the burn does not reach it (tried: the point is outside the map). Left for the owner.

### Fix round 2 (2026-11-26)

The controller's rulings:
- **A. The Caspian is a cited endorheic lake.** `lake_levels.json` flags the Caspian alone (`endorheic: true`). The source is Wikipedia's "Caspian Sea", read 2026-11-26: "An endorheic basin…"; its infobox gives the type "Endorheic" and the outflow "Evaporation". The Dead Sea and the Great Salt Lake are the lake map's, not the table's, so they carry no flag. A flagged lake:
  - is terminal whatever its balance, with its surplus reported (`Lake::surplus_m3s`, the check's "endorheic lakes' surplus" line);
  - has its nodes at or below its level seed routing like the sea at that level (`Routing::with_sinks`), so the land around it drains by descent instead of filling to its spill.

  Unflagged lakes keep their rules. `hydro_ref.py` mirrors the seeding (`route(..., sinks=)`, `resolve_lakes(..., endorheic=)`). A flagged table lake inside a reference box is mirrored; an unflagged one is still refused.
- **B. Burnt courses keep their order.** A course node's routing elevation is computed from the course's end upstream: `level − 10 m`, never below 0 m on land, and at least `DEPRESSION_EPS_M` (0.01 m) above the next node's. A stretch the 0 m floor would flatten now still falls toward the course's end. A burnt node that ends no course never drains into an ocean neighbour (`Routing::with_inland`), so a course meets the sea only at its own end.
- **C. Lake-centreline parts are oriented by their connections first** (`orient_lake_parts`). An end at an oriented course's first point is the part's downstream end; an end at a course's last point is its upstream end. This is repeated, so chains of reservoirs resolve. ETOPO decides only where neither end connects or the ends disagree.
  - Of the 183 parts: 149 were oriented by connection, 27 by ETOPO (no connection) and 7 were ambiguous (ETOPO). 14 parts changed direction, among them the Zambezi's Cahora Bassa, three Columbia reservoirs, the Volta and the Firat.
  - Spot checks:
    - The Novosibirsk Reservoir runs Kamen (53.89, 81.33) to the dam (54.85, 83.03), by connection.
    - Krasnoyarsk, Lake Nasser and Tsimlyansk (Don) are oriented by connection, all downstream.
    - The Volga: six reservoir parts by connection. The Gorky, Saratov and Volgograd reservoirs fell back to ETOPO, because their neighbours include the river part between Saratov and Balakovo, which ETOPO orients the wrong way. All three still run downstream.

The table is 964,421 bytes (unchanged). Run: `dev` profile. Pass time at 0 ka 1.33 s (0.20 s sampling); 118,963 river nodes (118,753 before the round), 861 lakes.

**hydro check: FAIL.** 20 of 22 outlets pass (19 before):
- **Yangtze:** fails on its tolerance only, 162 km, awaiting the owner.
- **Mississippi:** fails on its New Orleans via only, 23.1 km; it ends 137 km from its mouth.
- **Indus box:** fails on its links, as before.

| Bar | Before this round (round 1) | After | Verdict |
|---|---|---|---|
| Indus box, relaxed | 81.3%, 73/84, worst 77.5 km | the same | FAIL (links only) |
| Amazon flats | 91.6%, 458/482 | the same | PASS |
| The five lakes | exact | exact (the Caspian −28.0 m: its seeded lake holds the table's level) | PASS |
| Arid basins | 0 of 3, 0 of 2 | the same | PASS |
| Danube | 0.3 km, 42 km | the same | PASS |
| Jhelum / Indus | 1.1 km; 7 km | 1.1 km; 21 km (the delta's course now leads to its own end) | PASS |
| Ganges / Brahmaputra | 110 km | 95 km | PASS |
| Mekong | 91 km | 103 km | PASS |
| Nile | 136 km | 138 km | PASS |
| Niger, Congo, Hudson, Yenisei, Ob, Paraná, Lena, Mackenzie, Yellow, Pearl | — | the same margins | PASS |
| Orinoco | 27 km | 11 km | PASS |
| Yangtze | 162 km | 162 km | **FAIL** (12 km over) |
| Mississippi | 170 km; New Orleans 51.8 km | 137 km; New Orleans 23.1 km | **FAIL** (the via, 8.1 km over) |
| Volga | Volgograd 157.3 km; the Sea of Azov | Volgograd 13.0 km; ends in the Caspian, 70 km from its mouth | **PASS** |
| Discharge flags | Nile 5.11; Volga no river | Nile 5.11 only; Volga at Volgograd 10,118 m³/s, ratio 1.23 | |
| Endorheic surplus | — | the Caspian: 3,449 of 12,853 m³/s in, not routed | |
| Burnt nodes | 31,987; p99 434 m; 4,622 over 50 m | 31,987; p99 439 m; 4,638 over 50 m | |

Other discharge moves, all under 3%:
- Mekong at Kratié: 7,603 → 7,842 m³/s.
- Hudson: 412 → 407.
- Yenisei: 20,051 → 20,053.
- Ob: 17,977 → 17,979.
- Paraná: 25,644 → 25,647.
- Lena: 17,592 → 17,588.

**The lakes, diffed.** The check now writes `hydro-check-lakes.tsv`: every lake's kind, level, area, inflow, evaporation, outflow and exit. The round's start (commit c1ddda6, rebuilt with only that dump added) was diffed against this run by each lake's exit. Of 861 lakes, 840 are identical. The rest:
- **The Caspian:** Open → Terminal, 3,270 → 0 m³/s out (the flag). Its exit moved, since its basin is now its sinks.
- **One lake's exit moved with the same flows:** lake 497, 421.9 m, in Sweden.
- **19 lakes changed inflow, and so outflow, because the routing upstream of them changed.** None changed its kind, and none lost an outflow by a lake rule.
  - Lake Elton (49.10, 46.66): 10,041 → 0.7 m³/s in. The Volga no longer runs through it; it keeps its 0.7 m³/s and evaporates it.
  - Five small lakes in the Caspian depression: 481 → 5 m³/s at (50.56, 51.69), 21 → 54 at (48.79, 50.15), and three under 6 m³/s. Their basin now drains by descent to the Caspian.
  - Flathead Lake (47.80, −114.08): 792 → 921. Pend Oreille (48.21, −116.34): 954 → 1,083. The Clark Fork's reservoir parts were reoriented.
  - Ladoga's outlet at (59.98, 31.15): 2,503 → 2,440.
  - Lake Constance (47.69, 9.06): 414 → 430. The Rhine's lake part was reoriented.
  - A lake by the Yellow River's source (34.90, 97.28): 25 → 39.
  - Eight more, each by under 3%.

Pictures:
- The Volga before and after this round: [hydro-check-outlet-volga-before.svg](hydro-check-outlet-volga-before.svg), [hydro-check-outlet-volga.svg](hydro-check-outlet-volga.svg).
- The rivers that still fail: [hydro-check-outlet-mississippi.svg](hydro-check-outlet-mississippi.svg) and [hydro-check-outlet-yangtze.svg](hydro-check-outlet-yangtze.svg).

**The Mississippi now.** Rule B holds it on its course past LaPlace. Its course nodes route 0.69, 0.38 and 0.04 m toward the Gulf, and none drains into the Maurepas–Pontchartrain ocean node beside it. At (29.97, −90.45) its next course node, (29.97, −90.32), is itself an ocean node: Lake Pontchartrain's south shore at Kenner falls inside that 10 km cell. The burn never turns ocean into land, so the course meets the sea there, 23.1 km from New Orleans' point and 137 km from the Head of Passes. This is a lattice-resolution limit, not a rule fault. It goes to the owner unchanged.

**The Yangtze** is as round 1 left it: 162 km from its cited outer mouth, where the game's sea begins up the estuary. It waits on the owner's allowance.

### Fix round 1 (2026-11-26)

One review finding, one controller ruling and three owner decisions:
- **Misroute pictures** (review). The check writes `hydro-check-outlet-<river>.svg` for every
  outlet (`outlet_svg`). Each shows the trace through the lattice's node centres (red), today's
  burnt courses near it (green) and the bar's cited start, vias and mouth (black), with a 100 km
  scale bar. The pictures of the rivers that still fail are linked below.
- **The Caspian's overflow** (controller): not applied. The ruling's own guard fails on the real
  bake (below).
- **Burn the reservoirs** (owner): done.
- **Hand-add delta channels** (owner): done for the Yangtze, the Mekong's two arms and the
  Orinoco's Río Grande (the owner's "Yes, add it"). Not done for the Mississippi, whose course
  already runs to its mouth (below).

Run: `dev` profile. Pass time at 0 ka 1.32 s (0.20 s sampling), against 1.46 and 1.44 s
before the round. 118,753 river nodes (118,759), 861 lakes (861).

**hydro check: FAIL.** 19 of 22 outlets pass (16 before). The Yangtze fails on its tolerance only,
the Mississippi and the Volga on their route.

| Bar | Before this round | After | Verdict |
|---|---|---|---|
| Indus box, relaxed | 81.6%, 73/84 links, worst 77.5 km | 81.3%, 73/84, worst 77.5 km | FAIL (links only) |
| Indus box, coarse | 80.5%, 72/84, worst 77.7 km | 80.1%, 72/84, worst 77.7 km | |
| Amazon flats | 91.6%, 458/482, worst 145.9 km | 91.6%, 458/482, worst 145.9 km | PASS |
| The five lakes | at their levels | at their levels (unchanged) | PASS |
| Arid basins | game 0 of 3, reference 0 of 2 | the same | PASS |
| Danube, Jhelum, Indus, Amazon, Ganges, Brahmaputra, Yellow, Pearl, Nile, Congo, Niger, Hudson, Yenisei, Paraná, Lena, Mackenzie | PASS | the same margins, digit for digit | PASS |
| Yangtze | 341 km from its mouth, by the old Yellow River mouth | 162 km: the estuary's ocean begins at (32.05, 120.35) | **FAIL** (12 km over) |
| Mekong | 193 km, the Gulf of Thailand | 91 km, at (9.51, 106.29); Phnom Penh 7.8 km | PASS |
| Mississippi | 170 km; 51.8 km from New Orleans | unchanged | **FAIL** |
| Ob | Novosibirsk 168.9 km off; ends 87 km | Novosibirsk 9.1 km; ends 87 km | PASS |
| Orinoco | 174 km from the delta's head | 27 km from Barima Point, its new mouth, at (8.75, −60.61) | PASS |
| Volga | Volgograd 157.3 km off; ends at the Sea of Azov | unchanged (157.3 km, 689 km) | **FAIL** |
| Burnt nodes (informative) | 30,738; max 1,362 m, p99 439 m, 4,452 over 50 m | 31,987; max 1,362 m, p99 434 m, 4,622 over 50 m | |

The Indus box moved by 0.3–0.4 points: the reservoirs now burnt include two in or near it, the
Indus's Tarbela and the Sutlej's Bhakra. The discharge table changed only where reservoirs or
channels joined a river:
- Paraná at Corrientes: 24,539 → 25,644 m³/s (1.29 → 1.35).
- Mackenzie at Tsiigehtchic: 8,835 → 10,896 (0.96 → 1.18).
- Ganges at Farakka: 10,314 → 10,310.
- Mississippi at St. Louis: 9,704 → 9,703.

The same two are flagged: the Nile, 5.11 (the owner chose to add channel losses after this
milestone), and the Volga at Volgograd, no river.

Pictures of the rivers that still fail:
- [hydro-check-outlet-yangtze.svg](hydro-check-outlet-yangtze.svg), and before this round:
  [hydro-check-outlet-yangtze-before.svg](hydro-check-outlet-yangtze-before.svg).
- [hydro-check-outlet-mississippi.svg](hydro-check-outlet-mississippi.svg).
- [hydro-check-outlet-volga.svg](hydro-check-outlet-volga.svg), and before this round:
  [hydro-check-outlet-volga-before.svg](hydro-check-outlet-volga-before.svg). The Caspian was not
  changed, so there is no after-the-Caspian picture.

#### The reservoirs burnt

The bake now also keeps Natural Earth's `Lake Centerline` parts of rank ≤ 7 that the lake map
does not cover. A part is covered when at least half its length lies in mapped lake cells
(`LAKE_CENTRELINE_MAX_COVER` = 0.5, `lake_cover`), sampled at the midpoints of 1 km steps on the
lake map's 0.05° grid. `test_select_keeps_lake_centrelines_no_mapped_lake_covers` and
`test_lake_cover_is_the_share_of_length_in_mapped_lakes` pin it.

The table is now 964,421 bytes (≤ 1 MB): 1,428 courses, 1,241 before. That is 183 lake-centreline
parts and the four channels. Added (183 parts, by name):

(unnamed) (3), Al Furat, Alabama (2), Albany, Albert Nile, Angara (2), Atbara, Back (2), Balsas
(2), Bandama (2), Bell, Caniapiscau (2), Caroní, Cauvery (2), Chambal, Chattahoochee (3),
Churchill (2), Colorado (3), Columbia (5), Conchos, Cunene, Dalälven (2), Dnipro (4), Don, El Bahr
el Azraq, Ertis, Firat (3), Fuchun, Grande (5), Green, Grijalva (2), Guadiana, Han, Hudi, Ile,
Indus, Kabalebo, Kama (6), Kemijoki, Krishna, La Grande, Lachlan, Lempa, Lotta, Madison, Mae Klong,
Mahäna Nadï, Manicouagan, Missouri (4), Mouhoun, Murray (2), Nakanbé, Naryn, Negro, Nelson, Niger,
Nile (Lake Nasser), North Platte (4), Nottaway, Ob (the Novosibirsk Reservoir), Orange (2), Ord,
Paranapanema (3), Paranaíba (3), Paraná (4), Peace, Pecos (2), Ping, Pit, Pripyat, Péribonka (3),
R. des Outaouais (2), Red, Rhein, Rio Grande (4), Ro Grande de Santiago (2), Roanoke (3),
Sacramento, San Joaquin, Sanggan, Saskatchewan (2), Sassandra, Savannah, Snake (2), South
Saskatchewan, Stora Lule lv, Suda, Sukhona, Sutlej, Syr Darya (2), São Francisco (2), Tajo (2),
Tana (2), Tennessee (2), Tocantins, Tugaloo, Vaal (2), Verkhniy Yenisey (the Krasnoyarsk
Reservoir), Vilyuy, Volga (9), Volta, Wisconsin (2), Yom, Yuan, Zambezi (2), Zeya.

Left out as covered (136 parts, by name):

(unnamed) (15), Abay, Albany (4), Amazonas, Amur (2), Angara (Lake Baikal), Athabasca (2), Back
(2), Bell (3), Biya, Caniapiscau, Chambal, Churchill (12), Clark Fork, Colorado (2), Coppermine
(2), Dalälven (2), Dniester, Dnipro, Gan, Huang (2), Hudi, Jhelum (2), Jordan (2), Klamath, La
Grande (2), Lempa, Mackenzie (Great Slave Lake), Manicouagan, Mississippi (2), Missouri, Narva (2),
Nelson (4), Neva, Ångermanälven (2), Niger (the inner delta's lakes), Nizhnyaya Taymyra, Nottaway,
Ottawa, Ouémé, Pangani, Peace, R. des Outaouais (2), Rainy (2), Red, Rhein, Rhône (Lake Geneva),
Ro Grande de Santiago, Rupert (2), Rusizi, San Juan, Saskatchewan (2), Semliki (2), Severn (3),
Shire, Shishhid Gol, Snake, Sutlej, Svir, Syr Darya, Teslin, Thelon (4), Trombetas, Ural, Victoria
Nile (3), Vistula, Vorma (2), Vuoksi (2), Waikato, Winnipeg (3), Wujia (2), Yellowstone, Yuan.

The full lists with each part's cover and ends are in the bake's summary (`lake_centrelines_added`,
`lake_centrelines_covered`). The Krasnoyarsk gorge entry stays: the burn now also carries the same
reservoir's course.

One concern: orientation by the ends' lowest nearby cells works poorly on a flat reservoir. The
bake reversed 224 parts against 136 before. An example of a wrongly oriented part that matters:
Natural Earth's River part of the Volga between Saratov and Balakovo, where the lowest cells
are 14.54 and 14.49 m.

#### The hand-added delta channels

`data/hydrology/rivers_extra.json` holds four cited channels. The bake appends them after Natural
Earth's courses (`read_extras` refuses an uncited entry). Each course is the least-cost path over
ETOPO 2022 60″ cells between the cited points, a step costing its length × (1 + max(0, the cell's
elevation in m)) so the path keeps to the water, simplified to 1 km.

| Channel | From | To (cited) | Source for the course |
|---|---|---|---|
| Yangtze estuary | Natural Earth's Yangtze end at Zhenjiang (32.197, 119.606) | the Yangtze's infobox mouth (31.39361, 121.98306) | "Yangtze". It passes 12 km from Jiangyin's and 16 km from Nantong's article points (city centres off the banks). |
| Hậu (Bassac) | Natural Earth's Bassac end at Cần Thơ (10.005, 105.838) | "Bassac River", the article's coordinates (9.51667, 106.21667) | "Bassac River": mouth in Sóc Trăng province. |
| Tiền | Natural Earth's Tiền arm end (10.238, 106.119) | the Mekong's infobox mouth (10.19, 106.75) | "Mekong"; "Tiền River" lists its distributaries. |
| Río Grande (Orinoco) | "Barrancas del Orinoco" (8.70194, −62.19944), the delta's head | "Barima Point" (8.60722, −60.41083) on the Boca Grande; via "Curiapo" (8.58, −60.9975), 0.8 km from the course | "Orinoco Delta": "The main distributary is called the Rio Grande, which empties south-southeast through the southern portion of the delta". Britannica places the Boca Grande at Barima Point, but its page refused an automated read: that point rests on a search engine's summary of it, 2026-11-26. |

All are English Wikipedia articles, read 2026-11-26. The Orinoco outlet's mouth moved to Barima
Point; its allowance stays 150 km. `test_the_hand_added_channels_are_cited_and_reach_their_mouths`
(Python) and `the_hand_added_delta_channels_reach_their_mouths` (Rust) check each channel. Each
must:
- run node to node (as every course does);
- end within 10 km of its cited mouth;
- for the three that continue Natural Earth, start on that course's last point.

#### Why the Mississippi got no channel, and why the Yangtze ran north (a rule problem)

Natural Earth's Mississippi already runs from Baton Rouge past New Orleans to (28.94, −89.41) by
Southwest Pass, so a hand-added channel would only repeat it. The slip comes from the burn's
rule:
- A burnt node at or above sea level routes at max(level − 10 m, 0 m).
- Every course node whose running-minimum level is within 10 m of the sea therefore routes at
  exactly 0 m, so the lower course is a flat with no direction.
- A flat drains toward whichever ocean contact the flood reaches first.

The probe of the trace's nodes shows it. The levels fall from 7.6 m at Baton Rouge (30.63, −91.34)
to 0.7 m at LaPlace (30.06, −90.70), all routing at 0 m. LaPlace's node neighbours an ocean node
of the Maurepas–Pontchartrain water at (30.16, −90.57), which the flood reaches before the Gulf.

The Yangtze ran north along the burnt Hudi for the same reason:
- The Hudi's nodes (levels about 3 m) also route at 0 m.
- They joined the Yangtze's 0 m end into one flat, which drained across the northern Jiangsu
  lowland.
- The course was never followed against its direction as such: the clamp removed its direction.

With the estuary channel the flat now reaches the estuary's ocean first, and the Yangtze stays on
it. The Mississippi needs a rule change, which this round does not make. One way: keep a burnt
course's own order on its 0 m flats, the bed falling a little toward its end.

#### Why the Volga still misses Volgograd

The Volga reservoirs are burnt now, and the trace follows the burnt Volga from Nizhny Novgorod to
Saratov. Below Saratov it leaves its course at (50.85, 46.00) and runs straight south through Lake
Elton into the Caspian (the picture).
- The whole lower Volga lies inside the Caspian's depression basin. Its filled level is the
  basin's spill, 41.2 m on the Kuma–Manych.
- Inside a basin, routing is a breadth-first search toward the basin's sources (its lake's
  footprint, and its exit while the lake overflows), not by elevation.
- So the course's levels (16–20 m there) never steer it, and the straight red runs in the
  picture are those search steps.
- The Caspian still overflows over the Manych to the Don (2,119 m³/s).

#### The Caspian ruling was not applied: its guard fails

The ruling made a lake whose level comes from the lake map or the level table, standing below
its basin's spill, terminal whatever its balance. I implemented it, with a tolerance of
`OPEN_LAKE_MAP_TOL_M` = 10 m so a lake at its spill still flows. The Caspian then turned terminal
with a 2,142 m³/s surplus, and the Don/Manych river went away.

On the real bake it also stopped lakes that really drain:
- **Table lakes:** Baikal (455.5 m, surplus 4,124 m³/s; it drains by the Angara), Superior (183
  m, 2,775 m³/s; the St. Marys) and Ontario (74 m, 7,562 m³/s; the St. Lawrence).
- **Mapped lakes at the present day:** 313 in all. Among them:
  - Kyoga, which ended the Nile there ("ends in a lake at (1.45, 32.75)").
  - Tanganyika, Malawi and Titicaca (a surplus of 496 m³/s against its real Desaguadero
    outflow).
  - A 14,439 m³/s lake on the lower Amur.
- The Ob's and the Volga's traces then ended in Kulunda's and Elton's lakes.

The lattice's spill is the level at which the 10 km lattice would overflow. For a lake drained
through a narrow pass (the Angara, the St. Lawrence, the Victoria Nile before its gorge) that
spill stands above the real outlet. So "below its basin's spill" does not mean "below its real
sill". Everything was reverted. The table and lake rules are as before the round.

Options for the controller:
1. A cited "endorheic" flag per level-table lake (only the Caspian among the seven), leaving
   mapped lakes as they are.
2. The rule for table lakes only, with cited gorges for the Angara, the St. Marys and the St.
   Lawrence.
3. Another criterion.


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
