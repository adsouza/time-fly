# Bevy 0.20 Hybrid LOD & Procedural Street-Scene Architecture Guidelines

**Target Engine:** Bevy `0.20` (`wgpu` backend)  
**Target Graphics APIs:** WebGPU (`wasm32-unknown-unknown`) & Vulkan (Desktop)  
**Locked Physics Engine:** `avian3d = "0.8"` (exclusively)  

---

## 1. Non-Negotiable Platform & Feature Guardrails

Because the application must compile and run identically on both **WebGPU** and **Vulkan**, the coding agent MUST enforce WebGPU's baseline capabilities as the hard ceiling for all rendering, compute, and threading code:

1. **Strictly Prohibited Features:**
   - **NO Mesh Shaders** (`Task` / `Mesh` shader pipelines).
   - **NO Bevy Experimental Virtual Geometry (`MeshletMesh`)** or `SHADER_INT64_ATOMIC_MIN_MAX`.
   - **NO Unbounded Bindless Texture Arrays** or `binding_array` features unavailable in baseline WebGPU.
   - **NO OS Threads (`std::thread`)** or unconfigured `SharedArrayBuffer` assumptions for core game loops; rely on `AsyncComputeTaskPool` / `ComputeTaskPool` or GPU compute shaders.
   - **NO `bevy_rapier3d` or `bevy_gradmesh`** in the core rendering/physics pipeline.
2. **Required Rendering Standards:**
   - Use standard WGSL / Bevy `0.20` WESL vertex, fragment, and compute shaders (`@compute @workgroup_size(...)`).
   - Use **32-bit atomics (`atomic<u32>`)** exclusively for GPU buffer allocation and prefix-sum compaction.
   - Use pre-allocated GPU `Buffer` pools and `draw_indexed_indirect` for dynamic procedural geometry.
   - Compress 2D textures as `.ktx2` (Basis Universal / UASTC) to stay within WebGPU memory budgets.

---

## 2. Locked Crate Stack & ECS Conventions

- **Engine:** `bevy = "0.20"` (enable `webgpu` feature on `wasm32` targets).
- **Physics:** `avian3d = "0.8"` (with `f64` feature if planetary coordinate scales exceed `10 km`).
- **Deterministic Noise:** `fastnoise-lite = "1.1.1"` on CPU, mirrored mathematically in WGSL/WESL shaders so CPU queries and GPU geometry match identically.
- **Asset Preprocessing:** Offline `meshoptimizer` CLI/build pipeline to generate discrete LOD tiers (`LOD0`–`LOD3`) for pre-built glTF assets.
- **ECS Structure:** Organize every subsystem into a dedicated Bevy `Plugin` (`MacroTerrainPlugin`, `VoxelDetailPlugin`, `UrbanBlocksPlugin`, `LandmarkAssetsPlugin`, `UnderwaterPlugin`, `ProximityPhysicsPlugin`) scheduled via explicit `SystemSet` ordering.

---

## 3. Four-Tier Hybrid LOD Architecture

### Tier 1: Macro Terrain Shell (`terrain_cdlod.wesl`)
- **Purpose:** Planetary/regional ground shell, ocean floor baseline, and graded urban ground beds.
- **Technique:** Quadtree or concentric clipmap rings instancing a single static grid patch (`32x32` or `64x64` vertices) with **vertex-shader geomorphing (CDLOD)**.
- **Implementation Rules:**
  - Compute per-vertex `morph_factor` (`0.0` to `1.0`) from camera distance in `terrain_cdlod.wesl`, snapping fine vertices onto `2x2` parent grid cells and re-sampling `H(x, z)` via `textureSampleLevel` to prevent seams and T-junctions.
  - Flatten or grade `H(x, z)` smoothly across street corridors and city block footprints.
  - In the fragment shader, `discard` macro terrain fragments inside `voxel_window.w` (`inner_clip_radius`) so Tier 2 voxels render without z-fighting.

### Tier 2: Read-Only Procedural Voxels (`voxel_surface_nets.wesl`)
- **Purpose:** Close-range natural surface detail (cliffs, overhangs, arches, reefs, uneven seafloors) within a sliding window (`~128m–256m`) around the camera.
- **Technique:** Zero-persistence, deterministic **GPU Compute Naive Surface Nets** (3-pass: density sampling, vertex generation, quad/triangle index emission) writing into pre-allocated indirect draw buffer pools.
- **Implementation Rules:**
  - **No Disk/RAM Serialization:** Voxels are strictly read-only procedural detail; discard chunks outside the sliding window.
  - **Heightmap Coupling:** Evaluate density as `D(x, y, z) = H(x, z) - y + Noise3D(x, y, z) * Mask(x, z)`.
  - **Boundary Stitching:** Attenuate `Mask(x, z)` smoothly to `0.0` at the outer perimeter of the voxel window so voxel vertices converge flush onto `H(x, z)`.
  - **Urban Exclusion:** Force `Mask(x, z) = 0.0` (and skip chunk dispatch) inside flat manicured city districts to save GPU compute bandwidth.

