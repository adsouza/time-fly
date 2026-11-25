# Time Fly: An Interactive Memoir of Earth — Design

**Status:** Ready for approval (review revisions incorporated) · **Date:** 2026-11-21 · **Revised:** 2026-11-22
**Engine:** Bevy 0.20.1 (WESL shaders) · **Targets:** WebGPU (`wasm32-unknown-unknown`) and Vulkan desktop; development on macOS (Metal) · **Physics:** avian3d 0.8 · **Distribution:** free

Lines marked *(call)* are decisions I made that the owner hasn't explicitly confirmed. Everything else was decided in conversation.

---

## 1. Intent

Time Fly is a calm, single-player 3D wandering game about Earth's history. You are a tiny creature that can't change anything. You fly through the air and swim through water, across the whole planet and back through deep time. You can descend from orbit to a region, then to a local landscape, and at chosen places into a city street. A time slider moves the world through snapshots of the past, spaced further apart the further back they go.

**Why it exists:** to let people *feel* the scale of Earth's history (continents drifting, ice advancing, the first cities) through wandering rather than reading. Accuracy is "within reason": the big-picture facts are real, the close-up detail is plausible generated content, and it has to be fun to be in.

**The session:** pure wander. There are no goals, scores, quests or story. You drift, dive, surface and move the slider because looking is the reward. The product is atmosphere: era-appropriate light, sky, weather, water, sound, and how the creature moves.

## 2. Goals, non-goals, anti-goals

**Goals**
- The whole globe is explorable at any snapshot, with continental drift animating as you drag the slider.
- Natural worlds can be entered **anywhere**, at any era.
- Street worlds exist at a **chosen** list of city-and-era sites, each with a building kit appropriate to its culture and era.
- **Generated small settlements** (camps, hamlets, villages, small towns) appear in natural worlds wherever population reconstructions say people lived in that era (§12.1).
- **Ships and boats** sail rivers, coasts and seas wherever and whenever the historical record supports watercraft, with vessel types suited to the era and region (§12.2).
- Moving between levels feels like nesting: zoom-and-swap with continuous visuals.
- It runs identically on WebGPU and Vulkan.
- The art style is painterly naturalism: real landforms and biome colours, simplified brushy surfaces, warm art-directed light.

**Non-goals (for now)**
- Narration, story, a journal, collectibles or quests.
- **Visible animals or people (deferred).** For now, plants are the only visible life; fauna and crowds are heard, not seen (§13). The owner **will** want ambient fauna later, so the design leaves room for it (§18).
- Interacting with the world (picking things up, moving objects, NPC behaviour).
- Photorealism.
- Seamless continuous descent from orbit to street.
- Generated **large cities**. Big cities exist only as chosen, explicitly modelled sites. Generation stops at small towns.
- Multiplayer, saving or user accounts.

**Anti-goals (would make it a failure even if it works)**
- Anything that feels like a task list or a UI-heavy encyclopedia. The UI stays nearly invisible.
- Confidently wrong big-picture facts: coastlines, ice, the existence of plants, animals and cities must match their era (no grass in the Jurassic, no birdsong in the Triassic, no cities 50,000 years ago).
- Visible cracks, popping or z-fighting that break the dreamlike flow.

## 3. Constraints

- **Bevy 0.20.1.** WESL is Bevy's built-in shader composer in 0.20 (`bevy_shader` depends on `wesl`; `naga_oil` is gone), so all shaders are `.wesl`.
- **The WebGPU baseline is the ceiling** (carried over from the draft architecture): no mesh shaders, no `MeshletMesh`, no 64-bit atomics, no unbounded `binding_array`, no `std::thread` in core loops (use Bevy task pools or compute shaders). Use only 32-bit atomics. WebGPU has no multi-draw-indirect, so on the web Bevy batches on the CPU and draw-call budgets matter.
- **Textures** are `.ktx2` (Basis Universal / UASTC).
- **Cargo changes from the draft:** the Bevy feature is `zstd_rust` (there is no `zstd` feature in 0.20). Add `webgpu` on wasm32. `avian3d = "0.8"` (targets `bevy ^0.20`). Drop `fastnoise-lite` (see §8.3).
- **Free distribution.** Datasets with non-commercial terms are acceptable, but every dataset's attribution terms must be met. Credits live on a small credits screen reached from a minimal menu, not in the in-world UI. Distribution channels (web build, desktop downloads) are the builder's call.
- **Content is mostly generated, plus CC0 libraries** (§17). There is no dedicated artist or audio designer, so the art style must stay forgiving.
- **Precision:** rendering uses f32 inside each world. Geographic positions and era times are kept in f64 on the CPU.

## 4. Approaches considered

