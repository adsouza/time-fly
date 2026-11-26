use std::collections::BTreeSet;

use bevy::asset::AssetPlugin;
use timefly_core::deep_time::YearsBp;
use timefly_core::geo::{geo_to_render, latlon_to_geo};
use timefly_core::terrain::{curvature_drop_m, morph_vertex};
use timefly_core::worldgen::frame::RegionFrame;
use timefly_core::worldgen::generate::{Field, RegionSpec};
use timefly_core::worldgen::{APRON_SIZE_M, REGION_SIZE_M};

use super::*;
use crate::globe::GlobeCamera;
use view::TerrainSun;

/// Heights that vary on every axis: a tilted plane plus ripples, about −500 to 2,500 m.
fn field(res: u32, size_m: f64) -> Field {
    let mut elevation_m = Vec::new();
    for row in 0..res {
        for col in 0..res {
            let (x, z) = (f64::from(col), f64::from(row));
            elevation_m.push((1000.0 + 40.0 * x - 25.0 * z + 300.0 * (x * 0.7).sin()) as f32);
        }
    }
    Field::dry(
        res,
        size_m,
        elevation_m,
        vec![[0, 0, 0, 0]; (res * res) as usize],
    )
}

fn grid() -> Arc<RegionGrid> {
    let center = geo_to_render(latlon_to_geo(46.5, 10.0));
    Arc::new(RegionGrid {
        spec: RegionSpec {
            inner_res: 33,
            apron_res: 17,
            ..RegionSpec::new(center, YearsBp(0.0), 1)
        },
        inner: field(33, REGION_SIZE_M),
        apron: field(17, APRON_SIZE_M),
        boulders: Vec::new(),
    })
}

#[test]
fn pool_grows_in_chunks_and_never_shrinks() {
    assert_eq!(pool_len(0, 0), 0);
    assert_eq!(pool_len(0, 1), PATCH_POOL_CHUNK);
    assert_eq!(pool_len(0, PATCH_POOL_CHUNK), PATCH_POOL_CHUNK);
    assert_eq!(pool_len(0, PATCH_POOL_CHUNK + 1), 2 * PATCH_POOL_CHUNK);
    // Fewer patches never shrink the pool.
    assert_eq!(pool_len(5 * PATCH_POOL_CHUNK, 3), 5 * PATCH_POOL_CHUNK);
    // The ceiling holds.
    assert_eq!(pool_len(0, MAX_TERRAIN_PATCHES + 500), MAX_TERRAIN_PATCHES);
    assert_eq!(pool_len(MAX_TERRAIN_PATCHES, 10), MAX_TERRAIN_PATCHES);
    assert_eq!(MAX_TERRAIN_PATCHES % PATCH_POOL_CHUNK, 0);
}

/// Every camera Task 9 can reach over the region (±150 km, from the clearance to the ceiling)
/// selects at most [`MAX_TERRAIN_PATCHES`], so no frame drops a patch.
#[test]
fn pool_capacity_covers_every_camera() {
    let settings = LodSettings::region();
    let mut worst = (0usize, DVec3::ZERO);
    for altitude in [150.0, 400.0, 1_000.0, 2_000.0, 8_000.0, 20_000.0, 60_000.0] {
        for i in -30..=30 {
            for j in -30..=30 {
                let camera = DVec3::new(f64::from(i) * 5_000.0, altitude, f64::from(j) * 5_000.0);
                let n = select_patches(camera, &settings).len();
                if n > worst.0 {
                    worst = (n, camera);
                }
            }
        }
    }
    println!("worst selection: {} patches at {}", worst.0, worst.1);
    assert!(worst.0 <= MAX_TERRAIN_PATCHES, "{worst:?}");
    // The ceiling is not wildly oversized either: within two chunks of the worst case.
    assert!(
        MAX_TERRAIN_PATCHES < worst.0 + 2 * PATCH_POOL_CHUNK,
        "{worst:?}"
    );
}

#[test]
fn plan_packs_the_selection_with_bounds() {
    let settings = LodSettings::region();
    let range = [-500.0, 2_500.0];
    let camera = Vec3::new(12_000.0, 1_500.0, -40_000.0);
    let plan = plan_patches(camera, &settings, range, 0);
    let patches = select_patches(camera.as_dvec3(), &settings);
    assert_eq!(plan.instances, patch_instances(&patches, &settings));
    assert_eq!(plan.bounds.len(), patches.len());
    assert_eq!(plan.pool_len, pool_len(0, patches.len()));
    assert_eq!(plan.dropped, 0);
    // An oversized pool is kept.
    let kept = plan_patches(camera, &settings, range, MAX_TERRAIN_PATCHES);
    assert_eq!(kept.pool_len, MAX_TERRAIN_PATCHES);
}

#[test]
fn bounds_hold_every_displaced_vertex() {
    let settings = LodSettings::region();
    let range = [-500.0f32, 2_500.0f32];
    for camera in [
        Vec3::new(0.0, 150.0, 0.0),
        Vec3::new(140_000.0, 8_000.0, -90_000.0),
        Vec3::new(-73_210.0, 60_000.0, 41_007.0),
    ] {
        for patch in select_patches(camera.as_dvec3(), &settings) {
            let aabb = patch_bounds(&patch, range, camera);
            let (lo, hi) = (aabb.min(), aabb.max());
            let n = settings.patch_quads;
            for (i, j) in [(0, 0), (n, n), (1, n - 1), (n - 1, 1), (n / 2, n / 2 + 1)] {
                let [x, z] = morph_vertex([i, j], &patch, camera.to_array(), &settings);
                let d = Vec2::new(x - camera.x, z - camera.z).length();
                let drop = curvature_drop_m(f64::from(d)) as f32;
                for h in range {
                    let p = Vec3::new(x, h - drop, z);
                    let inside = p.cmpge(Vec3::from(lo) - 1e-3).all()
                        && p.cmple(Vec3::from(hi) + 1e-3).all();
                    assert!(inside, "{p} outside {lo:?}..{hi:?} for {patch:?}");
                }
            }
        }
    }
}

