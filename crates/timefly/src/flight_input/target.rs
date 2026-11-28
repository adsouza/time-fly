//! Resolves a screen tap to a destination and steers at the existing boost speed.

use bevy::ecs::system::SystemParam;
use bevy::math::DVec3;
use bevy::prelude::*;
use timefly_core::flight::globe::globe_speed_km_s;
use timefly_core::globe::EARTH_RADIUS_KM;
use timefly_core::terrain::blended_surface_m;

use crate::fly_camera::{BOOST, EDGE_M, FlyState, LOOK_RADIANS_PER_PX, fly_speed};
use crate::globe::GlobeCamera;
use crate::globe_flight::GlobeFlightState;
use crate::region::ActiveRegion;
use crate::terrain::view::RegionCamera;

use super::{FlightIntent, FlightView};

const TARGET_TURN_RAD_S: f64 = std::f64::consts::PI * 4.0 / 3.0;
const SURFACE_SAMPLE_M: f32 = 250.0;

#[derive(Clone, Copy)]
pub(super) enum Destination {
    Globe(DVec3),
    Region(Vec2),
}

#[derive(SystemParam)]
pub(super) struct TargetWorld<'w, 's> {
    globe: Option<Res<'w, GlobeFlightState>>,
    fly: Option<Res<'w, FlyState>>,
    region: Option<Res<'w, ActiveRegion>>,
    globe_camera: Query<'w, 's, (&'static Camera, &'static GlobalTransform), With<GlobeCamera>>,
    region_camera: Query<'w, 's, (&'static Camera, &'static GlobalTransform), With<RegionCamera>>,
}

impl TargetWorld<'_, '_> {
    pub fn pick(&self, view: FlightView, position: Vec2) -> Option<Destination> {
        match view {
            FlightView::Globe => {
                let ray = viewport_ray(self.globe_camera.single().ok()?, position)?;
                globe_target(ray).map(Destination::Globe)
            }
            FlightView::Region => {
                let ray = viewport_ray(self.region_camera.single().ok()?, position)?;
                let grid = &self.region.as_ref()?.grid;
                region_target(ray, |x, z| blended_surface_m(grid, x.into(), z.into()))
                    .map(Destination::Region)
            }
            FlightView::Uncontrolled => None,
        }
    }

    /// Returns whether there is still a route to follow.
    pub fn steer(
        &self,
        view: FlightView,
        destination: Destination,
        intent: &mut FlightIntent,
    ) -> bool {
        match (view, destination) {
            (FlightView::Region, Destination::Region(target)) => {
                let (Some(fly), Some(region)) = (&self.fly, &self.region) else {
                    return false;
                };
                let p = fly.pos_m;
                let offset = target - Vec2::new(p.x, p.z);
                let distance = offset.length();
                if distance <= 2.0 {
                    return false;
                }
                let heading = Vec2::from_angle(fly.yaw);
                let desired = Vec2::new(-offset.y, -offset.x);
                let turn = f64::from(heading.perp_dot(desired).atan2(heading.dot(desired)));
                let ground = blended_surface_m(&region.grid, p.x.into(), p.z.into());
                let speed = fly_speed(p.y - ground) * BOOST;
                steer(intent, turn, f64::from(distance), f64::from(speed));
                true
            }
            (FlightView::Globe, Destination::Globe(target)) => {
                let Some(flight) = self.globe.as_ref().map(|f| f.0) else {
                    return false;
                };
                let axis = flight.dir.cross(target);
                let radius = EARTH_RADIUS_KM + flight.altitude_km;
                let distance = axis.length().atan2(flight.dir.dot(target)) * radius;
                if distance <= 0.01 {
                    return false;
                }
                let direction = axis.cross(flight.dir);
                let turn = flight
                    .dir
                    .dot(flight.heading.cross(direction))
                    .atan2(flight.heading.dot(direction));
                let speed = globe_speed_km_s(flight.altitude_km, true);
                steer(intent, turn, distance, speed);
                true
            }
            _ => false,
        }
    }
}

fn steer(intent: &mut FlightIntent, turn: f64, distance: f64, speed: f64) {
    let limit = TARGET_TURN_RAD_S * intent.dt_s;
    let applied = turn.clamp(-limit, limit);
    intent.look_delta.x = (-applied / f64::from(LOOK_RADIANS_PER_PX)) as f32;
    if turn == applied && intent.dt_s > 0.0 {
        intent.movement.move_local.z = -(distance / (speed * intent.dt_s)).min(1.0) as f32;
        intent.movement.boost = true;
    }
}

fn viewport_ray((camera, transform): (&Camera, &GlobalTransform), position: Vec2) -> Option<Ray3d> {
    if !camera.is_active || !camera.logical_viewport_rect()?.contains(position) {
        return None;
    }
    camera.viewport_to_world(transform, position).ok()
}

fn globe_target(ray: Ray3d) -> Option<DVec3> {
    let origin = ray.origin.as_dvec3();
    let direction = ray.direction.as_dvec3().normalize();
    let along = -origin.dot(direction);
    let cross = origin + along * direction;
    let chord_squared = EARTH_RADIUS_KM.powi(2) - cross.length_squared();
    if chord_squared < 0.0 {
        return None;
    }
    let near = along - chord_squared.sqrt();
    (near >= 0.0).then(|| (origin + near * direction).normalize())
}

/// Finds the first terrain or water intersection within the flyable part of this region.
fn region_target(ray: Ray3d, surface_m: impl Fn(f32, f32) -> f32) -> Option<Vec2> {
    let mut start: f32 = 0.0;
    let mut end = 4.0 * EDGE_M;
    for (origin, direction) in [
        (ray.origin.x, ray.direction.x),
        (ray.origin.z, ray.direction.z),
    ] {
        if direction.abs() < f32::EPSILON {
            if origin.abs() > EDGE_M {
                return None;
            }
            continue;
        }
        let a = (-EDGE_M - origin) / direction;
        let b = (EDGE_M - origin) / direction;
        start = start.max(a.min(b));
        end = end.min(a.max(b));
    }
    let above = |t| {
        let p = ray.get_point(t);
        p.y > surface_m(p.x, p.z)
    };
    if start >= end || !above(start) {
        return None;
    }
    while start < end {
        let next = (start + SURFACE_SAMPLE_M).min(end);
        if !above(next) {
            let (mut lo, mut hi) = (start, next);
            for _ in 0..10 {
                let mid = (lo + hi) * 0.5;
                if above(mid) {
                    lo = mid;
                } else {
                    hi = mid;
                }
            }
            let hit = ray.get_point(hi);
            return Some(Vec2::new(hit.x, hit.z));
        }
        start = next;
    }
    None
}
