//! The reality check's comparison (rivers and lakes Task 11, design §7): how closely the game's
//! river centrelines follow a reference network, the 1′ reference pass over a box
//! (`tools/bake/timefly_bake/hydro_ref.py`).
//!
//! - **Resampling:** each reference link is cut into `ceil(length / RESAMPLE_KM)` equal pieces
//!   along its great-circle polyline; a piece counts its length at its midpoint, whose drainage
//!   area and elevation are interpolated along the link.
//! - **Distance:** a sample's great-circle distance to the nearest segment of any game polyline
//!   (a one-point polyline is a point).
//! - **Counting:** only samples the filter accepts count. `share_within` is the counted length
//!   within the tolerance over all counted length (0 with none counted). A link counts when any
//!   of its length is counted, and matches when at least half its counted length is within the
//!   tolerance. `worst_km` is the largest counted sample distance (0 with none counted,
//!   infinite with no game polylines).

use std::collections::HashMap;

use bevy_math::DVec3;
use serde_json::Value;

use super::{Down, EraHydrology};
use crate::geo::{RenderDir, geo_to_render, latlon_to_geo};
use crate::worldgen::frame::EARTH_RADIUS_M;

/// The reference is resampled every this many kilometres (at most).
pub const RESAMPLE_KM: f64 = 1.0;
/// A link matches when at least this share of its counted length is within the tolerance.
pub const LINK_MATCH_SHARE: f64 = 0.5;

/// A point of a reference link.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct RefPoint {
    /// Unit render-frame direction.
    pub dir: DVec3,
    pub lat_deg: f64,
    pub lon_deg: f64,
    /// The area draining through the point.
    pub area_km2: f64,
    pub elevation_m: f64,
}

impl RefPoint {
    pub fn new(lat_deg: f64, lon_deg: f64, area_km2: f64, elevation_m: f64) -> RefPoint {
        RefPoint {
            dir: geo_to_render(latlon_to_geo(lat_deg, lon_deg)).0,
            lat_deg,
            lon_deg,
            area_km2,
            elevation_m,
        }
    }
}

/// A reference river between confluences, upstream first.
#[derive(Clone, Debug, PartialEq)]
pub struct RefLink {
    pub points: Vec<RefPoint>,
    /// The link's discharge at its downstream end (m³/s).
    pub discharge_m3s: f64,
}

/// A lake of the reference network.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct RefLake {
    pub lat_deg: f64,
    pub lon_deg: f64,
    pub area_km2: f64,
    /// The mean precipitation over the lake and the river nodes upstream of it (mm/yr).
    pub basin_precip_mm: f64,
}

/// A reference fixture: its box and its network.
#[derive(Clone, Debug, PartialEq)]
pub struct Reference {
    /// `(lat_n, lon_w, lat_s, lon_e)`, degrees.
    pub bbox: (f64, f64, f64, f64),
    pub links: Vec<RefLink>,
    pub lakes: Vec<RefLake>,
}

impl Reference {
    /// Whether `(lat, lon)` lies in the box (edges included).
    pub fn contains(&self, lat_deg: f64, lon_deg: f64) -> bool {
        let (n, w, s, e) = self.bbox;
        (s..=n).contains(&lat_deg) && (w..=e).contains(&lon_deg)
    }
}

fn number(v: &Value, what: &str) -> Result<f64, String> {
    v.as_f64()
        .filter(|x| x.is_finite())
        .ok_or_else(|| format!("{what}: not a finite number"))
}

fn array<'a>(v: &'a Value, what: &str) -> Result<&'a Vec<Value>, String> {
    v.as_array().ok_or_else(|| format!("{what}: not an array"))
}