#[test]
fn grid_mesh_splits_quads_on_the_rising_diagonal() {
    let mesh = grid_mesh(4);
    let positions = mesh
        .attribute(Mesh::ATTRIBUTE_POSITION)
        .and_then(|a| a.as_float3())
        .unwrap();
    assert_eq!(positions.len(), 25);
    assert_eq!(positions[0], [0.0, 0.0, 0.0]);
    assert_eq!(positions[24], [1.0, 0.0, 1.0]);
    // Index (i, j) sits at (i, 0, j) / quads; i runs fastest.
    assert_eq!(positions[1 + 5 * 3], [0.25, 0.0, 0.75]);
    let Some(Indices::U32(indices)) = mesh.indices() else {
        panic!("u32 indices");
    };
    assert_eq!(indices.len(), 4 * 4 * 6);
    let grid = |v: u32| {
        let p = positions[v as usize];
        ((p[0] * 4.0) as i32, (p[2] * 4.0) as i32)
    };
    for tri in indices.chunks(3) {
        let [a, b, c] = [tri[0], tri[1], tri[2]].map(|v| Vec3::from(positions[v as usize]));
        // Faces +y (front faces wind counter-clockwise seen from above).
        assert!((b - a).cross(c - a).y > 0.0, "{tri:?}");
        // Each triangle holds its quad's (i, j)–(i+1, j+1) diagonal.
        let corners: Vec<(i32, i32)> = tri.iter().map(|&v| grid(v)).collect();
        let (i0, j0) = (
            corners.iter().map(|c| c.0).min().unwrap(),
            corners.iter().map(|c| c.1).min().unwrap(),
        );
        assert!(corners.contains(&(i0, j0)) && corners.contains(&(i0 + 1, j0 + 1)));
    }
}

#[test]
fn debug_camera_looks_north_and_down_from_above_the_centre() {
    let grid = grid();
    let ground = timefly_core::terrain::blended_height_m(&grid, 0.0, 0.0);
    assert!(ground > 0.0);
    let t = view::debug_camera_transform(&grid);
    assert_eq!(t.translation, Vec3::new(0.0, ground + 8_000.0, 0.0));
    let forward = t.forward().as_vec3();
    assert!(forward.z < 0.0 && forward.x.abs() < 1e-6, "{forward}");
    assert!(
        (forward.y.asin().to_degrees() + 20.0).abs() < 1e-4,
        "{forward}"
    );
}

// ---------------------------------------------------------------------------------------------
// The pool in a headless app (no renderer)

fn pool_app() -> App {
    let mut app = App::new();
    app.add_plugins((MinimalPlugins, AssetPlugin::default()))
        .init_asset::<Image>()
        .init_asset::<Mesh>()
        .init_asset::<ShaderBuffer>()
        .init_asset::<TerrainMaterial>();
    add_terrain_systems(&mut app);
    // The globe camera, as GlobePlugin spawns it.
    app.world_mut()
        .spawn((GlobeCamera, Camera::default(), IsDefaultUiCamera));
    app
}

fn enter(app: &mut App, grid: &Arc<RegionGrid>) {
    app.insert_resource(ActiveRegion {
        grid: grid.clone(),
        frame: RegionFrame {
            center: grid.spec.center,
        },
    });
}

fn patch_entities(app: &mut App) -> BTreeSet<Entity> {
    let world = app.world_mut();
    world
        .query_filtered::<Entity, With<TerrainPatch>>()
        .iter(world)
        .collect()
}

fn shown_slots(app: &mut App) -> BTreeSet<u32> {
    let world = app.world_mut();
    world
        .query::<(&TerrainPatch, &Visibility)>()
        .iter(world)
        .filter(|(_, v)| **v != Visibility::Hidden)
        .map(|(p, _)| p.slot)
        .collect()
}

fn set_camera(app: &mut App, at: Vec3) {
    let world = app.world_mut();
    let mut q = world.query_filtered::<&mut Transform, With<RegionCamera>>();
    q.single_mut(world).expect("one region camera").translation = at;
}

fn cameras(app: &mut App) -> (bool, bool, bool, bool) {
    let world = app.world_mut();
    let mut g = world.query_filtered::<(&Camera, Has<IsDefaultUiCamera>), With<GlobeCamera>>();
    let (gc, gu) = g.single(world).map(|(c, u)| (c.is_active, u)).unwrap();
    let mut r = world.query_filtered::<(&Camera, Has<IsDefaultUiCamera>), With<RegionCamera>>();
    let (rc, ru) = r
        .single(world)
        .map(|(c, u)| (c.is_active, u))
        .unwrap_or((false, false));
    (gc, gu, rc, ru)
}

#[test]
fn patch_pool_reuses_entities() {
    let grid = grid();
    let mut app = pool_app();
    enter(&mut app, &grid);
    app.update();
    let settings = LodSettings::region();
    let mut seen = patch_entities(&mut app);
    assert!(!seen.is_empty());
    // Fly a path: low over the centre, out to a corner, up to the ceiling and back down.
    let path: Vec<Vec3> = (0..=60)
        .map(|k| {
            let t = k as f32 / 60.0;
            let alt = 150.0 + 59_850.0 * (std::f32::consts::PI * t).sin();
            Vec3::new(140_000.0 * t, alt, -140_000.0 * t * (1.0 - t) * 4.0)
        })
        .collect();
    let mut largest = 0;
    for at in path {
        set_camera(&mut app, at);
        app.update();
        let now = patch_entities(&mut app);
        assert!(now.len() <= MAX_TERRAIN_PATCHES, "{} at {at}", now.len());
        assert!(
            now.is_superset(&seen),
            "a patch entity was despawned at {at}"
        );
        assert_eq!(now.len() % PATCH_POOL_CHUNK, 0);
        let selected = select_patches(at.as_dvec3(), &settings).len();
        let want: BTreeSet<u32> = (0..selected as u32).collect();
        assert_eq!(shown_slots(&mut app), want, "at {at}");
        largest = largest.max(now.len());
        seen = now;
    }
    // Slots and tags agree and are unique.
    let world = app.world_mut();
    let tags: BTreeSet<(u32, u32)> = world
        .query::<(&TerrainPatch, &MeshTag)>()
        .iter(world)
        .map(|(p, t)| (p.slot, t.value))
        .collect();
    assert_eq!(tags.len(), seen.len());
    assert!(tags.iter().all(|(s, t)| s == t));
    assert_eq!(largest, seen.len());

    // The buffer holds this frame's camera and patches.
    let scene = app.world().resource::<TerrainScene>();
    let buffer = app
        .world()
        .resource::<Assets<ShaderBuffer>>()
        .get(&scene.buffer)
        .unwrap();
    let bytes = buffer.cast_slice::<u8>().unwrap();
    let params: TerrainParams =
        bytemuck::pod_read_unaligned(&bytes[..material::TERRAIN_PARAMS_BYTES]);
    let last = Vec3::new(140_000.0, 150.0, 0.0);
    assert!(
        Vec3::from_slice(&params.camera).distance(last) < 1.0,
        "{params:?}"
    );
    assert_eq!(
        bytes.len(),
        material::TERRAIN_PARAMS_BYTES + MAX_TERRAIN_PATCHES * material::PATCH_INSTANCE_BYTES
    );
}

