//! Paleo-surface: the timeline of baked frames, the recent-to-deep bridge, sea level and frame
//! selection (spec §19). Everything here returns frame indices; the shell maps them to images.

pub mod timeline;

pub use timeline::{
    Bracket, DeepBracket, FrameMeta, FrameSelection, FrameSource, Timeline, TimelineError, bracket,
    effective_elevation_m,
};
