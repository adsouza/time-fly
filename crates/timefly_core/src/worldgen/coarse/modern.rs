//! The deep field's two-level path for ETOPO-dominant brackets (30 ka to about 2.2 Ma, where
//! the modern-terrain bridge frame carries at least half the weight and the source spacing is
//! the tiles' 1,852 m).
//!
//! Resolving plates at every fine lattice node would cost a full [`rendered_surface`] each
//! (every candidate plate's rotation and footprint, and both endpoints): about 4 µs, or 6–11 s
//! for a region. So the field splits in two:
//!
//! - **Support nodes**, on a fixed cube lattice spaced `DEEP_SPACING_M / 4` (27.75 km at a face
//!   centre), run the globe's reference at overview level and store the elevation *minus its
//!   ETOPO term* (`residual`), the ETOPO term's weight, which plate carries the node (or none,
//!   for the ocean shell's era-direction reads) and the key kind. They are computed on demand
//!   and cached; each is a pure function of its index.
//! - **A fine sample** at direction `d` is `bilerp(residual) + bilerp(weight) · T(d)`, where
//!   `T(d) = etopo(P(d)) − anomaly` is read per sample at `P(d)`: the nearest support node's
//!   plate's present-day position of `d`, exactly as the globe's ranking pass rotates it (or `d`
//!   itself for shell nodes).
//!
//! At a support node the fine sample equals the reference (at overview level) exactly. Between
//! nodes, the non-ETOPO part (the PaleoDEM endpoint, at about 1°) is bilinear on a quarter of its
//! own spacing, and the ETOPO part is read at full resolution, so coastlines follow ETOPO's own
//! contour carried with the plate. On a plate, nodes use the plate's own value even where it is
//! under water (the globe shows the ocean shell there), so the fine coast is the plate value's
//! zero contour: the globe's land rule at full resolution. Only shell nodes (gaps, era-fixed
//! points) use the shell's value.
//!
//! [`rendered_surface`]: crate::globe::rendered_surface

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::sync::RwLock;

use bevy_math::{DQuat, Quat};

use crate::cube::{CubeFace, bilinear_taps, texel_center_dir};
use crate::geo::RenderDir;
use crate::geodynamics::{PlateIndex, ResolvedPosition};
use crate::globe::{
    OCEAN_GAP_FALLBACK_M, RenderedSurface, blend_endpoints_m, endpoint_available, endpoint_weights,
    relative_rotation, rendered_surface, shell_surface_m,
};
use crate::worldgen::REGION_SIZE_M;
use crate::worldgen::frame::{EARTH_RADIUS_M, RegionFrame};
use crate::worldgen::tiles::{SourceLevel, TILE_MARGIN_M, TileSet};

use super::{
    CoarseSample, DEEP_SPACING_M, DeepField, KeyKind, cube_bilinear, koppen_code, lat_lon,
};

/// Cube size of the support lattice: face-centre texels of `DEEP_SPACING_M / 4`.
pub(super) fn support_cube_size() -> u32 {
    (2.0 * EARTH_RADIUS_M / (DEEP_SPACING_M / 4.0)).ceil() as u32
}

/// How a plate's ETOPO endpoint is read from an era direction: the globe's
/// `relative_rotation(source, current) · d`, exactly the identity when the two are equal.
#[derive(Copy, Clone, Debug, PartialEq)]
pub(super) enum PresentRead {
    Identity,
    Rotate(DQuat),
    /// The plate has no current or no source rotation: the endpoint is unavailable.
    Missing,
}

impl PresentRead {
    pub(super) fn of(source: Option<Quat>, current: Option<Quat>) -> PresentRead {
        match (source, current) {
            (Some(s), Some(c)) => {
                let r = relative_rotation(s.normalize(), c.normalize());
                if r == Quat::IDENTITY {
                    PresentRead::Identity
                } else {
                    PresentRead::Rotate(r.as_dquat())
                }
            }
            _ => PresentRead::Missing,
        }
    }

    fn apply(&self, d: RenderDir) -> RenderDir {
        match self {
            PresentRead::Rotate(q) => RenderDir((*q * d.0).normalize()),
            PresentRead::Identity | PresentRead::Missing => d,
        }
    }
}