| | Approach | Trade-off | Verdict |
|---|---|---|---|
| **1** | **A renderer per level.** Each level of the tree uses the technique suited to its scale, and all of them share one world generator. | Four renderers instead of one, but each is small, testable and can ship on its own. The globe never needs ground-level detail, because zoom-and-swap hands you to a child world first. | **Chosen** |
| 2 | One engine everywhere: a cube-sphere CDLOD system with all four detail tiers in every world (closest to the draft). | Uniform, but over-built. It solves ground-level globe detail that zoom-and-swap makes unnecessary, and plate motion is awkward on one continuous height map. | Rejected |
| 3 | One continuous planet with a floating origin and seamless descent. | Contradicts the chosen zoom-and-swap transitions and brings back planet-scale precision problems. | Rejected |

## 5. World model

### 5.1 The tree

```
Globe  (whole planet; era-specific continents, ice, sea level)
 └─ Region  ~300 km square   (call)       CDLOD terrain
     └─ Local  ~4 km square  (call)       CDLOD + close-up 3D voxel detail
         └─ Street  ~0.5 km  (call)       CDLOD ground + building kits; only at chosen sites and eras
```

Level sizes are chosen so f32 world coordinates stay precise inside every world *(call)*.

### 5.2 A world is a function

A world is fully determined by **(level, crust position, era, seed)**. Generation is deterministic, and nothing generated is saved.

**Positions on tracked crust** use the source model's stable plate ID and canonical plate-fixed latitude/longitude. The forward lookup applies the plate rotation only when its rotation and the position's footprint are both valid at that age. Normalize longitude to `(-180°, 180°]` and use zero longitude at the poles. Dense rotation-table indices are internal to a particular bake.

**Looking up a position in the other direction** *(call)*: at the requested age, inverse-rotate the era-space direction for each candidate plate and query the plate-fixed footprint at the resulting point. Reject candidates without a valid circuit or footprint at that age. Among valid overlaps, use the highest visible land surface, breaking ties by ascending source plate ID. Where there is no visible land, use the lowest valid plate ID. A point with no valid candidate uses an **era-fixed** key (snapshot age and canonical geographic coordinates). It remains explorable in that era. Deep-time gaps beyond the available terrain coverage use the ocean surface and a coarse ocean-floor fallback. When time changes, a traveller on era-fixed terrain or newly invalid crust returns to the globe.

- Use this lookup for each terrain sample, including across plate boundaries. The generator uses **resolved position, era, and seed** for terrain and a canonical crust-space lattice for seeding small-scale detail. A world's centre only sets its window. Overlapping worlds agree at the same geographic point and resolution, including at boundaries and in fallback areas.
- Each world is laid out in its era's frame, with north up for that era. On a snapshot change, rebuild around the same still-valid crust position in the new era's orientation, hidden by the crossfade.
- The recent globe displays the ice and sea-level grids in geographic coordinates. For those ages, use the present-day plate orientation in both lookup directions and evaluate validity at the requested age.

GPU noise has to be evaluated without f32 precision loss at planet-scale coordinates. The generator therefore splits each position into an integer lattice-cell index (hashed to make the per-cell seed) plus a local fraction *(call; the builder chooses the scheme)*.

### 5.3 Coarse truth, amplified

Past-era elevation data is coarse (about 1° per pixel, every 5 Myr). Present-day data is fine but too large to bundle for the web. Every level below the globe therefore follows one rule: **real data decides the large scale, and generation adds detail below the data's resolution.**

- Real data fixes: elevation trend, coastline / sea level, ice cover, biome, and climate band.
- Generated detail adds: landforms below the data's resolution (ridges, valleys, dunes, river meanders), vegetation placement, rock formations, and close-up 3D detail.
- Generated detail must never contradict the data. For example, it can't raise land above sea level where the data says ocean. That needs a coastline-preserving blend.
- Within roughly the last 26,000 years, accuracy improves sharply, because the present-day elevation model plus ice and sea-level reconstructions are high resolution.

## 6. Time model

### 6.1 Snapshots

The slider maps **log(years before present)** to position, with an offset so present maps to a finite value. Use a fixed game reference year of **2026 CE** *(call)* so saved data assets and cultural stops remain reproducible. Dated sources using the 1950 BP convention are converted to game age by adding 76 years. Deep-time model ages retain their published Myr values. The standard snapshots are *(call)*:

> present · 100 · 500 · 1 ka · 2 ka · 5 ka · 10 ka · 21 ka (Last Glacial Maximum) · 50 ka · 100 ka · 1 Ma · 5 Ma · 10 Ma · 20 Ma · 35 Ma · 50 Ma · 66 Ma · 100 Ma · 150 Ma · 200 Ma · 250 Ma · 300 Ma · 400 Ma · 540 Ma

**Extra snapshots** come from the owner's civilizations CSV (§10). The years of notable rows within the last ~12 ka are added as extra stops. Which rows count as notable is left to the builder, with a cap of roughly 20–30 extra stops so the slider stays usable.