/// Parses a reference fixture: `{"box": [lat_n, lon_w, lat_s, lon_e], "links": [{"lonlat":
/// [[lon, lat], …], "area_km2": […], "elevation_m": […], "discharge_m3s": f}], "lakes":
/// [{"lonlat": [lon, lat], "area_km2": f, "basin_precip_mm": f}]}` (`lakes` optional).
pub fn parse_reference(json: &str) -> Result<Reference, String> {
    let v: Value = serde_json::from_str(json).map_err(|e| e.to_string())?;
    let b = array(&v["box"], "box")?;
    if b.len() != 4 {
        return Err("box: needs [lat_n, lon_w, lat_s, lon_e]".into());
    }
    let bbox = (
        number(&b[0], "box")?,
        number(&b[1], "box")?,
        number(&b[2], "box")?,
        number(&b[3], "box")?,
    );
    if !(bbox.2 < bbox.0 && bbox.1 < bbox.3) {
        return Err(format!("box {bbox:?}: not north-west to south-east"));
    }
    let mut links = Vec::new();
    for (k, link) in array(&v["links"], "links")?.iter().enumerate() {
        let what = format!("link {k}");
        let lonlat = array(&link["lonlat"], &what)?;
        let area = array(&link["area_km2"], &what)?;
        let elevation = array(&link["elevation_m"], &what)?;
        if lonlat.len() < 2 || area.len() != lonlat.len() || elevation.len() != lonlat.len() {
            return Err(format!(
                "{what}: needs two points and one area and elevation per point"
            ));
        }
        let points = (0..lonlat.len())
            .map(|i| {
                let p = array(&lonlat[i], &what)?;
                let (lon, lat) = (
                    number(p.first().unwrap_or(&Value::Null), &what)?,
                    number(p.get(1).unwrap_or(&Value::Null), &what)?,
                );
                Ok(RefPoint::new(
                    lat,
                    lon,
                    number(&area[i], &what)?,
                    number(&elevation[i], &what)?,
                ))
            })
            .collect::<Result<Vec<_>, String>>()?;
        links.push(RefLink {
            points,
            discharge_m3s: number(&link["discharge_m3s"], &what)?,
        });
    }
    let mut lakes = Vec::new();
    if !v["lakes"].is_null() {
        for (k, lake) in array(&v["lakes"], "lakes")?.iter().enumerate() {
            let what = format!("lake {k}");
            let p = array(&lake["lonlat"], &what)?;
            if p.len() != 2 {
                return Err(format!("{what}: lonlat needs [lon, lat]"));
            }
            lakes.push(RefLake {
                lon_deg: number(&p[0], &what)?,
                lat_deg: number(&p[1], &what)?,
                area_km2: number(&lake["area_km2"], &what)?,
                basin_precip_mm: number(&lake["basin_precip_mm"], &what)?,
            });
        }
    }
    Ok(Reference { bbox, links, lakes })
}

/// How well the game follows a reference ([`centreline_agreement`]).
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Agreement {
    pub share_within: f64,
    pub matched_links: u32,
    pub links: u32,
    pub worst_km: f64,
}

/// The angle between unit vectors, accurate at small angles.
fn angle(a: DVec3, b: DVec3) -> f64 {
    2.0 * (0.5 * (a - b).length()).min(1.0).asin()
}

/// The angle from `p` to the shorter great-circle arc from `a` to `b` (unit vectors).
fn angle_to_arc(p: DVec3, a: DVec3, b: DVec3) -> f64 {
    let n = a.cross(b);
    let len = n.length();
    let ends = angle(p, a).min(angle(p, b));
    if len < 1e-15 {
        return ends;
    }
    let n = n / len;
    let q = p - n * p.dot(n);
    if a.cross(q).dot(n) >= 0.0 && q.cross(b).dot(n) >= 0.0 && q.length() > 0.0 {
        p.dot(n).abs().min(1.0).asin().min(ends)
    } else {
        ends
    }
}

/// Cell shells [`SegmentIndex::nearest`] searches before it falls back to every segment.
const MAX_SHELLS: i32 = 6;

