//! The global coarse lattice: cube-map texel centres whose face-centre texel spans
//! `max(spacing/4, texel)`, upsampled with [`bilinear_taps`]. A region precomputes the nodes it
//! touches (planned, then filled, a bounded number of units at a time); any other node is
//! computed on demand, through the same quantisation, so a texel never depends on what was
//! precomputed.

use crate::cube::{CubeFace, bilinear_taps, texel_center_dir};
use crate::geo::RenderDir;
use crate::worldgen::coarse::{CoarseField, CoarseSample, KeyKind};
use crate::worldgen::frame::{EARTH_RADIUS_M, FrameBasis};
use crate::worldgen::tiles::SourceLevel;

/// Sample points per side when planning which nodes a region touches.
const PLAN_POINTS: u32 = 128;
/// Planning units per lattice: one per sample point.
pub(super) const PLAN_UNITS: u32 = (PLAN_POINTS + 1) * (PLAN_POINTS + 1);

/// A coarse node: elevation, ice (quantised to bytes, as the output is), Köppen code (0 = none)
/// and its key's kind. Precomputed and on-demand nodes go through the same quantisation, so a
/// texel never depends on which nodes a region happened to precompute.
#[derive(Copy, Clone, Debug, PartialEq)]
pub(super) struct Node {
    pub(super) elevation_m: f32,
    pub(super) ice: u8,
    pub(super) koppen: u8,
    pub(super) kind: KeyKind,
}

impl Node {
    fn from_sample(s: CoarseSample) -> Node {
        let e = s.elevation_m;
        // Never let a bad source value become NaN terrain: NaN is sea, infinities clamp.
        let e = if e.is_nan() {
            -1.0
        } else {
            e.clamp(-12_000.0, 12_000.0)
        };
        let ice = if s.ice.is_finite() {
            s.ice.clamp(0.0, 1.0)
        } else {
            0.0
        };
        Node {
            elevation_m: e as f32,
            ice: (ice * 255.0).round() as u8,
            koppen: s.koppen.unwrap_or(0),
            kind: s.lattice.kind(),
        }
    }
}

/// Nodes per allocation of a lattice's node store ([`NodeStore`]): 64 Ki nodes, 512 KiB.
pub(super) const NODE_CHUNK: usize = 1 << 16;

/// A block's nodes in order, in allocations of exactly [`NODE_CHUNK`] nodes. A region's lattice
/// holds 17-31 MiB of nodes, and its size varies with the region. macOS keeps a freed block
/// resident and reuses it only for a request of the same size, so one allocation per block
/// left each region's lattice in the process footprint after it was freed. In chunks of one
/// size, the next region's lattice reuses them (M3 hop memory).
#[derive(Clone, Debug, Default)]
pub(super) struct NodeStore {
    chunks: Vec<Vec<PackedNode>>,
    len: usize,
}

impl NodeStore {
    pub(super) fn len(&self) -> usize {
        self.len
    }

    pub(super) fn push(&mut self, node: PackedNode) {
        if self.len.is_multiple_of(NODE_CHUNK) {
            self.chunks.push(Vec::with_capacity(NODE_CHUNK));
        }
        self.chunks
            .last_mut()
            .expect("a chunk was just added")
            .push(node);
        self.len += 1;
    }

    pub(super) fn get(&self, i: usize) -> Option<PackedNode> {
        self.chunks
            .get(i / NODE_CHUNK)?
            .get(i % NODE_CHUNK)
            .copied()
    }

    /// Each allocation's size in bytes.
    pub(super) fn allocation_bytes(&self) -> impl Iterator<Item = usize> + '_ {
        self.chunks
            .iter()
            .map(|c| c.capacity() * std::mem::size_of::<PackedNode>())
    }
}

impl std::ops::Index<usize> for NodeStore {
    type Output = PackedNode;

    fn index(&self, i: usize) -> &PackedNode {
        &self.chunks[i / NODE_CHUNK][i % NODE_CHUNK]
    }
}

/// A stored node: 8 bytes, with the key kind as an index into [`Lattice::kinds`]. A recent
/// region near a cube corner precomputes about 4 million nodes.
#[derive(Copy, Clone, Debug)]
pub(super) struct PackedNode {
    pub(super) elevation_m: f32,
    ice: u8,
    koppen: u8,
    kind: u16,
}