### 6.2 Slider behaviour by level

- **Globe:** continuous. Dragging animates plate motion, and ice sheets and sea level animate too (§7.1).
- **Region, local and street:** the slider snaps to the nearest snapshot. Changing snapshot regenerates the current world for the new era and crossfades to it (about 1 s). The creature keeps its **crust position** (§5.2) and its height above ground.
- **A place that doesn't exist in that era** (a street world at a site with no city in that era): the creature is lifted, using the same zoom used for exiting a world, to the nearest ancestor level that does exist.

### 6.3 What the era controls

Everything follows the era: elevation and coastline, ice, climate band and biome, **vegetation palette**, fauna-dependent ambient sound, and whether street worlds exist. Proposed vegetation eras *(call; the builder refines these with references)*:

| Era (years ago) | Land vegetation |
|---|---|
| > 470 Ma | Barren rock; microbial mats at shorelines |
| 470–385 Ma | Mosses, low early plants near water |
| 385–300 Ma | First forests; ferns, horsetails, giant clubmosses (coal swamps) |
| 300–130 Ma | Conifers, cycads, ginkgos, ferns; no flowers, no grass |
| 130–20 Ma | Flowering plants spread; broadleaf forests |
| < 20 Ma | Grasslands and savannas spread; modern biomes |

**Human land use** *(call)*: from about 10 ka to the present, farmland, pasture and cleared forest change the vegetation and colour layers of natural worlds, based on land-use reconstructions for the past and land-cover data for the present. Settlements are handled separately (§12.1). Together, land use and settlements avoid the "confidently wrong" pristine-everywhere look in the most visited eras.

## 7. Rendering by level

All levels share: **painterly shading** (§9), the **world generator** (§8), Bevy's lights, shadows and fog, and the **underwater pipeline** (§7.6).

### 7.1 Globe: a sphere made of plate pieces

- **Data:** bake each source elevation snapshot into a geographic cube map and record its timeline age and source reconstruction age. Bake plate-fixed footprints with all candidate plate IDs and validity intervals per cell, plus rotations with explicit missing-circuit flags. Check alignment against the source coastlines, including at 540 Ma.
- **Runtime:** build separate cube-sphere pieces with one plate per triangle and duplicate vertices along plate boundaries. Rotate each piece with its currently valid rotation. For each bracketing snapshot, rotate the same plate-fixed sample direction into that snapshot's source frame before sampling. Blend valid samples in plate-fixed coordinates. Use an ocean shell to cover gaps. Resolve overlaps with the highest visible land surface and break ties deterministically by source plate ID *(call)*.
- **Recent eras:** render the recent source grids on a fixed geographic sphere. Use ICE-6G_C signed `Topo` for relief and bathymetry and the `sftlf` and `sftgif` masks for land and ice coverage, including floating ice. Blend coverage between snapshots. Use a short crossfade to the deep-time globe beyond the ICE-6G_C interval. Any interval represented by an approximation needs an explicit data note.
- **Visuals:** painterly atmosphere rim, clouds, a day/night terminator *(call)*. There is no LOD beyond normal mip-mapping: the camera never gets closer than about region altitude before the dive.
- **Data budget:** measure compressed transfer, decoded resident textures and peak client memory separately. Verify startup and full-range scrubbing with the full bake in Chrome WebGPU.

### 7.2 Region: CDLOD terrain

- Uses the owner's `terrain_cdlod.wesl`, fixed as follows:
  - Snap morphing vertices by **whole-number grid index** (`grid_uv * N`), not by rounding world positions at exactly .5, which is ambiguous in floating point and causes cracks.
  - Integrate with Bevy's lighting, shadows and fog rather than hand-lit colour. The shader uses its own per-patch storage buffer and `instance_index`, so it needs a custom render pipeline or another Bevy-0.20-compatible route. The builder chooses, but lighting must match the other levels.
  - Fix the clip-radius inconsistency (the draft doc says `voxel_window.w`, the shader reads `.z`).
- The height source is the world generator, sampled into a height texture per world on load (GPU compute or task pool), then streamed in as detail is needed.
- Beyond the world's edge, the outer CDLOD rings continue **to the horizon at a coarser resolution, from the same generator** (the region's apron), so the land never ends at the edge. The parent world itself is not drawn past the edge: under the M3 design's approach A, only the region is drawn below the cloud *(owner, 2026-11-24; `docs/superpowers/specs/2026-11-24-milestone-3-zoom-and-swap-design.md` §1.1 item 6, §4; see §11.2)*.

### 7.3 Local: CDLOD + close-up 3D detail