/// The game's polylines as segments in a uniform grid over the unit sphere's bounding cube.
struct SegmentIndex {
    segments: Vec<(DVec3, DVec3)>,
    cell: f64,
    cells: HashMap<(i32, i32, i32), Vec<u32>>,
}

impl SegmentIndex {
    fn new(game: &[Vec<DVec3>], cell: f64) -> SegmentIndex {
        let mut segments = Vec::new();
        for line in game {
            let line: Vec<DVec3> = line.iter().map(|p| p.normalize()).collect();
            match line.len() {
                0 => {}
                1 => segments.push((line[0], line[0])),
                _ => segments.extend(line.windows(2).map(|w| (w[0], w[1]))),
            }
        }
        let key = |p: DVec3| {
            let k = (p / cell).floor();
            (k.x as i32, k.y as i32, k.z as i32)
        };
        let mut cells: HashMap<(i32, i32, i32), Vec<u32>> = HashMap::new();
        for (s, &(a, b)) in segments.iter().enumerate() {
            let (lo, hi) = (key(a.min(b)), key(a.max(b)));
            for x in lo.0..=hi.0 {
                for y in lo.1..=hi.1 {
                    for z in lo.2..=hi.2 {
                        cells.entry((x, y, z)).or_default().push(s as u32);
                    }
                }
            }
        }
        SegmentIndex {
            segments,
            cell,
            cells,
        }
    }

    /// The angle from `p` to the nearest segment, or infinity with none. Searches shells of
    /// cells outward until no unsearched segment can be nearer: one `k` cells out lies at least
    /// `k` cells of chord away, and an angle is at least its chord. Past [`MAX_SHELLS`] it
    /// searches every segment.
    fn nearest(&self, p: DVec3) -> f64 {
        let c = (p / self.cell).floor();
        let c = (c.x as i32, c.y as i32, c.z as i32);
        let mut best = f64::INFINITY;
        for k in 0..=MAX_SHELLS {
            for x in c.0 - k..=c.0 + k {
                for y in c.1 - k..=c.1 + k {
                    for z in c.2 - k..=c.2 + k {
                        let shell = (x - c.0).abs().max((y - c.1).abs()).max((z - c.2).abs());
                        if shell != k {
                            continue;
                        }
                        for &s in self.cells.get(&(x, y, z)).into_iter().flatten() {
                            let (a, b) = self.segments[s as usize];
                            best = best.min(angle_to_arc(p, a, b));
                        }
                    }
                }
            }
            if best <= f64::from(k) * self.cell {
                return best;
            }
        }
        self.segments
            .iter()
            .map(|&(a, b)| angle_to_arc(p, a, b))
            .fold(best, f64::min)
    }
}

/// How closely `game`'s polylines (render-frame directions) follow `reference`'s links, counting
/// only the resampled points `filter` accepts (the module's rules).
pub fn centreline_agreement(
    game: &[Vec<DVec3>],
    reference: &[RefLink],
    tolerance_km: f64,
    filter: impl Fn(&RefPoint) -> bool,
) -> Agreement {
    let km = EARTH_RADIUS_M * 1e-3;
    // Cells about the tolerance wide: most searches end in the first shell or two.
    let index = SegmentIndex::new(game, (tolerance_km / km).max(1e-4));
    let (mut counted, mut within) = (0.0, 0.0);
    let (mut matched, mut links) = (0u32, 0u32);
    let mut worst: f64 = 0.0;
    for link in reference {
        let (mut link_counted, mut link_within) = (0.0, 0.0);
        for w in link.points.windows(2) {
            let (a, b) = (w[0], w[1]);
            let length_km = angle(a.dir.normalize(), b.dir.normalize()) * km;
            let pieces = ((length_km / RESAMPLE_KM) - 1e-9).ceil().max(1.0);
            let piece_km = length_km / pieces;
            for i in 0..pieces as u32 {
                let t = (f64::from(i) + 0.5) / pieces;
                let sample = RefPoint {
                    dir: a.dir.lerp(b.dir, t).normalize(),
                    lat_deg: a.lat_deg + t * (b.lat_deg - a.lat_deg),
                    lon_deg: a.lon_deg + t * (b.lon_deg - a.lon_deg),
                    area_km2: a.area_km2 + t * (b.area_km2 - a.area_km2),
                    elevation_m: a.elevation_m + t * (b.elevation_m - a.elevation_m),
                };
                if !filter(&sample) {
                    continue;
                }
                let d = index.nearest(sample.dir) * km;
                link_counted += piece_km;
                if d <= tolerance_km {
                    link_within += piece_km;
                }
                worst = worst.max(d);
            }
        }
        if link_counted > 0.0 {
            links += 1;
            matched += u32::from(link_within >= LINK_MATCH_SHARE * link_counted);
            counted += link_counted;
            within += link_within;
        }
    }
    Agreement {
        share_within: if counted > 0.0 { within / counted } else { 0.0 },
        matched_links: matched,
        links,
        worst_km: worst,
    }
}