#[test]
fn the_region_camera_takes_over_while_the_terrain_is_shown() {
    let grid = grid();
    let mut app = pool_app();
    app.update();
    assert_eq!(cameras(&mut app), (true, true, false, false));
    enter(&mut app, &grid);
    app.update();
    app.update();
    assert_eq!(cameras(&mut app), (false, false, true, true));
    // The region camera has no clusters (ruling R28; the pool app has no observer to add it).
    let world = app.world_mut();
    let region_camera = world.resource::<TerrainScene>().camera;
    assert!(matches!(
        world.get::<bevy::light::cluster::ClusterConfig>(region_camera),
        Some(bevy::light::cluster::ClusterConfig::None)
    ));
    // Hidden: the globe comes back and every patch hides.
    app.world_mut().resource_mut::<TerrainSettings>().visible = false;
    app.update();
    assert_eq!(cameras(&mut app), (true, true, false, false));
    assert!(shown_slots(&mut app).is_empty());
    app.world_mut().resource_mut::<TerrainSettings>().visible = true;
    app.update();
    assert_eq!(cameras(&mut app), (false, false, true, true));
}

/// Review Focus 5: leaving a region despawns the pool and the view and frees every texture,
/// buffer, mesh and material.
#[test]
fn leaving_frees_the_terrain() {
    let grid = grid();
    let mut app = pool_app();
    enter(&mut app, &grid);
    for _ in 0..3 {
        app.update();
    }
    assert!(!patch_entities(&mut app).is_empty());
    assert_eq!(app.world().resource::<Assets<Image>>().len(), 7);
    // A replaced region (a snapshot change) swaps the six field textures.
    let replaced = Arc::new((*grid).clone());
    enter(&mut app, &replaced);
    app.update();
    app.update();
    assert_eq!(app.world().resource::<Assets<Image>>().len(), 7);
    assert!(Arc::ptr_eq(
        &app.world().resource::<TerrainScene>().grid,
        &replaced
    ));

    app.world_mut().remove_resource::<ActiveRegion>();
    // Freeing cascades over a few frames: the entities' material handles, then the material,
    // then the textures it held.
    for _ in 0..4 {
        app.update();
    }
    assert!(app.world().get_resource::<TerrainScene>().is_none());
    assert!(patch_entities(&mut app).is_empty());
    assert_eq!(cameras(&mut app), (true, true, false, false));
    let world = app.world_mut();
    assert_eq!(world.query::<&TerrainSun>().iter(world).count(), 0);
    assert_eq!(world.resource::<Assets<Image>>().len(), 0);
    assert_eq!(world.resource::<Assets<ShaderBuffer>>().len(), 0);
    assert_eq!(world.resource::<Assets<Mesh>>().len(), 0);
    assert_eq!(world.resource::<Assets<TerrainMaterial>>().len(), 0);
    drop(grid);
    assert_eq!(
        Arc::strong_count(&replaced),
        1,
        "the scene still holds the grid"
    );
}

#[test]
fn every_region_look_keeps_the_dune_switch() {
    assert!(TerrainOptions::default().dunes);
    let grid = grid();
    let mut deep = (*grid).clone();
    deep.spec.snapshot = YearsBp(540.0e6);
    for g in [&*grid, &deep] {
        assert_eq!(look_for(g, TerrainOptions { dunes: false }).wind.w, 0.0);
        assert_eq!(look_for(g, TerrainOptions::default()).wind.w, 1.0);
    }
}

#[test]
fn the_scene_takes_the_dune_switch_from_the_options() {
    let grid = grid();
    let mut app = pool_app();
    app.insert_resource(TerrainOptions { dunes: false });
    enter(&mut app, &grid);
    for _ in 0..3 {
        app.update();
    }
    let look = |app: &App| {
        let world = app.world();
        let scene = world.resource::<TerrainScene>();
        world
            .resource::<Assets<TerrainMaterial>>()
            .get(&scene.material)
            .unwrap()
            .extension
            .look
    };
    assert_eq!(look(&app).wind.w, 0.0);
    // A snapshot change rebuilds the look; the switch survives it.
    let mut deep = (*grid).clone();
    deep.spec.snapshot = YearsBp(540.0e6);
    enter(&mut app, &Arc::new(deep));
    for _ in 0..3 {
        app.update();
    }
    assert_eq!(look(&app).wind.w, 0.0);
}

/// A field with water on every texel: a level above the ground on even columns (a lake or a
/// river), below it on odd ones (dry), and river texels whose four values differ everywhere.
fn wet_field(res: u32, size_m: f64) -> Field {
    use timefly_core::worldgen::generate::water::encode_river;
    let mut f = field(res, size_m);
    for i in 0..(res * res) as usize {
        let (row, col) = (i as u32 / res, i as u32 % res);
        let h = f.elevation_m[i];
        f.water_level_m[i] = if col % 2 == 0 {
            h + 3.25 + row as f32
        } else {
            h - 64.0
        };
        // Distinct values within f16's range and precision.
        let k = (i % 97) as f32;
        f.river[i] = encode_river(
            12.5 - 1.5 * k,
            0.25 + 0.125 * k,
            -0.75 + 0.0625 * k,
            -40.0 + 2.5 * k,
        );
    }
    f
}

/// A water texel's words as WGSL reads them: `[W, W − h]` from `bitcast<f32>` and
/// `[ribbon, offset, flow_x, flow_z]` from `unpack2x16float`.
fn decode_water_texel(data: &[u8], index: usize) -> ([f32; 2], [f32; 4]) {
    use timefly_core::worldgen::generate::water::decode_river;
    let at = index * material::WATER_TEXEL_BYTES;
    let word = |k: usize| u32::from_le_bytes(data[at + 4 * k..at + 4 * k + 4].try_into().unwrap());
    let half = |w: u32| [(w & 0xFFFF) as u16, (w >> 16) as u16];
    let [ribbon, offset] = half(word(2));
    let [flow_x, flow_z] = half(word(3));
    let [ribbon, flow_x, flow_z, offset] = decode_river([ribbon, flow_x, flow_z, offset]);
    (
        [f32::from_bits(word(0)), f32::from_bits(word(1))],
        [ribbon, offset, flow_x, flow_z],
    )
}