- Region-style CDLOD, plus the owner's `voxel_surface_nets.wesl` inside a sliding window (~128–256 m) around the creature, for cliffs, overhangs, arches, reefs and seafloor.
- Fixes:
  - Replace the placeholder `sample_macro_height` with the real generator height.
  - Make the voxel blend edge and the CDLOD surface agree. Hardware texture filtering and hand-written bilinear filtering differ slightly, so either sample identically on both sides or overlap within the blend margin.
  - Clamp the indirect draw counts to buffer capacity when the atomic counters overflow.
- Voxel dispatch is skipped in flat terrain and in street-site footprints.
- This is the last tier to build (§14). Local worlds work without it.

### 7.4 Street: chosen sites only

- Ground is CDLOD, flattened and graded along streets and block footprints.
- Buildings are assembled by a deterministic grammar or WFC generator from **modular kits** (wall bays, windows, cornices, roofs, doors, street furniture). Each kit is specific to a **culture and era**: a Uruk mud-brick kit, a Roman insula kit, and so on. Shared mesh and material handles enable automatic batching.
- **Discrete LOD** per kit piece through `VisibilityRange` with dithered transitions.
- **Distant blocks:** each is merged into one simplified mesh on a background task, coloured with vertex colours or a small shared palette rather than baked atlases *(call)*, and swapped in with `VisibilityRange`, so a distant block costs one draw.
- **Interiors:** chosen buildings have interiors that stay hidden until the creature approaches a **window or door opening**, revealed through portal or line-of-sight checks.
- **Landmarks** (e.g. a ziggurat, a pyramid) can be hand-made glTF assets with offline `meshoptimizer` LODs.

### 7.5 Water surface

- A water plane at the era's sea level, with painterly Fresnel, depth tint and shoreline foam.
- Lakes and rivers come from the generator's hydrology *(builder; can start with sea level only)*.

### 7.6 Underwater

- Carried over from the draft: Beer–Lambert depth absorption and animated caustics on terrain, voxels and buildings below the water surface.
- When the camera is underwater, switch to dense underwater fog, colour grading and drifting particles.

## 8. World generator and data pipeline

### 8.1 Data sources (candidates; the builder verifies licences and fitness)

| Need | Candidate |
|---|---|
| Present-day elevation and bathymetry | ETOPO 2022 or GEBCO grid |
| Past elevation, 0–540 Ma | PALEOMAP PaleoDEMs (Scotese & Wright), 1°, 5 Myr intervals |
| Plate polygons and rotations | PALEOMAP plate model via GPlates / pyGPlates (**must match the PaleoDEMs**) |
| Ice sheets and sea level, last ~26 ka | ICE-6G_C or similar |
| Past climate and biome | Quaternary: CHELSA-TraCE21k or PaleoClim. Deep time: PALEOMAP climate-belt reconstructions |
| Present-day land cover | ESA WorldCover or MODIS land cover |
| Past human land use and **population density**, ~10 ka to present | HYDE 3.x (cropland, pasture, population) |
| Watercraft by region and era | A hand-authored **maritime presence table** (waters, era range, vessel types) compiled from the literature and cross-checked with the CSV (builder compiles; owner reviews) |
| Human presence before ~10 ka | A coarse hand-authored map of known human range per era, from the archaeological literature (builder compiles) |

Licences: free distribution allows non-commercial-only terms. Every attribution requirement goes on the credits screen.

### 8.2 Offline preprocessing tool

A separate Rust (or Python + pyGPlates) tool in the repo converts the raw data into compact game assets: source-frame globe grids, plate-fixed footprints, plate rotations and validity, coarse global elevation / biome / ice / sea-level grids per snapshot, and present-day elevation tiles. The outputs are `.ktx2` or compact binary.

- **Desktop:** ships everything bundled.
- **Web:** ships the globe and the coarse grids, and streams present-day elevation tiles as static files from a CDN (no server logic). Hosting is the builder's call.

### 8.3 Noise: one source of truth

The draft paired `fastnoise-lite` on the CPU with a different hash noise on the GPU, which can never match. Instead, implement **one** small noise family twice, in Rust and in WESL, from a single spec, with **golden-value tests** asserting the two agree within an epsilon. The CPU copy serves height queries for creature collision avoidance and object placement. Exact bit-for-bit equality isn't required: a flyer only needs approximate agreement *(call)*.

## 9. Art direction: painterly naturalism

- Real landforms and biome colours from data. Era-aware palettes (§6.3).
- Soft ramped diffuse with hue-shifted shadows, brush-like triplanar noise breaking up surfaces, soft silhouettes on vegetation.
- Gradient skies with painted clouds. Atmospheric perspective tinted per era and biome.
- Light tilt-shift during zoom-and-swap transitions only *(call)*. *(Dropped in M3 Task 11 when the cloud pass was over budget, then restored at the owner's request on 2026-11-25; see the M3 plan's rulings.)*
- Optional painterly post-process (e.g. a cheap Kuwahara-style filter) only if it fits the web frame budget *(builder)*.

## 10. Cultural data (owner's CSV)