/// Where a river from a point ends ([`trace_outlet`]).
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct OutletTrace {
    /// The river node the trace starts from.
    pub start: u32,
    /// The river nodes followed, the start included.
    pub nodes: u32,
    /// The lakes the trace passed through.
    pub lakes: u32,
    /// The great-circle distance from the `via` point to the path through the river nodes'
    /// lattice centres (km), if one was given.
    pub via_km: Option<f64>,
    /// How the last river node drains: never [`Down::River`].
    pub end: Down,
    /// The lattice node it drains into (the river node itself for [`Down::Sink`]).
    pub end_dir: DVec3,
}

/// The river node leaving lake `l`: of the river nodes within one and a half lattice spacings of
/// its exit that do not drain into it and carry at least its outflow (to f32 rounding), the one
/// of the largest discharge (ties: the lowest index). `None` for a lake with no outflow.
fn lake_outlet(hydro: &EraHydrology, l: u16) -> Option<u32> {
    let lake = &hydro.lakes[usize::from(l)];
    if lake.outflow_m3s <= 0.0 {
        return None;
    }
    let lattice = &hydro.lattice;
    let nodes = hydro.rivers.nodes();
    let mut near = Vec::new();
    hydro.rivers.near(
        lattice,
        lattice.dir(lake.exit),
        1.5 * lattice.mean_spacing_m(),
        &mut near,
    );
    near.into_iter()
        .filter(|&k| {
            let n = &nodes[k as usize];
            !matches!(n.down, Down::Lake(x, _) if x == l)
                && f64::from(n.discharge_m3s) >= 0.99 * lake.outflow_m3s
        })
        .reduce(|best, k| {
            if nodes[k as usize].discharge_m3s > nodes[best as usize].discharge_m3s {
                k
            } else {
                best
            }
        })
}

