//! The continental water pass (rivers and lakes design §4.1): a pure function of an era's
//! elevation on a global lattice. This part fills depressions, picks the ocean, routes every
//! node to a receiver and lists the basins. Climate, lakes and discharge come later (Task 3).
//!
//! Every rule here is mirrored by the Python reference pass (Task 11), so the neighbour order,
//! the tie rules and the float formats are part of the contract, not implementation details.

mod lattice;
pub mod params;
mod route;

pub use lattice::{CubeLattice, GridGraph, NEIGHBOUR_STEPS};
pub use route::{Basin, NO_BASIN, RECEIVER_EXIT, RECEIVER_NONE, Routed, Routing, receiver_of};

/// A graph the pass routes over. Node ids are `0..len()`.
pub trait HydroGraph: Sync {
    /// The number of nodes.
    fn len(&self) -> u32;

    /// `true` when the graph has no nodes.
    fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The neighbours of `i` in the order E, NE, N, NW, W, SW, S, SE ([`NEIGHBOUR_STEPS`]).
    /// `None` where a step leaves the graph or would repeat a neighbour (cube corners).
    /// Neighbourhood is symmetric: `j` is a neighbour of `i` exactly when `i` is one of `j`.
    fn neighbours(&self, i: u32) -> [Option<u32>; 8];

    /// The distance between nodes `i` and `j` in metres.
    fn distance_m(&self, i: u32, j: u32) -> f64;

    /// The area node `i` stands for, in square metres.
    fn area_m2(&self, i: u32) -> f64;

    /// The mean distance between neighbouring nodes in metres. Sets the ocean's strait reach.
    fn mean_spacing_m(&self) -> f64;
}