The source is `Chronology of global human civilizations - Sheet1.csv`. Columns are regions, rows are years, and cells are cultures, events or technologies. Import it into a data asset with these fixes:

- **Explicit dating convention.** Keep the owner's CSV as the source. During import, use a reviewed mapping to assign each dated row an explicit `approx_bp`, `bce`, or `ce` convention. Store the original value and convention alongside the normalized game age. Earliest rows are approximate BP (1950 reference). Resolve ambiguous rows with the owner before using them.
- Convert CE year `n` to game age `2026 - n`, BCE year `n` to `2026 + n - 1`, and approximate BP `n` to `n + 76`. The BCE/CE calculation accounts for the absence of a calendar year zero. Reject zero-year BCE/CE values and leave entries later than the fixed game reference year out of the slider until the reference or import policy changes *(call)*.
- Sort by normalized age after import. Skip the empty `11,000` row and hold the undated Manteños Guancavilcas row for owner review. Test an approximate BP row and the 1 BCE / 1 CE boundary.
- Map region names to geographic polygons. "Frangistan" is Europe.
- **Known gaps:** Southern Africa is empty; there are no East Asia (beyond China), Southeast Asia, or Oceania/Australia columns; nothing after 960 CE. The owner will extend the CSV. The code must cope with missing data.

**Uses:**
1. **Captions.** A faint line (e.g. *"Natufians · Levant · approx. 12,000 BP"* if the owner confirms that row as approximate BP) fades in when you enter a matching region and era, then fades out. Show dates using their source convention.
2. **Extra snapshots** (§6.1).
3. **Candidates for chosen city sites** (§12).

## 11. Navigation and creature

### 11.1 Creature and camera

- One small winged creature, **hummingbird or dragonfly scale (5–10 cm)**, cruising about 1–20 m above the ground in local and street worlds. It fits through windows. It flies in air and swims underwater (it tucks its wings and changes how it moves). Exact design is an art task *(call)*.
- **This scale sets the detail budget:** finest ground detail around 0.25–0.5 m, grass as painterly clumps or cards rather than individual blades, collision radius around 0.1 m, and a camera near plane to match. The builder sizes the CDLOD spacing, voxel size and vegetation density from these numbers.
- Third-person chase camera, slightly behind and above, so the world fills the frame *(call)*.
- Flight speed scales with level so every level feels quick to cross *(call)*.
- **Controls** *(call)*: keyboard and mouse, plus gamepad. Steer, climb and descend, boost. The slider is on screen and also bound to keys or shoulder buttons.
- **Collision avoidance:** the creature never clips through terrain or buildings. On the globe and in region worlds it uses CPU height queries from the generator. In local and street worlds it uses avian3d shape casts against colliders kept only within ~50 m. There is no full-world collider, and nothing is simulated physically beyond movement and drag.

### 11.2 Zoom-and-swap

Moving between levels uses altitude, with no menu *(call)*:
- **Down:** descending below a level's dive altitude centres a child world under the creature. When entering a child from the continuously timed globe, snap to the child's target snapshot before resolving its location (§5.2). The creature passes through a cloud: it gathers as the creature descends, the swap happens on a frame where the cloud is fully opaque, and the child emerges below it. *(Owner, 2026-11-24, M3: replaces the zoom and crossfade.)* The globe's relief flattens as the creature descends, so the land just above the cloud matches the child below it.
- **One world at a time:** below the cloud only the child is drawn. The parent is not visible past the child's edge *(owner, 2026-11-24, M3: this drops the earlier rule that the parent stays visible beyond the child)*.
- **Street worlds:** chosen sites are softly marked in local worlds (a gentle glow or landmark silhouette, no UI icon *(call)*). Descending into one enters the street world.
- **Up:** climbing through a world's ceiling (a region's cloud base), or pressing `Backspace`, reverses the dive through the cloud and returns to the parent at the matching geographic spot and heading.
- **Sideways and edges:** as the creature nears a region's border heading outward, the neighbouring region is preloaded. At the border the creature is lifted gently to about 60 km into cloud cover, the swap to the neighbour happens inside it, and the creature eases back down to its height above the ground. *(Owner, 2026-11-24, M3: replaces going up through the parent and back down, and the soft boundary that nudged the creature upward and out.)*
- **Loading:** the child or neighbour generates during the transition. If it isn't ready, the creature holds inside the opaque cloud, still moving gently, never a loading screen or a frozen view *(call)*.

## 12. Chosen city sites

The initial candidates come from the CSV: Göbekli Tepe (~9400 BCE), Çatalhöyük (~7000 BCE), Jericho, Uruk (~3300 BCE), Harappa / Mohenjo-daro (~2600 BCE), Giza (~2490 BCE), Caral, Rome (~100 CE), Teotihuacán (~150 CE), Tang Chang'an (~618 CE), Cahokia (~1000 CE), plus one present-day city.