#[cfg(test)]
impl PackedNode {
    pub(super) fn test_node(elevation_m: f32) -> PackedNode {
        PackedNode {
            elevation_m,
            ice: 0,
            koppen: 0,
            kind: 0,
        }
    }
}

/// Precomputed nodes of one cube face, over a column/row rectangle, filled row by row.
#[derive(Clone, Debug)]
pub(super) struct Block {
    pub(super) c0: u32,
    pub(super) r0: u32,
    pub(super) w: u32,
    pub(super) h: u32,
    pub(super) nodes: NodeStore,
}

impl Block {
    fn get(&self, col: u32, row: u32) -> Option<PackedNode> {
        let (c, r) = (col.wrapping_sub(self.c0), row.wrapping_sub(self.r0));
        if c < self.w && r < self.h {
            self.nodes.get((r * self.w + c) as usize)
        } else {
            None
        }
    }

    fn len(&self) -> usize {
        (self.w * self.h) as usize
    }
}

/// The interpolated coarse field at a direction.
pub(super) struct Coarse {
    pub(super) elevation_m: f64,
    pub(super) ice: f64,
    /// The highest-weight tap's key kind: the texel's plate.
    pub(super) kind: KeyKind,
    /// The highest-weight tap with a Köppen code, if any.
    pub(super) koppen: Option<u8>,
    /// Every tap's key kind and bilinear weight, so detail can blend across key boundaries.
    pub(super) taps: [(KeyKind, f64); 4],
}

/// The global coarse lattice of one cube size and source level, with the nodes a region needs
/// precomputed.
#[derive(Clone, Debug)]
pub(super) struct Lattice {
    pub(super) n: u32,
    /// Which data the nodes read: the apron's lattice reads only the overview (ruling R7).
    level: SourceLevel,
    /// Half-side (m) of the square whose taps are precomputed.
    half_m: f64,
    pub(super) blocks: [Option<Block>; 6],
    /// The distinct key kinds of the stored nodes.
    pub(super) kinds: Vec<KeyKind>,
    /// Planning progress (sample points done) and the faces' column/row bounds so far.
    next_point: u32,
    bbox: [Option<[u32; 4]>; 6],
    /// Filling progress: the face being filled.
    fill_face: usize,
}

impl Lattice {
    /// An unplanned lattice: nothing is sampled until [`plan`](Self::plan) and
    /// [`fill`](Self::fill) run.
    pub(super) fn new(n: u32, level: SourceLevel, half_m: f64) -> Lattice {
        Lattice {
            n,
            level,
            half_m,
            blocks: Default::default(),
            kinds: Vec::new(),
            next_point: 0,
            bbox: [None; 6],
            fill_face: 0,
        }
    }

    /// Plans up to `budget` sample points of the square; returns how many it did. When the last
    /// point is done the face blocks are sized: between sample points the footprint can reach
    /// one point step further, in nodes as small as a cube corner's (under half a face-centre
    /// node).
    pub(super) fn plan(&mut self, basis: &FrameBasis, budget: u32) -> u32 {
        let start = self.next_point;
        let end = start.saturating_add(budget).min(PLAN_UNITS);
        let half = self.half_m;
        for k in start..end {
            let (a, b) = (k / (PLAN_POINTS + 1), k % (PLAN_POINTS + 1));
            let x = -half + 2.0 * half * f64::from(a) / f64::from(PLAN_POINTS);
            let z = -half + 2.0 * half * f64::from(b) / f64::from(PLAN_POINTS);
            for ((face, col, row), _) in bilinear_taps(basis.local_to_dir(x, z), self.n) {
                let bb = &mut self.bbox[face as usize];
                *bb = Some(match *bb {
                    None => [col, col, row, row],
                    Some([c0, c1, r0, r1]) => [c0.min(col), c1.max(col), r0.min(row), r1.max(row)],
                });
            }
        }
        self.next_point = end;
        if end == PLAN_UNITS && start < end {
            let node_m = 2.0 * EARTH_RADIUS_M / f64::from(self.n);
            let step_m = 2.0 * half / f64::from(PLAN_POINTS);
            let grow = (step_m / (0.45 * node_m)).ceil() as u32 + 2;
            let last = self.n - 1;
            for face in CubeFace::ALL {
                let Some([c0, c1, r0, r1]) = self.bbox[face as usize] else {
                    continue;
                };
                let (c0, r0) = (c0.saturating_sub(grow), r0.saturating_sub(grow));
                let (c1, r1) = ((c1 + grow).min(last), (r1 + grow).min(last));
                let (w, h) = (c1 - c0 + 1, r1 - r0 + 1);
                self.blocks[face as usize] = Some(Block {
                    c0,
                    r0,
                    w,
                    h,
                    nodes: NodeStore::default(),
                });
            }
        }
        end - start
    }