/// Two plates meeting inside a support cell keep the two-level formula only where every node is
/// at least this far from sea level, on the same side.
const CLEAR_OF_SEA_LEVEL_M: f64 = 300.0;

/// Whether a support cell's samples take the reference: plate nodes against shell nodes, or
/// different plates with a node near sea level or nodes on both sides of it.
fn needs_reference(carriers: [Option<PlateIndex>; 4], values: [f64; 4]) -> bool {
    if carriers.iter().all(|&c| c == carriers[0]) {
        return false;
    }
    if carriers.iter().any(|c| c.is_none()) {
        return true;
    }
    let near = values
        .iter()
        .any(|v| v.abs() < CLEAR_OF_SEA_LEVEL_M || v.is_nan());
    let both = values.iter().any(|&v| v > 0.0) && values.iter().any(|&v| v <= 0.0);
    near || both
}

/// One support node: see the module doc.
#[derive(Copy, Clone, Debug, PartialEq)]
pub(super) struct SupportNode {
    /// The node's value (the reference's elevation, or its plate's raw value under water).
    value_m: f64,
    residual_m: f64,
    etopo_weight: f64,
    /// The plate whose present position the ETOPO term is read at; `None` reads at `d`.
    pub(super) carrier: Option<PlateIndex>,
    /// A shell node whose value is a thin crack's frame land (`DeepField::sliver_land_m`).
    sliver: bool,
    kind: KeyKind,
}

/// The support nodes computed so far, shared by clones of the field. Values are pure functions
/// of their index, so the cache never changes a result.
#[derive(Default)]
pub(super) struct SupportCache(RwLock<HashMap<(u8, u32, u32), SupportNode>>);

impl fmt::Debug for SupportCache {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let len = self.0.read().map_or(0, |m| m.len());
        write!(f, "SupportCache({len} nodes)")
    }
}

/// A plate's raw value at an era direction (the globe's blend before the visibility test), its
/// ETOPO endpoints' total weight, and the ETOPO term `etopo − anomaly` it read.
struct PlateTerms {
    value_m: f64,
    etopo_weight: f64,
    etopo_term_m: f64,
}

impl<'a> DeepField<'a> {
    /// Whether this field takes the two-level path.
    pub(super) fn is_modern(&self) -> bool {
        self.etopo_weight() >= 0.5
    }

    fn etopo_endpoint(&self) -> usize {
        if self.etopo[0] { 0 } else { 1 }
    }

    /// The ETOPO endpoint's elevation and Köppen code at a direction in its published frame
    /// (present-day geography): a tile or the overview by `level`, else the bridge frame's cube.
    /// The same value [`DeepField::endpoint_sample`] gives that endpoint.
    fn etopo_read(&self, dir: RenderDir, level: SourceLevel) -> (f64, Option<u8>) {
        let (lat, lon) = lat_lon(dir);
        match self.tiles.lookup(lat, lon, level) {
            Some((h, code)) => (f64::from(h), koppen_code(code)),
            None => {
                let [h] = cube_bilinear(self.frames[self.etopo_endpoint()], dir, [0]);
                (h, None)
            }
        }
    }

    /// A plate's terms at an era direction, reading endpoints where the globe's ranking pass
    /// does. `None` when neither endpoint is available.
    fn plate_terms(&self, plate: PlateIndex, era: RenderDir) -> Option<PlateTerms> {
        let p = plate.0 as usize;
        let plate_fixed = RenderDir((self.to_plate[p]? * era.0).normalize());
        let ages = self.sampling.source_ages_myr();
        let footprints = self.crust.footprints();
        let available = [0, 1].map(|e| {
            endpoint_available(
                self.rotations[p].sources[e].is_some(),
                footprints.contains(plate, plate_fixed, ages[e]),
            )
        });
        let weights = endpoint_weights(available, self.sampling.older_weight)?;
        let anomalies = self.sampling.anomalies_m();
        let star = self.etopo_endpoint();
        let mut etopo_m = 0.0;
        let elevation = [0, 1].map(|e| {
            if weights[e] == 0.0 {
                return 0.0;
            }
            let dir =
                PresentRead::of(self.rotations[p].sources[e], self.rotations[p].current).apply(era);
            let h = self
                .endpoint_sample(e, dir, SourceLevel::Overview)
                .elevation_m;
            if e == star {
                etopo_m = h;
            }
            h
        });
        let etopo_weight: f64 = (0..2).filter(|&e| self.etopo[e]).map(|e| weights[e]).sum();
        Some(PlateTerms {
            value_m: blend_endpoints_m(weights, elevation, anomalies),
            etopo_weight,
            etopo_term_m: etopo_m - anomalies[star],
        })
    }