#[test]
fn the_water_texture_holds_level_thickness_ribbon_and_flow() {
    use timefly_core::worldgen::generate::water::decode_river;
    let f = wet_field(5, 1_000.0);
    let image = material::water_image(&f);
    assert_eq!(
        image.texture_descriptor.format,
        bevy::render::render_resource::TextureFormat::Rgba32Uint
    );
    assert_eq!(image.size(), UVec2::new(5, 5));
    assert_eq!(image.asset_usage, RenderAssetUsages::RENDER_WORLD);
    let data = image.data.as_ref().unwrap();
    assert_eq!(data.len(), 25 * material::WATER_TEXEL_BYTES);
    for i in 0..25 {
        let ([level, thickness], [ribbon, offset, flow_x, flow_z]) = decode_water_texel(data, i);
        let w = f.water_level_m[i];
        let [want_ribbon, east, north, want_offset] = decode_river(f.river[i]);
        assert_eq!(level.to_bits(), w.to_bits(), "texel {i}");
        assert_eq!(thickness, w - f.elevation_m[i], "texel {i}");
        assert_eq!((ribbon, offset), (want_ribbon, want_offset), "texel {i}");
        // Region +z points south: flow_z is minus the north flow.
        assert_eq!((flow_x, flow_z), (east, -north), "texel {i}");
    }
    // Texel (col 3, row 2) is index 2 × 5 + 3, as in the height texture; it is dry.
    let ([level, thickness], _) = decode_water_texel(data, 13);
    assert_eq!(level, f.elevation_m[13] - 64.0);
    assert_eq!(thickness, -64.0);
    // A lake texel stands above its ground.
    assert!(decode_water_texel(data, 12).0[1] > 0.0);
}

/// The texture carries everything `blended_water` reads: a reader that interpolates the decoded
/// texels as the shader will (the half-width `ribbon + |offset|` and the offset, then
/// `half-width − |offset|`) gets the core's sample, inside the inner field.
#[test]
fn the_water_texture_rebuilds_blended_water() {
    use timefly_core::terrain::{BLEND_APRON_TEXELS, blended_water};
    // An apron fine enough (9.4 km) that the blend toward it starts 112.5 km out.
    let mut grid = (*grid()).clone();
    grid.spec.apron_res = 257;
    grid.inner = wet_field(33, REGION_SIZE_M);
    grid.apron = wet_field(257, APRON_SIZE_M);
    let image = material::water_image(&grid.inner);
    let data = image.data.as_ref().unwrap();
    let f = &grid.inner;
    let e0 = f.size_m / 2.0 - BLEND_APRON_TEXELS * grid.apron.spacing_m();
    let mut checked = 0;
    for (x, z) in [
        (0.0, 0.0),
        (1_234.5, -6_789.0),
        (-50_000.3, 20_000.7),
        (e0 - 1.0, -e0 + 1.0),
        (4_687.5, 9_375.0),
    ] {
        let (u, v) = (
            (x + f.size_m / 2.0) / f.spacing_m(),
            (z + f.size_m / 2.0) / f.spacing_m(),
        );
        let (c0, r0) = (u.floor(), v.floor());
        let (fu, fv) = ((u - c0) as f32, (v - r0) as f32);
        let texel = |r: f64, c: f64| decode_water_texel(data, (r as usize) * 33 + c as usize);
        let lerp2 = |q: &dyn Fn(([f32; 2], [f32; 4])) -> f32| {
            let top = q(texel(r0, c0)) + (q(texel(r0, c0 + 1.0)) - q(texel(r0, c0))) * fu;
            let bottom = q(texel(r0 + 1.0, c0))
                + (q(texel(r0 + 1.0, c0 + 1.0)) - q(texel(r0 + 1.0, c0))) * fu;
            top + (bottom - top) * fv
        };
        let level = lerp2(&|t| t.0[0]);
        let thickness = lerp2(&|t| t.0[1]);
        let hw = lerp2(&|t| t.1[0] + t.1[1].abs());
        let offset = lerp2(&|t| t.1[1]);
        let flow = [lerp2(&|t| t.1[2]), lerp2(&|t| t.1[3])];
        let want = blended_water(&grid, x, z);
        assert_eq!(level, want.level_m, "({x}, {z})");
        assert!(
            (thickness - want.thickness_m).abs() <= 1e-3,
            "({x}, {z}): {thickness} vs {want:?}"
        );
        assert_eq!(hw - offset.abs(), want.ribbon_m, "({x}, {z})");
        assert_eq!(flow, [want.flow[0], -want.flow[1]], "({x}, {z})");
        checked += 1;
    }
    assert_eq!(checked, 5);
}

/// What one upload costs: the six field textures' bytes, as `field_texture_bytes` reports them
/// (and the scene logs at `debug`). At the standard resolutions that is 24 B a texel over
/// 1,025² + 513² texels, 30.1 MiB, of which the water is 20.0 MiB (M3: 10.0 MiB in all).
#[test]
fn the_field_textures_cost_24_bytes_a_texel() {
    use timefly_core::worldgen::{APRON_RES, REGION_RES};
    let grid = grid();
    let mut images = Assets::<Image>::default();
    let fields = upload_fields(&grid, &mut images);
    let uploaded: usize = [
        &fields.inner_height,
        &fields.apron_height,
        &fields.inner_surface,
        &fields.apron_surface,
        &fields.inner_water,
        &fields.apron_water,
    ]
    .iter()
    .map(|h| images.get(*h).unwrap().data.as_ref().unwrap().len())
    .sum();
    assert_eq!(uploaded, field_texture_bytes(&grid));
    assert_eq!(uploaded, (33 * 33 + 17 * 17) * 24);
    let texels = (REGION_RES as usize).pow(2) + (APRON_RES as usize).pow(2);
    assert_eq!(texels, 1_313_794);
    assert_eq!(texels * material::WATER_TEXEL_BYTES, 21_020_704);
    assert_eq!(texels * 24, 31_531_056);
}

/// A grid for the swap tests: [`wet_field`]s raised by `lift_m`, so two of them tell apart.
fn wet_grid(lift_m: f32) -> RegionGrid {
    let mut grid = (*grid()).clone();
    grid.inner = wet_field(33, REGION_SIZE_M);
    grid.apron = wet_field(17, APRON_SIZE_M);
    for f in [&mut grid.inner, &mut grid.apron] {
        f.elevation_m.iter_mut().for_each(|h| *h += lift_m);
        f.water_level_m.iter_mut().for_each(|w| *w += lift_m);
    }
    grid
}

/// Enters `grid` as the region job does: the region holds the only reference.
fn enter_owned(app: &mut App, grid: RegionGrid) {
    let frame = RegionFrame {
        center: grid.spec.center,
    };
    app.insert_resource(ActiveRegion {
        grid: Arc::new(grid),
        frame,
    });
}

/// The first texel of an `R32Float` or an `Rgba32Uint` image, as f32 (the height, the level).
fn first_f32(app: &App, id: AssetId<Image>) -> f32 {
    let image = app.world().resource::<Assets<Image>>().get(id).unwrap();
    f32::from_le_bytes(image.data.as_ref().unwrap()[..4].try_into().unwrap())
}