/// Follows the river from `from` (a render-frame direction) to its end: it starts at the river
/// node of the largest discharge within `reach_km` (ties: the lowest index) and follows
/// [`Down::River`] links, and through each lake with an outflow to the river leaving it
/// ([`lake_outlet`]; the path crosses the lake from where the river enters to that node). `None`
/// with no river node in reach.
pub fn trace_outlet(
    hydro: &EraHydrology,
    from: DVec3,
    reach_km: f64,
    via: Option<DVec3>,
) -> Option<OutletTrace> {
    let lattice = &hydro.lattice;
    let nodes = hydro.rivers.nodes();
    let mut near = Vec::new();
    hydro
        .rivers
        .near(lattice, RenderDir(from), reach_km * 1e3, &mut near);
    let start = near.iter().copied().reduce(|best, k| {
        if nodes[k as usize].discharge_m3s > nodes[best as usize].discharge_m3s {
            k
        } else {
            best
        }
    })?;
    let via = via.map(DVec3::normalize);
    let mut via_angle = via.map(|v| angle(v, lattice.dir(nodes[start as usize].node).0));
    let mut see = |a: DVec3, b: DVec3| {
        if let (Some(v), Some(best)) = (via, via_angle.as_mut()) {
            *best = best.min(angle_to_arc(v, a, b));
        }
    };
    let (mut at, mut count, mut lakes) = (start, 1u32, 0u32);
    loop {
        let n = &nodes[at as usize];
        let here = lattice.dir(n.node).0;
        let (end, next) = match n.down {
            Down::River(j) => (nodes[j as usize].node, Some(j)),
            Down::Lake(l, j) => (j, lake_outlet(hydro, l)),
            Down::Ocean(j) | Down::Ice(j) | Down::Sink(j) => (j, None),
        };
        let to = lattice.dir(end).0;
        see(here, to);
        match next {
            Some(k) if (count as usize) <= nodes.len() => {
                if let Down::Lake(..) = n.down {
                    see(to, lattice.dir(nodes[k as usize].node).0);
                    lakes += 1;
                }
                at = k;
                count += 1;
            }
            _ => {
                return Some(OutletTrace {
                    start,
                    nodes: count,
                    lakes,
                    via_km: via_angle.map(|a| a * EARTH_RADIUS_M * 1e-3),
                    end: n.down,
                    end_dir: to,
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hydrology::{CubeLattice, HydroGraph, Lake, LakeKind};
    use crate::worldgen::testkit::{RiverChain, lattice_walk, river_hydro};
    use proptest::prelude::*;

    const KM_PER_DEG: f64 = EARTH_RADIUS_M * 1e-3 * std::f64::consts::PI / 180.0;

    /// A point `x_km` east and `y_km` north of (0°, 0°) along the equator and a meridian.
    fn at(x_km: f64, y_km: f64) -> RefPoint {
        RefPoint::new(y_km / KM_PER_DEG, x_km / KM_PER_DEG, 0.0, 0.0)
    }

    fn line(points: &[(f64, f64)]) -> Vec<DVec3> {
        points.iter().map(|&(x, y)| at(x, y).dir).collect()
    }

    /// An eastward reference link from 0 to `length_km`, elevation falling from `top_m` to 0.
    fn link(length_km: f64, top_m: f64) -> RefLink {
        let mut a = at(0.0, 0.0);
        a.elevation_m = top_m;
        a.area_km2 = 10_000.0;
        let mut b = at(length_km, 0.0);
        b.area_km2 = 20_000.0;
        RefLink {
            points: vec![a, b],
            discharge_m3s: 100.0,
        }
    }

    #[test]
    fn agreement_counts_length_within_tolerance() {
        // The game follows the first 6 km of a 10 km link, 1 km to its north. Within 1.5 km:
        // samples 0.5..5.5 (on the line) and 6.5 (0.5 km past its end: √1.25 = 1.118 km);
        // 7.5 is at √(1.5² + 1) = 1.80 km.
        let game = vec![line(&[(0.0, 1.0), (3.0, 1.0), (6.0, 1.0)])];
        let a = centreline_agreement(&game, &[link(10.0, 0.0)], 1.5, |_| true);
        assert!((a.share_within - 0.7).abs() < 1e-9, "{a:?}");
        assert_eq!((a.matched_links, a.links), (1, 1));
        let worst = (3.5f64 * 3.5 + 1.0).sqrt();
        assert!((a.worst_km - worst).abs() < 1e-3, "{a:?}");
        // A tolerance below the offset: nothing within, and the link does not match.
        let a = centreline_agreement(&game, &[link(10.0, 0.0)], 0.9, |_| true);
        assert_eq!((a.share_within, a.matched_links, a.links), (0.0, 0, 1));
        // No game rivers: nothing within, infinitely far.
        let a = centreline_agreement(&[], &[link(10.0, 0.0)], 5.0, |_| true);
        assert_eq!((a.share_within, a.matched_links), (0.0, 0));
        assert_eq!(a.worst_km, f64::INFINITY);
        // No reference: nothing counted, so no share (a bar on it fails rather than passes).
        let a = centreline_agreement(&game, &[], 5.0, |_| true);
        assert_eq!(
            a,
            Agreement {
                share_within: 0.0,
                matched_links: 0,
                links: 0,
                worst_km: 0.0
            }
        );
    }

    #[test]
    fn a_link_matches_with_half_its_length() {
        // Two 4 km links 50 km apart; the game lies on the first 2 km of one and the first 1 km
        // of the other. Within 0.1 km: half of the first (samples 0.5, 1.5), a quarter of the
        // second (0.5).
        let mut second = link(4.0, 0.0);
        for p in &mut second.points {
            *p = at(p.lon_deg * KM_PER_DEG, 50.0);
        }
        let game = vec![
            line(&[(0.0, 0.0), (2.0, 0.0)]),
            line(&[(0.0, 50.0), (1.0, 50.0)]),
        ];
        let a = centreline_agreement(&game, &[link(4.0, 0.0), second.clone()], 0.1, |_| true);
        assert_eq!((a.matched_links, a.links), (1, 2), "{a:?}");
        // The second link runs along a parallel, a little shorter than 4 km.
        assert!((a.share_within - 3.0 / 8.0).abs() < 1e-5, "{a:?}");
        // Just under half of the first (the 1.5 km sample is 0.2 km out): no match.
        let game = vec![line(&[(0.0, 0.0), (1.3, 0.0)])];
        let a = centreline_agreement(&game, &[link(4.0, 0.0)], 0.1, |_| true);
        assert_eq!((a.matched_links, a.links), (0, 1), "{a:?}");
    }

    #[test]
    fn flats_take_their_own_filter() {
        // A 10 km link falling from 1,000 m to 0: below 300 m is its last 3 km, where the game
        // lies. The filter counts only that part, and only filtered samples set the worst.
        let game = vec![line(&[(7.0, 0.0), (10.0, 0.0)])];
        let all = centreline_agreement(&game, &[link(10.0, 1000.0)], 0.1, |_| true);
        assert!((all.share_within - 0.3).abs() < 1e-9, "{all:?}");
        assert_eq!(all.matched_links, 0);
        let flats =
            centreline_agreement(&game, &[link(10.0, 1000.0)], 0.1, |p| p.elevation_m < 300.0);
        assert!((flats.share_within - 1.0).abs() < 1e-12, "{flats:?}");
        assert_eq!((flats.matched_links, flats.links), (1, 1));
        assert!(flats.worst_km < 1e-6, "{flats:?}");
        // The area is interpolated too: the link's upstream half is below 15,000 km².
        let big = centreline_agreement(&game, &[link(10.0, 0.0)], 0.1, |p| p.area_km2 >= 15_000.0);
        assert!((big.share_within - 0.6).abs() < 1e-9, "{big:?}");
        // A link the filter removes entirely does not count.
        let none = centreline_agreement(&game, &[link(10.0, 1000.0)], 0.1, |p| p.elevation_m > 5e3);
        assert_eq!((none.links, none.share_within), (0, 0.0));
    }

    #[test]
    fn distances_are_great_circle_to_the_nearest_segment() {
        let a = at(0.0, 0.0).dir;
        let b = at(10.0, 0.0).dir;
        let km = EARTH_RADIUS_M * 1e-3;
        // Beside the middle, beyond an end, and on the arc.
        assert!((angle_to_arc(at(5.0, 3.0).dir, a, b) * km - 3.0).abs() < 1e-6);
        assert!((angle_to_arc(at(14.0, 3.0).dir, a, b) * km - 5.0).abs() < 1e-3);
        assert!(angle_to_arc(at(2.5, 0.0).dir, a, b) * km < 1e-6);
        // A one-point polyline is a point.
        let index = SegmentIndex::new(&[vec![a]], 1e-3);
        assert!((index.nearest(at(3.0, 4.0).dir) * km - 5.0).abs() < 1e-3);
    }

    #[test]
    fn references_parse_and_reject_bad_shapes() {
        let json = r#"{"box": [37.5, 66.0, 23.0, 82.0],
            "links": [{"lonlat": [[70.0, 30.0], [70.1, 29.9]], "area_km2": [1e4, 2e4],
                       "elevation_m": [300, 200], "discharge_m3s": 55.5}],
            "lakes": [{"lonlat": [69.0, 31.0], "area_km2": 1500, "basin_precip_mm": 200}]}"#;
        let r = parse_reference(json).unwrap();
        assert_eq!(r.bbox, (37.5, 66.0, 23.0, 82.0));
        assert_eq!(r.links.len(), 1);
        let p = r.links[0].points[1];
        assert_eq!(
            (p.lat_deg, p.lon_deg, p.area_km2, p.elevation_m),
            (29.9, 70.1, 2e4, 200.0)
        );
        assert_eq!(p.dir, geo_to_render(latlon_to_geo(29.9, 70.1)).0);
        assert_eq!(r.links[0].discharge_m3s, 55.5);
        assert_eq!(r.lakes[0].basin_precip_mm, 200.0);
        assert!(r.contains(30.0, 70.0) && !r.contains(38.0, 70.0));
        let no_lakes = r#"{"box": [1, 0, 0, 1], "links": []}"#;
        assert!(parse_reference(no_lakes).unwrap().lakes.is_empty());
        for bad in [
            r#"{"links": []}"#,
            r#"{"box": [0, 0, 1, 1], "links": []}"#,
            r#"{"box": [1, 0, 0, 1], "links": [{"lonlat": [[0, 0]], "area_km2": [1],
                "elevation_m": [1], "discharge_m3s": 1}]}"#,
            r#"{"box": [1, 0, 0, 1], "links": [{"lonlat": [[0, 0], [0, 1]], "area_km2": [1],
                "elevation_m": [1, 2], "discharge_m3s": 1}]}"#,
        ] {
            assert!(parse_reference(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn an_outlet_trace_follows_the_largest_river_to_its_end() {
        // A main river east along 20 nodes into the ocean, and a small one beside its start
        // ending at the ice.
        let lattice = CubeLattice { n: 64 };
        let start = lattice.nearest(RenderDir(DVec3::new(0.3, 0.2, 1.0).normalize()));
        let main = lattice_walk(&lattice, start, 0, 20);
        let sea = main[20];
        let main = main[..20].to_vec();
        let side = lattice_walk(&lattice, lattice.neighbours(start)[6].unwrap(), 6, 2);
        let chain = |nodes: Vec<u32>, q: f32, end| RiverChain {
            q: vec![q; nodes.len()],
            bed: vec![0.0; nodes.len()],
            nodes,
            end,
        };
        let ice = side[2];
        let hydro = river_hydro(
            lattice,
            &[
                chain(main.clone(), 500.0, Down::Ocean(sea)),
                chain(side[..2].to_vec(), 50.0, Down::Ice(ice)),
            ],
            Vec::new(),
        );
        let from = lattice.dir(start).0;
        let mid = lattice.dir(main[10]).0;
        let t = trace_outlet(
            &hydro,
            from,
            3.0 * lattice.mean_spacing_m() * 1e-3,
            Some(mid),
        )
        .expect("a river in reach");
        assert_eq!(hydro.rivers.nodes()[t.start as usize].node, main[0]);
        assert_eq!((t.nodes, t.end), (20, Down::Ocean(sea)));
        assert_eq!(t.end_dir, lattice.dir(sea).0);
        assert!(t.via_km.unwrap() < 1e-6, "{t:?}");
        // A via point off the path: its distance to the nearest stretch.
        let off = lattice.dir(lattice.neighbours(main[10])[2].unwrap()).0;
        let km = trace_outlet(&hydro, from, 50.0, Some(off))
            .unwrap()
            .via_km
            .unwrap();
        let step = lattice.distance_m(main[10], lattice.neighbours(main[10])[2].unwrap()) * 1e-3;
        assert!((km - step).abs() < 0.05 * step, "{km} vs {step}");
        // From the small river's end, with a reach that holds only its last node: its own end.
        let t = trace_outlet(&hydro, lattice.dir(side[1]).0, 1.0, None).unwrap();
        assert_eq!((t.nodes, t.end, t.via_km), (1, Down::Ice(ice), None));
        // Nothing in reach.
        let far = RenderDir(DVec3::new(-1.0, 0.0, 0.0)).0;
        assert_eq!(trace_outlet(&hydro, far, 50.0, None), None);
    }

    #[test]
    fn an_outlet_trace_passes_through_a_lake_with_an_outflow() {
        // A river east into a lake two nodes wide; another leaves it beside its exit for the
        // ocean. A lake with no outflow ends the trace.
        let lattice = CubeLattice { n: 64 };
        let start = lattice.nearest(RenderDir(DVec3::new(0.3, 0.2, 1.0).normalize()));
        let walk = lattice_walk(&lattice, start, 0, 16);
        let (inflow, lake, outflow, sea) = (&walk[..6], &walk[6..8], &walk[8..15], walk[15]);
        let lake_with = |outflow_m3s| Lake {
            level_m: 100.0,
            spill_m: 100.0,
            kind: LakeKind::Data,
            inflow_m3s: 120.0,
            evaporation_m3s: 120.0 - outflow_m3s,
            outflow_m3s,
            area_km2: 2000.0,
            exit: lake[1],
        };
        let chain = |nodes: &[u32], q: f32, end| RiverChain {
            q: vec![q; nodes.len()],
            bed: vec![0.0; nodes.len()],
            nodes: nodes.to_vec(),
            end,
        };
        let chains = [
            chain(inflow, 120.0, Down::Lake(0, lake[0])),
            chain(outflow, 100.0, Down::Ocean(sea)),
        ];
        let from = lattice.dir(start).0;
        let hydro = river_hydro(lattice, &chains, vec![lake_with(100.0)]);
        let t = trace_outlet(&hydro, from, 1.0, None).unwrap();
        assert_eq!(
            (t.nodes, t.lakes, t.end),
            (13, 1, Down::Ocean(sea)),
            "{t:?}"
        );
        let closed = river_hydro(lattice, &chains, vec![lake_with(0.0)]);
        let t = trace_outlet(&closed, from, 1.0, None).unwrap();
        assert_eq!(
            (t.nodes, t.lakes, t.end),
            (6, 0, Down::Lake(0, lake[0])),
            "{t:?}"
        );
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(64))]

        /// The index finds the same nearest segment as a search of all of them.
        #[test]
        fn the_index_finds_the_nearest_segment(
            lines in prop::collection::vec(
                prop::collection::vec((-60.0f64..60.0, -60.0f64..60.0), 1..6), 0..8),
            probes in prop::collection::vec((-90.0f64..90.0, -90.0f64..90.0), 1..10),
            cell_km in prop::sample::select(vec![0.5, 5.0, 40.0]),
        ) {
            let game: Vec<Vec<DVec3>> = lines.iter().map(|l| line(l)).collect();
            let km = EARTH_RADIUS_M * 1e-3;
            let index = SegmentIndex::new(&game, cell_km / km);
            for &(x, y) in &probes {
                let p = at(x, y).dir;
                let brute = index
                    .segments
                    .iter()
                    .map(|&(a, b)| angle_to_arc(p, a, b))
                    .fold(f64::INFINITY, f64::min);
                prop_assert_eq!(index.nearest(p), brute);
            }
        }
    }
}