    /// Computes a support node from the globe's reference at overview level.
    fn compute_support(&self, face: CubeFace, col: u32, row: u32) -> SupportNode {
        let dir = texel_center_dir(face, col, row, self.support_n);
        let surface = rendered_surface(self.crust, dir, &self.sampling, &self.rotations, |e, d| {
            self.endpoint_sample(e, d, SourceLevel::Overview)
        });
        let resolved = match surface {
            RenderedSurface::Land { plate_id, .. } => self.plate_index(plate_id),
            RenderedSurface::Ocean => {
                match self.crust.resolve(dir, self.sampling.age_myr, |_| None) {
                    ResolvedPosition::Tracked(pos) => self.plate_index(pos.plate_id),
                    ResolvedPosition::EraFixed { .. } => None,
                }
            }
            RenderedSurface::GapLand { .. } => None,
        };
        if let Some(plate) = resolved
            && let Some(t) = self.plate_terms(plate, dir)
        {
            let kind = KeyKind::Tracked(self.crust.rotations().plate_ids[plate.0 as usize]);
            return SupportNode {
                value_m: t.value_m,
                residual_m: t.value_m - t.etopo_weight * t.etopo_term_m,
                etopo_weight: t.etopo_weight,
                carrier: Some(plate),
                sliver: false,
                kind,
            };
        }
        // The ocean shell (both endpoints at the era direction, mixed by the older weight), or
        // the frames' land in a thin era-fixed sliver, as the reference path shows it.
        let key = self.tracked_or_era_fixed(dir.0);
        let (value, sliver) = match surface {
            RenderedSurface::GapLand { elevation_m } => (elevation_m, false),
            _ => self.ocean_value(dir, &key, SourceLevel::Overview),
        };
        let w = self.sampling.older_weight;
        let mix = [1.0 - w, w];
        let etopo_weight: f64 = (0..2).filter(|&e| self.etopo[e]).map(|e| mix[e]).sum();
        let star = self.etopo_endpoint();
        let term =
            self.etopo_read(dir, SourceLevel::Overview).0 - self.sampling.anomalies_m()[star];
        SupportNode {
            value_m: value,
            residual_m: value - etopo_weight * term,
            etopo_weight,
            carrier: None,
            sliver,
            kind: key.kind(),
        }
    }

    pub(super) fn support(&self, face: CubeFace, col: u32, row: u32) -> SupportNode {
        let key = (face as u8, col, row);
        let cached = match self.support.0.read() {
            Ok(map) => map.get(&key).copied(),
            Err(poisoned) => poisoned.into_inner().get(&key).copied(),
        };
        if let Some(node) = cached {
            return node;
        }
        let node = self.compute_support(face, col, row);
        match self.support.0.write() {
            Ok(mut map) => map.insert(key, node),
            Err(poisoned) => poisoned.into_inner().insert(key, node),
        };
        node
    }