/// The hop's memory (follow-up): once the fields are on the GPU, the active grid lets its river
/// texels go (8 B a texel, 10 MiB at full size). Nothing on the CPU reads them; the water
/// textures hold them.
#[test]
fn the_active_grid_frees_its_rivers_once_uploaded() {
    for replaced in [false, true] {
        let mut app = pool_app();
        if replaced {
            enter_owned(&mut app, wet_grid(0.0));
            for _ in 0..3 {
                app.update();
            }
        }
        let grid = wet_grid(500.0);
        let (full, river) = (grid.bytes(), grid.inner.river.clone());
        let freed = (33 * 33 + 17 * 17) * 8;
        enter_owned(&mut app, grid);
        for _ in 0..3 {
            app.update();
        }
        let world = app.world();
        let region = world.resource::<ActiveRegion>();
        let scene = world.resource::<TerrainScene>();
        assert!(
            Arc::ptr_eq(&region.grid, &scene.grid),
            "replaced {replaced}"
        );
        assert!(region.grid.inner.river.is_empty() && region.grid.apron.river.is_empty());
        assert_eq!(full - region.grid.bytes(), freed, "replaced {replaced}");
        // The water texture still carries them.
        let image = world
            .resource::<Assets<Image>>()
            .get(&scene.inner_water)
            .unwrap();
        let data = image.data.as_ref().unwrap();
        for i in [0, 17, 500, 1088] {
            let [ribbon, east, north, offset] =
                timefly_core::worldgen::generate::water::decode_river(river[i]);
            let (_, [r, o, x, z]) = decode_water_texel(data, i);
            assert_eq!([r, o, x, z], [ribbon, offset, east, -north], "texel {i}");
        }
    }
}

/// The hop's memory (third and fourth stages, the owner's rulings of 2026-11-25): a replace
/// spreads its uploads over four frames. The swap frame removes the old water (the render world
/// frees it a frame later); the next makes the new water, when the old is gone; the third
/// uploads nothing, so the water's staging drains; the fourth replaces the heights and surface
/// codes. Navigation keeps the cloud opaque for the frames that end incomplete (`holds_cloud`):
/// the swap frame and [`REPLACE_EXTRA_HELD_FRAMES`] more.
#[test]
fn a_replace_frees_the_old_water_before_making_the_new() {
    use crate::water::{RegionWater, WaterMaterial, add_water_systems};
    assert_eq!(
        REPLACE_EXTRA_HELD_FRAMES, 2,
        "the owner's rulings: two more frames in cloud"
    );
    let mut app = pool_app();
    app.init_asset::<crate::sky::SkyMaterial>()
        .init_asset::<WaterMaterial>();
    crate::sky::add_sky_systems(&mut app);
    add_water_systems(&mut app);
    enter_owned(&mut app, wet_grid(0.0));
    for _ in 0..3 {
        app.update();
    }
    let old = {
        let s = app.world().resource::<TerrainScene>();
        assert_eq!(
            (s.steps_remaining(), s.fields_pending(), s.holds_cloud()),
            (0, false, false)
        );
        [
            s.inner_height.id(),
            s.apron_height.id(),
            s.inner_surface.id(),
            s.apron_surface.id(),
            s.inner_water.id(),
            s.apron_water.id(),
        ]
    };
    let (old_h, new_h, new_w) = (
        first_f32(&app, old[0]),
        wet_grid(500.0).inner.elevation_m[0],
        wet_grid(500.0).inner.water_level_m[0],
    );
    enter_owned(&mut app, wet_grid(500.0));
    // The swap frame: the old water is gone and the new one not made yet; the old heights.
    app.update();
    {
        let s = app.world().resource::<TerrainScene>();
        assert_eq!(
            (s.steps_remaining(), s.fields_pending(), s.holds_cloud()),
            (3, true, true)
        );
        let images = app.world().resource::<Assets<Image>>();
        assert!(images.get(&s.inner_water).is_none() && images.get(&s.apron_water).is_none());
        assert!(
            images.get(old[4]).is_none() && images.get(old[5]).is_none(),
            "old water kept"
        );
        assert_eq!(s.inner_height.id(), old[0]);
        assert_eq!(first_f32(&app, s.inner_height.id()), old_h);
        assert!(Arc::ptr_eq(
            &s.grid,
            &app.world().resource::<ActiveRegion>().grid
        ));
        assert!(
            s.grid.inner.river.is_empty(),
            "the rivers freed on the swap frame"
        );
        assert_eq!(images.len(), 5, "no water set at all");
    }
    // The next frame: the new water, still the old heights; then a frame that uploads nothing.
    for (k, left) in [(1, 2), (2, 1)] {
        app.update();
        let s = app.world().resource::<TerrainScene>();
        assert_eq!(
            (s.steps_remaining(), s.fields_pending(), s.holds_cloud()),
            (left, true, left > 1),
            "swap + {k}"
        );
        assert_eq!(first_f32(&app, s.inner_water.id()), new_w);
        assert_eq!(first_f32(&app, s.inner_height.id()), old_h);
        assert_eq!(s.inner_height.id(), old[0], "swap + {k}: heights untouched");
        assert_eq!(app.world().resource::<Assets<Image>>().len(), 7);
    }
    // The fourth: the new heights and surfaces; the old ones go.
    app.update();
    let world = app.world();
    let s = world.resource::<TerrainScene>();
    assert_eq!(
        (s.steps_remaining(), s.fields_pending(), s.holds_cloud()),
        (0, false, false)
    );
    assert_eq!(first_f32(&app, s.inner_height.id()), new_h);
    let m = world
        .resource::<Assets<TerrainMaterial>>()
        .get(&s.material)
        .unwrap();
    assert_eq!(m.extension.inner_height, s.inner_height);
    assert_eq!(m.extension.apron_surface, s.apron_surface);
    assert_eq!(s.height_range_m, height_range_m(&s.grid));
    // The sea binds the new water (it reads no heights).
    let water = world.resource::<RegionWater>();
    let wm = world
        .resource::<Assets<WaterMaterial>>()
        .get(&water.material)
        .unwrap();
    assert_eq!(wm.extension.inner_water, s.inner_water);
    assert_eq!(wm.extension.apron_water, s.apron_water);
    let images = world.resource::<Assets<Image>>();
    assert!(
        old[..4].iter().all(|&id| images.get(id).is_none()),
        "old fields kept"
    );
    assert_eq!(images.len(), 7);
}