    pub(super) fn planned(&self) -> bool {
        self.next_point == PLAN_UNITS
    }

    /// Samples up to `budget` more nodes, face by face and row by row; returns how many.
    pub(super) fn fill<F: CoarseField>(&mut self, field: &F, budget: u32) -> u32 {
        let mut used = 0;
        while used < budget && self.fill_face < 6 {
            let Some(block) = self.blocks[self.fill_face].as_mut() else {
                self.fill_face += 1;
                continue;
            };
            while used < budget && block.nodes.len() < block.len() {
                let k = block.nodes.len() as u32;
                let (col, row) = (block.c0 + k % block.w, block.r0 + k / block.w);
                let face = CubeFace::ALL[self.fill_face];
                let node = sample_node(field, self.level, face, col, row, self.n);
                // Kinds repeat in long runs (one plate, or geographic everywhere).
                let kind = match self.kinds.iter().rposition(|&k| k == node.kind) {
                    Some(i) => i,
                    None => {
                        self.kinds.push(node.kind);
                        self.kinds.len() - 1
                    }
                };
                block.nodes.push(PackedNode {
                    elevation_m: node.elevation_m,
                    ice: node.ice,
                    koppen: node.koppen,
                    kind: u16::try_from(kind).expect("fewer than 65,536 plates"),
                });
                used += 1;
            }
            if block.nodes.len() == block.len() {
                self.fill_face += 1;
            }
        }
        used
    }

    pub(super) fn filled(&self) -> bool {
        self.fill_face == 6
    }

    fn node<F: CoarseField>(&self, field: &F, face: CubeFace, col: u32, row: u32) -> Node {
        match self.blocks[face as usize]
            .as_ref()
            .and_then(|b| b.get(col, row))
        {
            Some(p) => Node {
                elevation_m: p.elevation_m,
                ice: p.ice,
                koppen: p.koppen,
                kind: self.kinds[p.kind as usize],
            },
            None => sample_node(field, self.level, face, col, row, self.n),
        }
    }

    pub(super) fn interp<F: CoarseField>(&self, field: &F, dir: RenderDir) -> Coarse {
        let (mut e, mut ice) = (0.0, 0.0);
        let (mut best_w, mut kind) = (-1.0, KeyKind::EraFixed);
        let (mut koppen_w, mut koppen) = (-1.0, None);
        let mut taps = [(KeyKind::EraFixed, 0.0); 4];
        for (k, ((face, col, row), w)) in bilinear_taps(dir, self.n).into_iter().enumerate() {
            let node = self.node(field, face, col, row);
            taps[k] = (node.kind, w);
            e += w * f64::from(node.elevation_m);
            ice += w * f64::from(node.ice) / 255.0;
            if w > best_w {
                best_w = w;
                kind = node.kind;
            }
            if node.koppen != 0 && w > koppen_w {
                koppen_w = w;
                koppen = Some(node.koppen);
            }
        }
        Coarse {
            elevation_m: e,
            ice,
            kind,
            koppen,
            taps,
        }
    }

    pub(super) fn elevation<F: CoarseField>(&self, field: &F, dir: RenderDir) -> f64 {
        bilinear_taps(dir, self.n)
            .iter()
            .map(|&((face, col, row), w)| {
                w * f64::from(self.node(field, face, col, row).elevation_m)
            })
            .sum()
    }
}

fn sample_node<F: CoarseField>(
    field: &F,
    level: SourceLevel,
    face: CubeFace,
    col: u32,
    row: u32,
    n: u32,
) -> Node {
    let dir = texel_center_dir(face, col, row, n);
    Node::from_sample(match level {
        SourceLevel::Tiles => field.sample(dir),
        SourceLevel::Overview => field.sample_overview(dir),
    })
}
