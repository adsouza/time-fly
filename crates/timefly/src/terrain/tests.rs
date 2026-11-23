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
    Field {
        res,
        size_m,
        surface: vec![[0, 0, 0, 0]; (res * res) as usize],
        elevation_m,
    }
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
    assert_eq!(app.world().resource::<Assets<Image>>().len(), 5);
    // A replaced region (a snapshot change) swaps the four field textures.
    let replaced = Arc::new((*grid).clone());
    enter(&mut app, &replaced);
    app.update();
    app.update();
    assert_eq!(app.world().resource::<Assets<Image>>().len(), 5);
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