/// The swap frame binds no water (Task 7's stages), so the sea draws nothing until the new
/// water is made (Task 8): it is hidden, and its material holds neither the old water (which the
/// render world must free) nor the default image (a float texture where `texture_2d<u32>` is
/// bound). The frame after, it shows the new water.
#[test]
fn the_sea_draws_nothing_while_its_water_is_missing() {
    use crate::water::{NO_WATER, RegionWater, WaterMaterial, add_water_systems};
    let mut app = pool_app();
    app.init_asset::<crate::sky::SkyMaterial>()
        .init_asset::<WaterMaterial>();
    crate::sky::add_sky_systems(&mut app);
    add_water_systems(&mut app);
    enter_owned(&mut app, wet_grid(0.0));
    for _ in 0..3 {
        app.update();
    }
    let sea = |app: &App| {
        let world = app.world();
        let water = world.resource::<RegionWater>();
        let m = world
            .resource::<Assets<WaterMaterial>>()
            .get(&water.material)
            .unwrap();
        (
            *world.get::<Visibility>(water.entity).unwrap(),
            m.extension.inner_water.id(),
            m.extension.apron_water.id(),
        )
    };
    let scene_water = |app: &App| {
        let s = app.world().resource::<TerrainScene>();
        (s.inner_water.id(), s.apron_water.id())
    };
    let old = scene_water(&app);
    assert_eq!(sea(&app), (Visibility::Inherited, old.0, old.1));
    enter_owned(&mut app, wet_grid(500.0));
    // The swap frame.
    app.update();
    let (visibility, inner, apron) = sea(&app);
    assert_eq!(
        visibility,
        Visibility::Hidden,
        "the sea draws on the swap frame"
    );
    for id in [inner, apron] {
        assert_eq!(id, NO_WATER.id());
        assert!(id != old.0 && id != old.1, "the old water is still bound");
        assert_ne!(id, Handle::<Image>::default().id());
        assert!(app.world().resource::<Assets<Image>>().get(id).is_none());
    }
    // The next frame: the new water, shown.
    app.update();
    let new = scene_water(&app);
    assert_ne!(new, old);
    assert_eq!(sea(&app), (Visibility::Inherited, new.0, new.1));
    // And it stays bound through the heights' frames.
    for _ in 0..3 {
        app.update();
        assert_eq!(sea(&app), (Visibility::Inherited, new.0, new.1));
    }
}

/// A snapshot change uploads the new grid's water and drops the old textures.
#[test]
fn a_replaced_region_rebinds_the_water_textures() {
    let mut grid = (*grid()).clone();
    grid.inner = wet_field(33, REGION_SIZE_M);
    grid.apron = wet_field(17, APRON_SIZE_M);
    let grid = Arc::new(grid);
    let mut app = pool_app();
    enter(&mut app, &grid);
    for _ in 0..3 {
        app.update();
    }
    let water = |app: &App| {
        let scene = app.world().resource::<TerrainScene>();
        (scene.inner_water.id(), scene.apron_water.id())
    };
    let level = |app: &App, id: AssetId<Image>| {
        let image = app.world().resource::<Assets<Image>>().get(id).unwrap();
        decode_water_texel(image.data.as_ref().unwrap(), 0).0[0]
    };
    // Ids, not handles: a handle held here would keep the old textures alive.
    let (inner, apron) = water(&app);
    assert_eq!(level(&app, inner), grid.inner.water_level_m[0]);
    assert_eq!(level(&app, apron), grid.apron.water_level_m[0]);
    let size = |app: &App, id: AssetId<Image>| {
        app.world()
            .resource::<Assets<Image>>()
            .get(id)
            .unwrap()
            .size()
    };
    assert_eq!(size(&app, inner), UVec2::splat(33));
    assert_eq!(size(&app, apron), UVec2::splat(17));

    // The new snapshot's lake stands 100 m higher.
    let mut replaced = (*grid).clone();
    for f in [&mut replaced.inner, &mut replaced.apron] {
        f.water_level_m.iter_mut().for_each(|w| *w += 100.0);
    }
    let replaced = Arc::new(replaced);
    enter(&mut app, &replaced);
    for _ in 0..3 {
        app.update();
    }
    let (new_inner, new_apron) = water(&app);
    assert_ne!(new_inner, inner);
    assert_ne!(new_apron, apron);
    assert_eq!(level(&app, new_inner), replaced.inner.water_level_m[0]);
    assert_eq!(level(&app, new_apron), replaced.apron.water_level_m[0]);
    let images = app.world().resource::<Assets<Image>>();
    assert!(
        images.get(inner).is_none() && images.get(apron).is_none(),
        "old water kept"
    );
    // Four field textures, two water textures and the palette.
    assert_eq!(images.len(), 7);
}

#[test]
fn sky_and_water_follow_the_scene() {
    use crate::sky::{RegionSky, SkyMaterial, add_sky_systems};
    use crate::water::{RegionWater, WaterMaterial, add_water_systems};
    use bevy::pbr::DistanceFog;

    let grid = grid();
    let mut app = pool_app();
    app.init_asset::<SkyMaterial>()
        .init_asset::<WaterMaterial>();
    add_sky_systems(&mut app);
    add_water_systems(&mut app);
    enter(&mut app, &grid);
    for _ in 0..3 {
        app.update();
    }
    let (dome, sea) = {
        let world = app.world();
        let scene = world.resource::<TerrainScene>();
        let sky = world.resource::<RegionSky>();
        let water = world.resource::<RegionWater>();
        assert!(Arc::ptr_eq(&sky.grid, &grid) && Arc::ptr_eq(&water.grid, &grid));
        // The water reads the terrain's own buffer and the fields' water textures.
        let m = world
            .resource::<Assets<WaterMaterial>>()
            .get(&water.material)
            .unwrap();
        assert_eq!(m.extension.patches, scene.buffer);
        assert_eq!(m.extension.inner_water, scene.inner_water);
        assert_eq!(m.extension.apron_water, scene.apron_water);
        // The region camera carries the era's fog and the sky-coloured ambient light.
        let camera = world.entity(scene.camera);
        assert!(camera.contains::<DistanceFog>());
        let (zenith, _, fog) = crate::sky::sky_tint(sky.era, sky.biome);
        assert_eq!(camera.get::<DistanceFog>().unwrap().color, fog);
        assert_eq!(camera.get::<AmbientLight>().unwrap().color, zenith);
        // The terrain's look carries the same zenith as its shadow hue.
        let look = world
            .resource::<Assets<TerrainMaterial>>()
            .get(&scene.material)
            .unwrap()
            .extension
            .look;
        assert_eq!(look, look_for(&grid, TerrainOptions::default()));
        (sky.dome, water.entity)
    };

    // A snapshot change: the same entities, rebound to the new grid and retinted for its era.
    let mut deep = (*grid).clone();
    deep.spec.snapshot = YearsBp(540.0e6);
    let deep = Arc::new(deep);
    enter(&mut app, &deep);
    // A replace takes four frames (`a_replace_frees_the_old_water_before_making_the_new`).
    for _ in 0..4 {
        app.update();
    }
    {
        let world = app.world();
        let scene = world.resource::<TerrainScene>();
        let sky = world.resource::<RegionSky>();
        let water = world.resource::<RegionWater>();
        assert_eq!((sky.dome, water.entity), (dome, sea));
        assert_eq!(
            sky.era,
            timefly_core::worldgen::biome::VegetationEra::Barren
        );
        assert!(Arc::ptr_eq(&water.grid, &deep));
        let m = world
            .resource::<Assets<WaterMaterial>>()
            .get(&water.material)
            .unwrap();
        assert_eq!(m.extension.inner_water, scene.inner_water);
        let fog = world
            .entity(scene.camera)
            .get::<DistanceFog>()
            .unwrap()
            .color;
        assert_eq!(fog, crate::sky::sky_tint(sky.era, sky.biome).2);
        let look = world
            .resource::<Assets<TerrainMaterial>>()
            .get(&scene.material)
            .unwrap()
            .extension
            .look;
        assert_eq!(look, look_for(&deep, TerrainOptions::default()));
    }

    // Leaving: both go, and their assets with them.
    app.world_mut().remove_resource::<ActiveRegion>();
    for _ in 0..4 {
        app.update();
    }
    let world = app.world_mut();
    assert!(world.get_resource::<RegionSky>().is_none());
    assert!(world.get_resource::<RegionWater>().is_none());
    assert!(world.get_entity(dome).is_err() && world.get_entity(sea).is_err());
    assert_eq!(world.resource::<Assets<SkyMaterial>>().len(), 0);
    assert_eq!(world.resource::<Assets<WaterMaterial>>().len(), 0);
    assert_eq!(world.resource::<Assets<Mesh>>().len(), 0);
    assert_eq!(world.resource::<Assets<Image>>().len(), 0);
}