The owner picks the final list and the first site at milestone 6. Until then, nothing depends on it.

**Site layouts** are generated (grammar or WFC) around hand-placed landmarks and the main axes recorded by archaeology (for example, a site's main avenue), stored per site. They are not full archaeological reconstructions *(call)*. Each site is a (location, era range, kit) entry in a data file, so adding sites doesn't need code changes.

### 12.1 Generated small settlements

Beyond the chosen cities, natural worlds contain **generated settlements wherever people plausibly lived in that era**.

- **Where:** population density and land use for the era (HYDE, ~10 ka to present) set how likely settlement is and how big. Before ~10 ka, the human-range map allows only small, sparse camps. Placement within a cell favours plausible spots: near fresh water, on gentle slopes, above flood level, on farmland or coast. The exact spots are generated, seeded from crust position, so they're plausible rather than archaeologically real *(call)*.
- **What:** size follows the era and density: hunter-gatherer camps (shelters, hearth smoke), then hamlets, villages, and market towns up to a few hundred buildings. Anything bigger is a chosen city. Fields, paths and tracks connect them to the land-use layer.
- **How they look:** built from **vernacular kits** chosen by region, era and climate (mud brick, timber, stone, thatch, hide, reed, and so on), with the CSV's region columns supplying the cultural region. These are parametric kits generated from a small set of forms and materials, not hand-modelled per culture *(call)*. They're simpler than chosen-city kits and use the same batching and distant-block merging.
- **Where they live in the tree:** settlements are **features of local worlds, not separate street worlds**. At 5–10 cm scale the creature can already fly between houses inside a local world. They have **no interiors** *(call)*. If the owner later wants to enter a particular town, it can be promoted to a chosen site.
- **The era rule still holds:** no settlements where or when the data says nobody lived, and no towns before farming.

### 12.2 Ships and boats

Watercraft appear on rivers, lakes, coasts and seas **only where and when the historical record supports them**, so the anti-goal on confidently wrong facts applies.

- **Data:** a hand-authored maritime presence table lists waters (polygons or sea lanes), an era range and allowed vessel types. Illustrative entries: Nile reed boats and early plank boats from ~4000 BCE, Mesopotamian river craft, Bronze Age Mediterranean galleys, Indian Ocean dhows, Austronesian and Polynesian voyaging canoes (rolling out across the Pacific roughly 1500 BCE to 1200 CE), Norse longships ~800–1100 CE, Chinese junks, European sailing ships from ~1500, steamships from ~1850, modern cargo ships and ferries. The table is data, so the owner can extend or correct it without code changes.
- **Density:** follows settlement density and nearby chosen sites (§12.1, §12). Harbours and river towns get boats moored and under way; open sea lanes get occasional distant sails. No watercraft before the record supports them, and **no visible crew** (people stay non-visible, §2) *(call)*.
- **Motion:** ships move slowly along generated routes between settlements or along the sea lanes. Position is a deterministic function of route and clock time, so nothing is saved *(call)*. They sit on the painterly water with simple bobbing, wakes and sail sway.
- **Where they appear:** local and street worlds show ships as full models. Region worlds may show only distant sails or wakes near coasts *(call; the builder decides whether it's worth it at 300 km scale)*.
- **Models:** generated **parametric vessel kits** (hull form, length, sail rig, oars, colours) per vessel type, plus CC0 meshes where they fit, with discrete LODs and the usual batching.
- **Creature:** ships get simple kinematic colliders only near the creature, so it flies around masts and sails, never through them.

## 13. Sound

- Ambient soundscapes by biome and era (wind, water, insects, birds, city murmur), mixed according to altitude and the creature's surroundings.
- Fauna sounds respect the era: no birds before ~150 Ma, no city sounds before cities. Sound is the only way animals and people are present (§2).
- A gentle score is optional *(builder)*.

## 14. Suggested build order

1. **Globe + slider:** plate-piece sphere, continuous drift, ice and sea level, painterly sky. Proves the data pipeline end to end.
2. **Region worlds:** world generator, CDLOD fixes, painterly terrain, water, era vegetation palette.
3. **Zoom-and-swap:** globe ↔ region and region ↔ region (M3). Region ↔ local moves to the local-worlds milestone, since local worlds don't exist before it *(builder's call, proposed in the M3 design §1.1 item 8; the owner did not object)*.
   - Then **lakes and rivers** at region scale, from the generator's hydrology (§7.5): a milestone of their own between M3 and M4 *(owner, 2026-11-24)*.
4. **Local worlds:** CDLOD at fine scale with vegetation, entered from regions (region ↔ local), likely through the same cloud *(builder's call, not yet decided)*. Then the voxel close-up tier and underwater.
5. **Captions, settlements and ships:** CSV import, captions, extra snapshots. Then generated settlements in local worlds, starting with one vernacular kit. Then ships, starting with one vessel type on one stretch of water.
6. **First street site:** one kit, distant blocks, interiors. Then more sites.
7. **Sound and polish.**

## 15. Verification (each milestone)

- `cargo check --target wasm32-unknown-unknown` and the native build pass. Verify rendering natively on Metal and in Chrome WebGPU on the development Mac, and on Vulkan using CI-built x86-64 Linux packages (.deb and Flatpak) that the owner runs manually on a test-only Linux machine. Record the adapter and backend used.
- No mesh shaders, `MeshletMesh`, `std::thread` in core loops, or 64-bit atomics.
- The generator is deterministic (same inputs, same outputs), and overlapping worlds agree where they overlap.
- The Rust and WESL noise golden tests pass.
- No cracks or T-junctions between CDLOD rings. No z-fighting at the voxel–terrain blend edge.
- Collider count stays bounded no matter how big the world is.
- Performance targets *(call)*: 60 fps at 1440p on a mid-range desktop GPU; 30+ fps (aiming for 60) in Chrome WebGPU on an M1-class laptop. The desktop target is measured on the Linux machine. Browser numbers from the development Mac (Apple M4) are provisional against the M1-class reference.
- Crash flow: a forced panic and a forced abort each lead to the next-run notice pointing at the right log, on native and (panic only) on the web.
- For the globe milestone, verify the full bake. Record cold-load time to the first interactive globe frame, transferred bytes, decoded texture bytes, peak client memory, and frame times during a full-range scrub.

## 16. Left to the builder

- The exact Bevy 0.20 integration route for the CDLOD pipeline (custom render pipeline vs. another route), provided lighting matches across levels.
- Noise family and lattice scheme; erosion and hydrology detail.
- Plate mesh tessellation and the visual treatment of ocean-filled boundaries within the §7.1 lookup and overlap rules.
- Data hosting for the web; preprocessing tool language.
- Painterly post-process, if any.
- Which CSV rows become extra snapshots.
- Distribution channels and hosting for the free release.
- Module layout. Suggested plugins: `WorldTreePlugin` (levels, zoom-and-swap), `TimePlugin` (slider, snapshots), `GlobePlugin`, `TerrainCdlodPlugin`, `VoxelDetailPlugin`, `StreetSitePlugin`, `SettlementPlugin`, `ShipsPlugin`, `WaterPlugin`, `CreaturePlugin`, `CulturePlugin` (CSV and captions), `AmbiencePlugin`. Order them with explicit `SystemSet`s. Module boundaries follow the bounded contexts in §19.

## 17. Content sourcing

There's no dedicated artist or audio designer, so content is **mostly generated, supplemented by CC0 libraries**:

- **Generated:** terrain, era vegetation (procedural plant meshes or cards from a small set of parametric species per vegetation era), building-kit variation, site layouts, painterly materials (noise-driven, so few textures are needed), and the sky.
- **CC0 libraries:** base meshes for kit pieces and landmarks where they fit, ambient sound recordings (wind, water, insects, birds), and any texture detail. Each asset is recorded in a manifest with its source and licence, even under CC0.
- **Hand-made by the builder:** the creature (simple, stylised, animated procedurally rather than with keyframes where possible) and the first site's kit.
- The plan should keep each era's vegetation set and each site's kit small, then grow them. Painterly naturalism is what makes a small content set acceptable.

## 18. Leaving room for ambient fauna (later version)

Ambient fauna is deferred, not dropped. To keep adding it cheap later:

- The generator exposes **habitat queries**: biome, era, water depth, vegetation density and settlement proximity at any crust position. Fauna spawning will need the same inputs.
- The era- and biome-aware **fauna tables used for ambient sound** (§13) are structured so they can later become spawn tables (species, habitat, era range, group size).
- Leave a reserved `AmbientLifePlugin` slot in the plugin and `SystemSet` ordering, and reserve draw-call and frame-time headroom (about 10–15%, *(call)*) in the performance budget.

## 19. Architecture: functional core, imperative shell, domains

**Functional core, imperative shell.** Domain logic lives in pure functions over immutable values. The same inputs give the same outputs, with no I/O, clocks, globals or unseeded randomness. This matches the world model: a world is already a function (§5.2). The shell owns everything with effects: Bevy systems, asset loading, GPU resources, input, files and logging.

A shell system follows *gather → decide → apply*: read components and resources, call a pure function, write the result. Any branch that changes what the player sees belongs in the pure function, where a unit test can reach it. Shaders are pure functions of their bound data. Logic that must agree between CPU and GPU (footprint acceptance, sea level, endpoint selection) keeps a CPU reference in the core, and tests compare the two.

This stops where it stops paying. Render pipelines, mesh upload and per-frame GPU plumbing are shell code by nature; don't wrap them in domain abstractions.

**Bounded contexts** *(call)*:

| Context | Owns | Lives in |
|---|---|---|
| Deep time | game age, slider scale, snapshots, reference year, labels | core `deep_time` |
| Geodynamics | plates, rotations, footprints, crust positions, the resolver | core `geodynamics` |
| Paleo-surface | the timeline of frames, the bridge, sea level, frame selection | core `surface` |
| Shared kernel | geographic and render frames, the cube-map convention | core `geo`, `cube` |
| Data ingestion | translating external datasets into the game's terms | bake tool |
| Presentation | globe meshes, materials, UI, camera | game crate (shell) |
| Diagnostics | logging, tracing, crash reports | game crate (generic subdomain, §20) |

Later milestones add world tree, terrain generation, culture, settlements, maritime and ambience contexts.

The bake tool is the **anti-corruption layer**. Source quirks (PALEOMAP plate numbering, netCDF axis order, ICE-6G_C percentages, the 1950 BP convention) never leak past it. The one source fact the domain keeps is a PaleoDEM's source reconstruction age, because rendering needs it.

**Value objects.** Use a newtype wherever two quantities share a representation but must never mix: stable plate ID vs dense plate index, geographic vs render-frame direction. Elsewhere use unit suffixes (`_myr`, `_m`, `_km`).

**Aggregates** have validating constructors, so the shell only ever holds valid ones: the crust model (rotations and footprints with consistent indices) and the timeline (ordered frames, a bridge, a sea-level curve). A failed validation is a load error, never a panic.

**Domain events** (time changed, load state changed, frame selection changed, and later dives and world swaps) are Bevy messages. Registering one also registers its logger (§20).

**Ubiquitous language.** Code uses these terms exactly:
- **Game age:** years before 2026 CE.
- **Timeline age:** where a frame sits on the slider. **Source reconstruction age:** the age the source data was reconstructed for. The two can differ.
- **Snapshot:** a standard stop on the slider. **Frame:** one baked source image at a timeline age.
- **Bridge:** the transition from the recent (ICE-6G_C) globe to the deep-time globe.
- **Plate ID:** the source model's stable plate number. **Plate index:** a dense index inside one bake.
- **Crust position:** plate ID plus plate-fixed latitude/longitude. **Era-fixed position:** snapshot age plus geographic latitude/longitude, used where no crust is tracked.
- **Footprint:** where a plate exists, per cell, with valid age intervals. **Rotation circuit:** the chain of rotations that places a plate; it can be missing.
- **Ocean shell:** the sphere drawn under plate pieces to fill gaps.
- **Level / world / dive:** a tier of the world tree, one generated instance of it, and the zoom-and-swap descent.

## 20. Diagnostics and crash reporting

The goal is to debug a problem from what the session recorded, without reproducing it.

**Tracing as an aspect.** Instrumentation sits at seams, not scattered through function bodies *(call)*:
- Bevy's `trace` feature wraps every system and schedule in a span.
- Shell boundaries (asset loaders, file I/O, GPU resource creation, plugin setup) carry `#[instrument]` attributes that record identifying fields and log returned errors.
- Registering a domain event or a state also registers a generic logger for it, so every time change, load-state transition and frame selection is logged without per-site code.
- The core gets attributes only on coarse entry points (parsers, aggregate constructors, the resolver). It never instruments per-sample functions and has no log statements in function bodies.
- The bake tool applies a `@traced` decorator to every public function, and a test enforces coverage.

**Where records go:**
- The console (terminal or browser console).
- A per-session log file on native, in the platform's log location: macOS `~/Library/Logs/Time Fly`, Linux `$XDG_STATE_HOME/time-fly/logs` (default `~/.local/state/time-fly/logs`), Windows `%LOCALAPPDATA%\Time Fly\logs`. Keep the last 10 sessions.
- An in-memory **flight recorder** of the most recent detailed events. It goes into the crash report on a panic and into the session log at the first error. On the web it is saved to browser storage every few seconds and when the page hides.

**Crash detection.** Each session records its build, platform, adapter and status, and holds a lock while it runs. A clean exit marks it clean. At the next launch, a session still marked running whose lock is free has crashed or been killed. Panics, aborts, driver crashes, out-of-memory kills and power loss all look the same from there, so the notice says the game "didn't close normally". A panic also writes a crash report: message, location, backtrace, build, last known game state and recent events.

**The notice.** The next run tells the player where the log is and asks them to send it to the developer. Buttons show the file (native) or download it (web), open the report page, or dismiss. The notice appears once per crash. If the crash came before the first frame, native builds show a system dialog before the renderer starts and the web shows an HTML overlay, so a broken GPU path can't hide the notice.

**Privacy.** Nothing is uploaded. Logs replace the home directory with `~` and contain no user names or host names, and the notice says what the log contains. The report page is the repository's issue tracker *(call; the owner can change it)*.
