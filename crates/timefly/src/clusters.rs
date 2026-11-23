//! Clustered forward lighting is switched off (ruling R28).
//!
//! The game has no clusterable objects (no point, spot or rect lights, light probes or decals;
//! each scene has one directional sun), yet Bevy 0.20 rebuilds every active view's cluster
//! storage buffers each frame (`prepare_clusters_for_gpu_clustering`, and
//! `prepare_clusters_for_cpu_clustering`). Natively the old buffers are freed on drop; on WebGPU
//! wgpu's buffer `Drop` never calls `destroy()`, so Chrome held about 26.7 MB/s of them (the page
//! grew about 45 MB/s in a region).
//!
//! - Every `Camera3d` gets `ClusterConfig::None`, from its spawn site and, for any camera added
//!   later, from an observer on `Camera3d`.
//! - `ClusterConfig::None` alone does not shrink the GPU path: its index list keeps the GPU
//!   clustering capacity (262,144 B a frame). So GPU clustering is switched off as
//!   `GlobalClusterSettings` documents (`gpu_clustering = None`). The CPU path still makes a
//!   view's cluster buffers each frame, at their minimum size with no clusters.

use bevy::light::cluster::{ClusterConfig, GlobalClusterSettings};
use bevy::prelude::*;

pub struct NoClusteringPlugin;

impl Plugin for NoClusteringPlugin {
    fn build(&self, app: &mut App) {
        app.add_observer(no_clusters_on_new_cameras)
            .add_systems(Startup, disable_gpu_clustering);
    }
}

/// Every new 3D camera gets `ClusterConfig::None`.
fn no_clusters_on_new_cameras(add: On<Add<Camera3d>>, mut commands: Commands) {
    commands.entity(add.entity).insert(ClusterConfig::None);
}

/// GPU clustering off (its per-frame index list is sized by capacity, not by clusters).
fn disable_gpu_clustering(settings: Option<ResMut<GlobalClusterSettings>>) {
    if let Some(mut settings) = settings
        && settings.gpu_clustering.is_some()
    {
        settings.gpu_clustering = None;
        info!("GPU light clustering disabled (no clustered lights; ruling R28)");
    }
}

/// Panics unless every `Camera3d` in `world` has `ClusterConfig::None`. Tests call it on apps
/// without [`NoClusteringPlugin`], so a spawn site that forgets it fails there.
#[cfg(test)]
pub(crate) fn assert_no_clusters(world: &mut World) {
    let mut cameras = world.query::<(Entity, Option<&Name>, Option<&ClusterConfig>, &Camera3d)>();
    let mut count = 0;
    for (e, name, config, _) in cameras.iter(world) {
        count += 1;
        assert!(
            matches!(config, Some(ClusterConfig::None)),
            "camera {e:?} ({name:?}) clusters: {config:?}"
        );
    }
    assert!(count > 0, "no 3D camera to check");
}

#[cfg(test)]
mod tests {
    use bevy::light::cluster::GlobalClusterGpuSettings;

    use super::*;

    #[test]
    fn every_camera_gets_no_clusters_and_gpu_clustering_goes_off() {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .insert_resource(GlobalClusterSettings {
                supports_storage_buffers: true,
                clustered_decals_are_usable: false,
                gpu_clustering: Some(GlobalClusterGpuSettings {
                    initial_z_slice_list_capacity: 16,
                    initial_index_list_capacity: 65_536,
                }),
                max_uniform_buffer_clusterable_objects: 204,
                view_cluster_bindings_max_indices: 16_384,
            })
            .add_plugins(NoClusteringPlugin);
        // A camera from anywhere, now or later, with or without its own setting.
        let bare = app.world_mut().spawn(Camera3d::default()).id();
        app.update();
        let later = app
            .world_mut()
            .spawn((Camera3d::default(), Camera::default()))
            .id();
        app.update();
        for e in [bare, later] {
            assert!(matches!(
                app.world().get::<ClusterConfig>(e),
                Some(ClusterConfig::None)
            ));
        }
        assert!(
            app.world()
                .resource::<GlobalClusterSettings>()
                .gpu_clustering
                .is_none()
        );
    }
}