// ---------------------------------------------------------------------------------------------
// Seen from higher up (M3 Task 6)

use crate::fly_camera::{CAMERA_CEILING_M, CAMERA_CLEARANCE_M, FlyState, region_chase_pose};
use timefly_core::flight::altitudes::REGION_CLOUD_BASE_M;
use view::{FOG_END_M, M2_CEILING_M, REGION_FAR_M, SHADOW_DISTANCE_M, ViewRanges, view_ranges};

const R_M: f64 = timefly_core::worldgen::frame::EARTH_RADIUS_M;

/// The chase camera's eye for a fly at `(x, h, z)` heading `yaw` over sea-level ground.
fn chase_eye(x: f32, h: f32, z: f32, yaw: f32) -> DVec3 {
    let fly = FlyState {
        pos_m: Vec3::new(x, h, z),
        yaw,
        pitch: -crate::fly_camera::START_PITCH_DEG.to_radians(),
    };
    region_chase_pose(&fly, |_, _| 0.0).eye
}

/// Distance (m) from `eye` to the sea-level ground at horizontal `(x, z)`, dropped by the
/// curvature measured from the eye (as the terrain and the water draw it).
fn to_ground(eye: DVec3, x: f64, z: f64) -> f64 {
    let s = (x - eye.x).hypot(z - eye.z);
    DVec3::new(x - eye.x, -curvature_drop_m(s) - eye.y, z - eye.z).length()
}

/// The distance the fog reads for a fragment `d` metres away.
fn fog_distance(r: &ViewRanges, d: f64) -> f64 {
    (d - f64::from(r.fog_start_m)).max(0.0) * f64::from(r.fog_scale())
}

#[test]
fn view_ranges_match_m2_below_sixty_km() {
    for h in [
        -500.0,
        0.0,
        150.0,
        8_000.0,
        30_000.0,
        59_999.0,
        M2_CEILING_M,
    ] {
        let r = view_ranges(h);
        assert_eq!(
            r,
            ViewRanges {
                far_m: REGION_FAR_M,
                shadow_m: SHADOW_DISTANCE_M,
                fog_start_m: 0.0,
                fog_end_m: FOG_END_M,
            },
            "{h}"
        );
        assert_eq!(r.fog_scale(), 1.0);
        assert_eq!(r.first_cascade_m(), view::FIRST_CASCADE_M);
    }
    // The fog end is M2's: every era and biome is fully fogged there.
    use crate::terrain::material::{ALL_BIOMES, ALL_ERAS};
    for era in ALL_ERAS {
        for biome in ALL_BIOMES {
            let bevy::pbr::FogFalloff::ExponentialSquared { density } =
                crate::sky::distance_fog(era, biome).falloff
            else {
                panic!("M2's fog is exponential-squared");
            };
            let fog = 1.0 - (-(FOG_END_M * density).powi(2)).exp();
            assert!(fog >= view::FOG_COMPLETE, "{era:?} {biome:?}: {fog}");
        }
    }
}

/// From every reachable chase camera at 60–300 km, looking anywhere from 89° down to 10° up,
/// the view's top edge (the farthest it sees along each azimuth) reaches the apron's edge only
/// where that edge is already past the fog's end.
#[test]
fn the_apron_edge_is_never_visible() {
    let half = APRON_SIZE_M / 2.0;
    let tan_half_v = f64::from((view::REGION_FOV_DEG.to_radians() / 2.0).tan());
    // Wider than any window the game opens.
    let aspect = 2.4;
    let mut edge_in_view = 0;
    for h_km in [60.0f32, 120.0, 240.0, 300.0] {
        let h = h_km * 1000.0;
        let r = view_ranges(h);
        // The fly anywhere in the inner field, and past its soft edge (the spring lets a boosted
        // fly overshoot by tens of kilometres).
        let reach = crate::fly_camera::EDGE_M + 40_000.0;
        for (fx, fz) in [(0.0, 0.0), (1.0, 0.0), (1.0, 1.0), (-1.0, 0.5), (0.0, -1.0)] {
            for k in 0..16 {
                let yaw = k as f32 * std::f32::consts::TAU / 16.0;
                let eye = chase_eye(fx * reach, h, fz * reach, yaw);
                let horizon = (2.0 * R_M * eye.y).sqrt();
                for pitch_deg in -89..=10 {
                    let rot = Quat::from_rotation_y(yaw)
                        * Quat::from_rotation_x((pitch_deg as f32).to_radians());
                    let (fwd, up, right) = (
                        (rot * Vec3::NEG_Z).as_dvec3(),
                        (rot * Vec3::Y).as_dvec3(),
                        (rot * Vec3::X).as_dvec3(),
                    );
                    for i in -8..=8 {
                        let x = f64::from(i) / 8.0 * tan_half_v * aspect;
                        let ray = fwd + up * tan_half_v + right * x;
                        let along = ray.xz().length();
                        if along < 1e-9 {
                            continue;
                        }
                        // The ray against the dropped ground y = −s²/2R: a hit, or the horizon.
                        let a = along * along / (2.0 * R_M);
                        let disc = ray.y * ray.y - 4.0 * a * eye.y;
                        let far = if ray.y < 0.0 && disc >= 0.0 {
                            let t = (-ray.y - disc.sqrt()) / (2.0 * a);
                            t * along
                        } else {
                            horizon
                        };
                        // Where the ray's ground track leaves the apron.
                        let u = ray.xz() / along;
                        let exit = |c: f64, d: f64| {
                            if d.abs() < 1e-12 {
                                f64::INFINITY
                            } else {
                                (half * d.signum() - c) / d
                            }
                        };
                        let s_edge = exit(eye.x, u.x).min(exit(eye.z, u.y));
                        if far <= s_edge {
                            continue;
                        }
                        edge_in_view += 1;
                        let at = eye.xz() + u * s_edge;
                        let d = to_ground(eye, at.x, at.y);
                        // A 1% margin for the apron's own relief at its edge.
                        assert!(
                            fog_distance(&r, 0.99 * d) >= f64::from(FOG_END_M),
                            "fly {h_km} km, eye {eye}, yaw {yaw}, pitch {pitch_deg}: the edge \
                             {d:.0} m away, fog {r:?}"
                        );
                    }
                }
            }
        }
    }
    // The hazard is real: from up high, the apron's edge is in view.
    assert!(edge_in_view > 1_000, "{edge_in_view}");
}

