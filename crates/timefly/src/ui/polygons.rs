//! Counts triangles in the meshes selected by the scene cameras' visibility checks.

use std::any::TypeId;
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};
use std::time::Duration;

use bevy::camera::visibility::VisibleEntities;
use bevy::mesh::PrimitiveTopology;
use bevy::prelude::*;
use bevy::render::mesh::{RenderMesh, RenderMeshBufferInfo};
use bevy::render::render_asset::RenderAssets;
use bevy::render::{Extract, ExtractSchedule, Render, RenderApp, RenderSystems};

use crate::globe::{BridgeCamera, GlobeCamera, GlobeRenderStatus};
use crate::terrain::view::RegionCamera;

use super::fps::FPS_REFRESH_S;

const NO_SAMPLE: u64 = u64::MAX;

#[derive(Clone, Resource)]
pub(super) struct PolygonCount(Arc<AtomicU64>);

impl PolygonCount {
    pub(super) fn sample(&self) -> Option<u64> {
        let count = self.0.load(Ordering::Relaxed);
        (count != NO_SAMPLE).then_some(count)
    }

    pub(super) fn publish(&self, count: u64) {
        self.0.store(count, Ordering::Relaxed);
    }
}

#[derive(Default, Resource)]
struct VisibleMeshes(Vec<AssetId<Mesh>>);

pub(super) fn add_polygon_count(app: &mut App) {
    let count = PolygonCount(Arc::new(AtomicU64::new(NO_SAMPLE)));
    if let Some(render_app) = app.get_sub_app_mut(RenderApp) {
        render_app
            .insert_resource(count.clone())
            .init_resource::<VisibleMeshes>()
            .add_systems(ExtractSchedule, extract_visible_meshes)
            .add_systems(
                Render,
                count_polygons
                    .after(RenderSystems::PrepareAssets)
                    .before(RenderSystems::Render),
            );
    }
    app.insert_resource(count);
}

type SceneCamera = Or<(With<GlobeCamera>, With<RegionCamera>, With<BridgeCamera>)>;
type SceneView = (&'static Camera, &'static VisibleEntities, Has<BridgeCamera>);

fn extract_visible_meshes(
    time: Extract<Res<Time<Real>>>,
    globe_status: Extract<Option<Res<GlobeRenderStatus>>>,
    cameras: Extract<Query<SceneView, SceneCamera>>,
    meshes: Extract<Query<&Mesh3d>>,
    mut visible: ResMut<VisibleMeshes>,
    mut next_sample: Local<Duration>,
) {
    let now = time.elapsed();
    if now < *next_sample {
        return;
    }
    *next_sample = now + Duration::from_secs_f32(FPS_REFRESH_S);
    visible.0.clear();

    // The interactive globe's bridge draws both inputs offscreen before compositing them.
    // Exclude camera work unrelated to the view, such as the loading pipeline warm-up.
    let globe_ready = matches!(
        globe_status.as_deref(),
        Some(GlobeRenderStatus::Interactive { .. })
    );
    for (camera, entities, bridge) in cameras.iter() {
        if camera.is_active && (!bridge || globe_ready) {
            visible.0.extend(
                entities
                    .iter(TypeId::of::<Mesh3d>())
                    .filter_map(|entity| meshes.get(*entity).ok())
                    .map(|mesh| mesh.0.id()),
            );
        }
    }
}

fn count_polygons(
    visible: Res<VisibleMeshes>,
    meshes: Res<RenderAssets<RenderMesh>>,
    count: Res<PolygonCount>,
) {
    if !visible.is_changed() {
        return;
    }
    // Most scene meshes are render-world-only. Use their prepared buffer counts without
    // keeping copies of the vertices in the main world or reading anything back from the GPU.
    let triangles = visible
        .0
        .iter()
        .filter_map(|id| meshes.get(*id))
        .map(|mesh| {
            let elements = match mesh.buffer_info {
                RenderMeshBufferInfo::Indexed { count, .. } => count,
                RenderMeshBufferInfo::NonIndexed => mesh.vertex_count,
            };
            let triangles = match mesh.primitive_topology() {
                PrimitiveTopology::TriangleList => elements / 3,
                PrimitiveTopology::TriangleStrip => elements.saturating_sub(2),
                _ => 0,
            };
            u64::from(triangles)
        })
        .sum();
    count.publish(triangles);
}

#[cfg(test)]
mod tests {
    use bevy::app::SubApp;
    use bevy::asset::RenderAssetUsages;
    use bevy::mesh::BaseMeshPipelineKey;
    use bevy::render::MainWorld;
    use bevy::render::render_resource::IndexFormat;

    use super::*;

    fn view(entities: Vec<Entity>) -> VisibleEntities {
        let mut visible = VisibleEntities::default();
        *visible.get_mut(TypeId::of::<Mesh3d>()) = entities;
        visible
    }

    #[test]
    fn counts_visible_polygon_instances_from_prepared_meshes() {
        let mut app = App::new();
        app.insert_sub_app(RenderApp, SubApp::new());
        add_polygon_count(&mut app);
        let count = app.world().resource::<PolygonCount>().clone();

        let mut main = MainWorld::default();
        main.init_resource::<Time<Real>>();
        let source = Mesh::new(
            PrimitiveTopology::TriangleList,
            RenderAssetUsages::RENDER_WORLD,
        );
        let mut assets = Assets::<Mesh>::default();
        let indexed = assets.add(source.clone());
        let indexed_id = indexed.id();
        let non_indexed = assets.add(source.clone());
        let non_indexed_id = non_indexed.id();
        let a = main.spawn(Mesh3d(indexed.clone())).id();
        let b = main.spawn(Mesh3d(indexed)).id();
        let c = main.spawn(Mesh3d(non_indexed)).id();
        main.spawn((GlobeCamera, Camera::default(), view(vec![a, b, c])));
        main.spawn((
            RegionCamera,
            Camera {
                is_active: false,
                ..default()
            },
            view(vec![c]),
        ));
        // An offscreen camera unrelated to the scene must not add geometry to the count.
        main.spawn((Camera::default(), view(vec![a, b])));
        let mut meshes = RenderAssets::<RenderMesh>::default();
        let render_mesh = |buffer_info| RenderMesh {
            vertex_count: 6,
            aabb_center: Vec3::ZERO,
            buffer_info,
            key_bits: BaseMeshPipelineKey::from_primitive_topology_and_strip_index(
                PrimitiveTopology::TriangleList,
                None,
            ),
            layout: source.get_mesh_vertex_buffer_layout(&mut default()),
        };
        meshes.insert(
            indexed_id,
            render_mesh(RenderMeshBufferInfo::Indexed {
                count: 9,
                index_format: IndexFormat::Uint32,
            }),
        );
        meshes.insert(
            non_indexed_id,
            render_mesh(RenderMeshBufferInfo::NonIndexed),
        );

        let render = app.sub_app_mut(RenderApp).world_mut();
        render.insert_resource(main);
        render.insert_resource(meshes);
        render.run_schedule(ExtractSchedule);
        render.run_schedule(Render);
        assert_eq!(count.sample(), Some(8));
    }
}