### Tier 3: Procedural City Blocks & Street Scenes
- **Purpose:** Dense urban environments, streets, sidewalks, and multi-story buildings without heightmap stretching.
- **Technique:** Deterministic grammar / Wave-Function-Collapse (WFC) city block generator assembling reusable modular architectural kits, paired with **Hierarchical LOD (HLOD)**.
- **Implementation Rules:**
  - **Automatic GPU Batching:** Spawn modular exterior pieces (wall bays, windows, cornices, storefronts, streetlights, road curbs) using shared `Handle<Mesh>` and `Handle<StandardMaterial>` instances so Bevy `0.20` automatically batches identical modules into minimal draw calls.
  - **Module Discrete LODs:** Provide 2–3 discrete LOD tiers per modular kit piece controlled via Bevy's `VisibilityRange` (`start_margin` / `end_margin` dithering).
  - **City Block HLOD (Far Distance):** For distant city blocks beyond the modular cutoff radius, generate or merge a simplified single-mesh block proxy (with baked texture atlas) per block and swap it in via `VisibilityRange` so an entire distant city block costs **1 draw call**.
  - **Street Grounding:** Snap building foundations and street props to the exact graded macro height `H(x, z)` at placement time.

### Tier 4: Iconic High-Poly Static Landmarks & Interactive Assets
- **Purpose:** Signature hero buildings, enterable interiors, interactive machinery, vehicles, docks, and underwater wrecks.
- **Technique:** Pre-built glTF assets preprocessed offline with `meshoptimizer` into `LOD0`–`LOD3` child entities using `VisibilityRange`.
- **Implementation Rules:**
  - **Interior Culling:** For enterable buildings, separate the exterior shell from interior rooms/props. Keep interior entities hidden (`Visibility::Hidden`) until the player enters a proximity/doorway trigger volume or line-of-sight portal.
  - **Deterministic Grounding:** Place interactive assets on dry terrain, city plazas, or underwater seafloors using the exact deterministic height/SDF evaluation function.

---

## 4. Underwater & Water Surface Pipeline

- **Terrain & Voxel Continuity:** Both `terrain_cdlod.wesl` and `voxel_surface_nets.wesl` naturally represent submerged bathymetry and overhangs below `sea_level`.
- **Water Rendering:** Render a height-grid or projected water surface plane at `sea_level` with Fresnel reflection, refraction/depth tinting, and shoreline foam where `water_depth` approaches `0`.
- **Submerged Shading & Camera Transition:**
  - Apply Beer-Lambert exponential depth absorption and dual-layer animated caustics in terrain, voxel, and building shaders for surfaces below `sea_level`.
  - When `camera.y < sea_level`, switch global `DistanceFog` to dense underwater exponential scattering and apply subtle post-process color grading / particle particulates.

---

## 5. Physics & Collision Strategy (`avian3d = "0.8"`)

1. **No Full-World Terrain Colliders:** Never generate physics colliders for the entire macro terrain or distant city blocks.
2. **Sliding-Window Voxel Colliders:** Maintain a tight `32m–48m` active physics radius around the player. Generate coarse `RigidBody::Static` + `Collider::trimesh_from_mesh` entities only for active walkable/swimmable voxel chunks in that radius, despawning them immediately when the player moves away.
3. **Urban & Building Colliders:**
   - For procedural city blocks, use fast primitive **`Collider::cuboid` / `Collider::compound`** shapes on walls, floors, stairs, and sidewalks rather than high-poly trimeshes.
   - For iconic interactive glTF assets, use `ColliderConstructor::ConvexHullFromMesh` or `ColliderConstructor::TrimeshFromMesh` on simplified collision meshes.
4. **Swimming & Buoyancy:** Detect underwater state via `y < sea_level` and apply vertical buoyancy forces and fluid drag directly through Avian's `ExternalForce` / `LinearDamping` components.

---

## 6. Coding Agent Verification Checklist

Before marking any milestone complete, the agent MUST verify:
- [ ] `cargo check --target wasm32-unknown-unknown` succeeds with zero native-only feature leaks.
- [ ] Zero references to `MeshletMesh`, mesh shaders, `std::thread`, or `bevy_rapier3d`.
- [ ] Macro terrain exhibits zero visible cracks/T-junctions across CDLOD ring boundaries and zero z-fighting inside `inner_clip_radius`.
- [ ] Procedural voxel edges stitch seamlessly into `H(x, z)` at the outer window perimeter and skip dispatch inside flat urban zones.
- [ ] Distant city blocks collapse into single-draw-call HLOD proxies via `VisibilityRange`, and enterable landmark interiors remain culled until approached.
- [ ] Active `avian3d` collider count remains bounded within the player's local neighborhood regardless of total world size.