/// Higher up, the ground under the fly is seen through no more haze than at M2's ceiling.
#[test]
fn the_ground_under_the_fly_stays_as_clear_as_at_sixty_km() {
    let under = |h: f32| {
        let eye = chase_eye(0.0, h, 0.0, 0.0);
        fog_distance(&view_ranges(h), to_ground(eye, 0.0, 0.0))
    };
    let m2 = under(M2_CEILING_M);
    for h in (60..=300).step_by(10) {
        let h = h as f32 * 1000.0;
        assert!(under(h) <= 1.01 * m2, "{h}: {} vs {m2}", under(h));
    }
}

/// Above M2's ceiling the ranges grow with the height, without jumps; the shadow cascades end
/// in full fog from 120 km, and the far plane holds the apron's far corner from every camera.
#[test]
fn view_ranges_grow_smoothly_and_hide_the_shadow_seam() {
    let mut last = view_ranges(M2_CEILING_M);
    for m in (61..=300).map(|km| km as f32 * 1000.0) {
        let r = view_ranges(m);
        for (now, before, name) in [
            (r.far_m, last.far_m, "far"),
            (r.shadow_m, last.shadow_m, "shadow"),
            (r.fog_start_m, last.fog_start_m, "fog start"),
            (r.fog_end_m, last.fog_end_m, "fog end"),
        ] {
            assert!(now >= before, "{name} shrinks at {m}");
            assert!(
                now - before <= 30_000.0,
                "{name} jumps at {m}: {before} -> {now}"
            );
        }
        if m >= 120_000.0 {
            assert!(r.shadow_m >= r.fog_end_m, "{m}: {r:?}");
        }
        // The far corner from a fly at the opposite corner of the inner field.
        let edge = crate::fly_camera::EDGE_M;
        let dir = Vec2::splat(-1.0).normalize();
        let yaw = crate::bench::region::path::yaw_of(-dir);
        let eye = chase_eye(edge, m, edge, yaw);
        let corner = to_ground(eye, -APRON_SIZE_M / 2.0, -APRON_SIZE_M / 2.0);
        assert!(
            f64::from(r.far_m) >= corner,
            "{m}: far {} < {corner}",
            r.far_m
        );
        last = r;
    }
}

/// The fly's clearance reads `blended_surface_m` (no drop: the drop at zero distance is zero), but
/// the terrain and the water draw the ground dropped by the curvature measured from the chase
/// camera, which sits behind the fly. The two agree to within a centimetre at the clearance and a
/// fraction of a percent of the height above the ground anywhere up to the ceiling.
#[test]
fn clearance_matches_the_drawn_ground_under_the_fly() {
    assert_eq!(curvature_drop_m(0.0), 0.0);
    assert_eq!(CAMERA_CEILING_M, REGION_CLOUD_BASE_M as f32);
    let grid = grid();
    let ground = |x: f64, z: f64| f64::from(timefly_core::terrain::blended_height_m(&grid, x, z));
    let surface_at =
        |x: f64, z: f64| f64::from(timefly_core::terrain::blended_surface_m(&grid, x, z));
    for (x, z) in [(0.0, 0.0), (90_000.0, -40_000.0), (-149_000.0, 149_000.0)] {
        let surface = surface_at(f64::from(x), f64::from(z)) as f32;
        for agl in [CAMERA_CLEARANCE_M, 1_000.0, 8_000.0, 60_000.0, 150_000.0] {
            let y = (surface + agl).min(CAMERA_CEILING_M);
            for yaw in [0.0f32, 1.0, 2.5, 4.0] {
                let fly = FlyState {
                    pos_m: Vec3::new(x, y, z),
                    yaw,
                    pitch: -0.3,
                };
                let eye = region_chase_pose(&fly, surface_at).eye;
                let offset = (eye.x - f64::from(x)).hypot(eye.z - f64::from(z));
                let clearance = ground(f64::from(x), f64::from(z)) - curvature_drop_m(0.0);
                let drawn = ground(f64::from(x), f64::from(z)) - curvature_drop_m(offset);
                let gap = clearance - drawn;
                let height = f64::from(y) - f64::from(surface);
                assert!(gap >= 0.0, "the drawn ground is above the clearance's");
                let allowed = if agl <= CAMERA_CLEARANCE_M {
                    0.01
                } else {
                    0.006 * height
                };
                assert!(
                    gap <= allowed,
                    "fly {} {agl} m up: the camera {offset:.0} m behind drops the ground \
                     {gap:.3} m",
                    fly.pos_m
                );
            }
        }
    }
    // The water drops its sea by the same d² / 2R from the same camera as the terrain.
    let water = include_str!("../../assets/shaders/water.wesl");
    assert!(water.contains("let xz = params.camera.xz + rel;"));
    assert!(water.contains("let drop = dot(rel, rel) / (2.0 * params.lattice.z);"));
    let params = TerrainParams::new(&grid, &LodSettings::region());
    assert_eq!(f64::from(params.lattice[2]), R_M);
}