    /// The two-level sample at `d` (see the module doc), by the support cell's taps:
    /// - all on one plate: `bilerp(residual) + bilerp(weight) · T(d)`;
    /// - all shell nodes, none a sliver: the globe's shell rule evaluated at `d` itself (both
    ///   endpoints read there, so coverage-mask edges fall where the reference has them);
    /// - where land and sea can change inside the cell for a reason the formula does not model
    ///   (`needs_reference`: plate against shell, a thin crack's 50 km ring, or two plates
    ///   meeting near sea level): the reference itself at overview level, so land and sea agree
    ///   with it across the edge;
    /// - two plates meeting on clearly dry (or clearly drowned) ground: the formula, with the
    ///   nearest node's plate.
    pub(super) fn modern_sample(&self, era_dir: RenderDir, level: SourceLevel) -> CoarseSample {
        let d = RenderDir(era_dir.0.normalize());
        let (mut residual, mut weight) = (0.0, 0.0);
        let (mut best_w, mut nearest) = (-1.0, None);
        let mut carriers = [None; 4];
        let mut values = [0.0; 4];
        let mut sliver = false;
        for (k, ((face, col, row), w)) in bilinear_taps(d, self.support_n).into_iter().enumerate() {
            let node = self.support(face, col, row);
            residual += w * node.residual_m;
            weight += w * node.etopo_weight;
            carriers[k] = node.carrier;
            values[k] = node.value_m;
            sliver |= node.sliver;
            if w > best_w {
                best_w = w;
                nearest = Some(node);
            }
        }
        let nearest = nearest.expect("four taps");
        if sliver || needs_reference(carriers, values) {
            return self.sample_with(era_dir, SourceLevel::Overview);
        }
        if carriers[0].is_none() {
            return self.shell_sample(d, &nearest, level);
        }
        let read = match nearest.carrier {
            Some(plate) => self.present[plate.0 as usize].apply(d),
            None => d,
        };
        let (etopo, koppen) = self.etopo_read(read, level);
        let term = etopo - self.sampling.anomalies_m()[self.etopo_endpoint()];
        CoarseSample {
            elevation_m: residual + weight * term,
            ice: 0.0,
            koppen,
            lattice: <Self as super::CoarseField>::lattice_key(self, d, nearest.kind),
        }
    }

    /// The shell rule at `d`, read at `level`: the globe's shell blend (gap land where uncovered
    /// land shows, otherwise water, covered land at the crack fill).
    fn shell_sample(
        &self,
        d: RenderDir,
        nearest: &SupportNode,
        level: SourceLevel,
    ) -> CoarseSample {
        let samples = [
            self.endpoint_sample(0, d, level),
            self.endpoint_sample(1, d, level),
        ];
        let shell = shell_surface_m(&self.sampling, samples);
        let elevation_m = if shell.is_nan() {
            OCEAN_GAP_FALLBACK_M
        } else {
            shell
        };
        CoarseSample {
            elevation_m,
            ice: 0.0,
            koppen: self.etopo_read(d, level).1,
            lattice: <Self as super::CoarseField>::lattice_key(self, d, nearest.kind),
        }
    }

    /// The tiles the inner field around `frame` reads on the two-level path: the era square
    /// itself (shell nodes) and the square carried by the present-day rotation of every plate
    /// valid at a support node the square taps (a superset of the carriers).
    pub(super) fn modern_tiles_needed(&self, frame: &RegionFrame) -> Vec<(u8, u8)> {
        let basis = frame.basis();
        // Sample points finer than the smallest support cell (a cube corner's, about 0.47 of a
        // face-centre cell), over the square widened by a cell, hit every cell that intersects
        // the square, so their taps are every node a texel can take its plate from.
        let cell_m = DEEP_SPACING_M / 4.0;
        let half = REGION_SIZE_M / 2.0 + TILE_MARGIN_M + cell_m;
        let step = 0.2 * cell_m;
        let count = (2.0 * half / step).ceil() as u32;
        let mut nodes: HashSet<(CubeFace, u32, u32)> = HashSet::new();
        for a in 0..=count {
            for b in 0..=count {
                let x = -half + 2.0 * half * f64::from(a) / f64::from(count);
                let z = -half + 2.0 * half * f64::from(b) / f64::from(count);
                for (tap, _) in bilinear_taps(basis.local_to_dir(x, z), self.support_n) {
                    nodes.insert(tap);
                }
            }
        }
        let mut plates: HashSet<u32> = HashSet::new();
        for (face, col, row) in nodes {
            let dir = texel_center_dir(face, col, row, self.support_n);
            self.crust.resolve(dir, self.sampling.age_myr, |c| {
                plates.insert(c.plate_index.0);
                None
            });
        }
        let mut out = TileSet::needed_for(frame);
        let mut plates: Vec<u32> = plates.into_iter().collect();
        plates.sort_unstable();
        for p in plates {
            if let PresentRead::Rotate(q) = self.present[p as usize] {
                out.extend(TileSet::needed_for_rotated(frame, q));
            }
        }
        out.sort_unstable();
        out.dedup();
        out
    }
}
