//! CPU copies of `.tfgrid` files: the globe frames a region's coarse field reads, the tiles and
//! the overview. The globe's own frames are GPU-only images ([`crate::data::TfgridLoader`]), so
//! region generation loads the few it needs again through [`CpuGridLoader`].
//!
//! Bevy picks the loader by the requested asset type: `server.load::<CpuGrid>(path)` uses this
//! loader and `server.load::<Image>(path)` the image loader, for the same `.tfgrid` file, with
//! no label (see the Task 5 report, "Deviations").

use std::fmt;
use std::sync::Arc;

use bevy::asset::io::Reader;
use bevy::asset::{AssetLoader, LoadContext, LoadState};
use bevy::prelude::*;
use serde::{Deserialize, Serialize};
use timefly_core::formats::{FormatError, Grid, read_tfgrid};
use timefly_core::sha256::sha256_hex;
use timefly_core::surface::Timeline;
use timefly_core::worldgen::coarse::frames_needed;
use tracing::instrument;

use super::job::{LOAD_TIMEOUT_S, load_timed_out};
use crate::data::{GlobeData, describe_load_error};

/// A decoded `.tfgrid` kept on the CPU. It never becomes an [`Image`]. The region plugin takes
/// each one out of `Assets<CpuGrid>` as soon as it loads, so it holds no handle afterwards.
#[derive(Asset, TypePath, Debug)]
pub struct CpuGrid(pub Grid);

/// What a [`CpuGridLoader`] checks before decoding. Tiles and the overview pass their manifest
/// entry; the globe frames (whose manifest lists no hashes) pass nothing.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CpuGridSettings {
    /// The expected file size.
    pub bytes: Option<u64>,
    /// The expected lowercase hex SHA-256 of the file.
    pub sha256: Option<String>,
}

/// Why a CPU grid failed to load. Messages name no file path: the caller logs the asset path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CpuGridError {
    Io(String),
    /// The file's size differs from the manifest's (a truncated or partial download).
    Size {
        expected: u64,
        got: u64,
    },
    /// The file's SHA-256 differs from the manifest's.
    Sha256 {
        expected: String,
        got: String,
    },
    Format(FormatError),
}

impl fmt::Display for CpuGridError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CpuGridError::Io(e) => write!(f, "could not read: {e}"),
            CpuGridError::Size { expected, got } => {
                write!(f, "size {got} B does not match the manifest's {expected} B")
            }
            CpuGridError::Sha256 { expected, got } => write!(
                f,
                "sha256 {} does not match the manifest's {}",
                &got[..16.min(got.len())],
                &expected[..16.min(expected.len())]
            ),
            CpuGridError::Format(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for CpuGridError {}

/// Loads a `.tfgrid` as a [`CpuGrid`], checking its size and SHA-256 when the settings give
/// them.
#[derive(Default, TypePath)]
pub struct CpuGridLoader;

impl AssetLoader for CpuGridLoader {
    type Asset = CpuGrid;
    type Settings = CpuGridSettings;
    type Error = CpuGridError;

    async fn load(
        &self,
        reader: &mut dyn Reader,
        settings: &CpuGridSettings,
        load_context: &mut LoadContext<'_>,
    ) -> Result<CpuGrid, CpuGridError> {
        let mut bytes = Vec::new();
        reader
            .read_to_end(&mut bytes)
            .await
            .map_err(|e| CpuGridError::Io(e.to_string()))?;
        decode_cpu_grid(&bytes, settings, &load_context.path().to_string())
    }

    fn extensions(&self) -> &[&str] {
        &["tfgrid"]
    }
}

/// Checks `bytes` against `settings` and decodes them.
#[instrument(level = "debug", skip_all, fields(path = %path, len = bytes.len()), err)]
pub fn decode_cpu_grid(
    bytes: &[u8],
    settings: &CpuGridSettings,
    path: &str,
) -> Result<CpuGrid, CpuGridError> {
    if let Some(expected) = settings.bytes {
        let got = bytes.len() as u64;
        if got != expected {
            return Err(CpuGridError::Size { expected, got });
        }
    }
    if let Some(expected) = &settings.sha256 {
        let got = sha256_hex(bytes);
        if !got.eq_ignore_ascii_case(expected) {
            return Err(CpuGridError::Sha256 {
                expected: expected.clone(),
                got,
            });
        }
    }
    read_tfgrid(bytes)
        .map(CpuGrid)
        .map_err(CpuGridError::Format)
}

/// Decoded globe frames kept between requests (ruling R11), so a snapshot change or a new
/// region at a nearby snapshot does not load and decode the same files again. At most
/// [`FRAME_CACHE_SLOTS`] files (about 0.8 MB each at the bake's cube size of 128), least
/// recently used first out; frames the current request reads are never evicted. It lives for the
/// session and is not cleared when a region is left: it is bounded, and re-entering is common.
#[derive(Resource, Default, Debug)]
pub struct FrameCache {
    frames: Vec<CachedFrame>,
    clock: u64,
}

/// Frame files the [`FrameCache`] holds: a bridge snapshot reads up to four.
pub const FRAME_CACHE_SLOTS: usize = 4;

#[derive(Debug)]
struct CachedFrame {
    /// The file's image slot in `GlobeData`.
    slot: u32,
    grid: Arc<Grid>,
    last_used: u64,
}

impl FrameCache {
    pub fn len(&self) -> usize {
        self.frames.len()
    }

    pub fn is_empty(&self) -> bool {
        self.frames.is_empty()
    }

    /// The cached grid of image slot `slot`, marked as just used.
    pub fn get(&mut self, slot: u32) -> Option<Arc<Grid>> {
        self.clock += 1;
        let clock = self.clock;
        self.frames.iter_mut().find(|f| f.slot == slot).map(|f| {
            f.last_used = clock;
            f.grid.clone()
        })
    }

    /// The cached grid of image slot `slot`, without marking it used (a read that must not
    /// change what the region job keeps).
    pub fn peek(&self, slot: u32) -> Option<Arc<Grid>> {
        self.frames
            .iter()
            .find(|f| f.slot == slot)
            .map(|f| f.grid.clone())
    }

    /// Adds a slot's grid, then evicts the least recently used slots outside `keep` (the current
    /// request's slots) down to [`FRAME_CACHE_SLOTS`].
    pub fn insert(&mut self, slot: u32, grid: Arc<Grid>, keep: &[u32]) {
        self.clock += 1;
        self.frames.retain(|f| f.slot != slot);
        self.frames.push(CachedFrame {
            slot,
            grid,
            last_used: self.clock,
        });
        while self.frames.len() > FRAME_CACHE_SLOTS {
            let victim = self
                .frames
                .iter()
                .enumerate()
                .filter(|(_, f)| !keep.contains(&f.slot))
                .min_by_key(|(_, f)| f.last_used)
                .map(|(i, _)| i);
            match victim {
                Some(i) => {
                    self.frames.remove(i);
                }
                None => break,
            }
        }
    }
}

/// Takes a loaded CPU grid out of the asset storage (dropping the caller's handle afterwards
/// leaves no copy behind).
pub(crate) fn take_grid(
    grids: &mut Assets<CpuGrid>,
    handle: &Handle<CpuGrid>,
) -> Option<Arc<Grid>> {
    grids.remove(handle).map(|CpuGrid(grid)| Arc::new(grid))
}

/// The frame files a snapshot reads: each image slot once, with the timeline frames it serves.
fn frame_files(timeline: &Timeline, age_myr: f64) -> Vec<(u32, Vec<usize>)> {
    let mut files: Vec<(u32, Vec<usize>)> = Vec::new();
    for frame in frames_needed(timeline, age_myr) {
        let slot = timeline.frames()[frame].image_slot;
        match files.iter_mut().find(|(s, _)| *s == slot) {
            Some((_, frames)) => frames.push(frame),
            None => files.push((slot, vec![frame])),
        }
    }
    files
}

/// A frame file loading for some timeline frames (repeated files share one load).
struct PendingFrame {
    slot: u32,
    frames: Vec<usize>,
    handle: Handle<CpuGrid>,
    started_s: f64,
}

/// The globe frames one snapshot's coarse field reads, from the [`FrameCache`] or loaded as
/// [`CpuGrid`]s. The region job and the continental pass each hold one; both may load the
/// same file at once (the asset server shares the load), so a load whose grid another holder
/// already took is answered from the cache.
pub struct FrameLoads {
    /// The image slots this snapshot reads, kept in the [`FrameCache`].
    slots: Vec<u32>,
    pending: Vec<PendingFrame>,
    frames: Vec<Option<Arc<Grid>>>,
    fetched: u32,
}

impl FrameLoads {
    /// Starts the loads for `age_myr`'s frames that the cache does not hold (`now_s` on the
    /// [`LoadClock`](crate::clock::LoadClock)).
    pub fn start(
        globe: &GlobeData,
        age_myr: f64,
        server: &AssetServer,
        cache: &mut FrameCache,
        now_s: f64,
    ) -> FrameLoads {
        let timeline = &globe.timeline;
        let mut frames = vec![None; timeline.frames().len()];
        let mut slots = Vec::new();
        let mut pending = Vec::new();
        for (slot, served) in frame_files(timeline, age_myr) {
            slots.push(slot);
            match cache.get(slot) {
                Some(grid) => {
                    for &f in &served {
                        frames[f] = Some(grid.clone());
                    }
                }
                None => pending.push(PendingFrame {
                    slot,
                    handle: server.load::<CpuGrid>(globe.frame_path(served[0]).clone()),
                    frames: served,
                    started_s: now_s,
                }),
            }
        }
        FrameLoads {
            fetched: u32::try_from(pending.len()).unwrap_or(u32::MAX),
            slots,
            pending,
            frames,
        }
    }

    /// Frame files loaded for it (not in the cache when it started).
    pub fn fetched(&self) -> u32 {
        self.fetched
    }

    /// Takes in finished loads. `Some(Ok(frames))` once every frame is in (timeline frame `i`
    /// at index `i`, `None` for frames the snapshot does not read), after which the loads are
    /// spent; `Some(Err(why))` when a frame failed, was lost or timed out
    /// ([`LOAD_TIMEOUT_S`]); `None` while loading.
    pub fn poll(
        &mut self,
        server: &AssetServer,
        grids: &mut Assets<CpuGrid>,
        cache: &mut FrameCache,
        now_s: f64,
    ) -> Option<Result<Vec<Option<Arc<Grid>>>, String>> {
        let mut failure = None;
        let (slots, frames) = (&self.slots, &mut self.frames);
        self.pending.retain(|p| {
            let mut keep = |grid: Arc<Grid>| {
                for &f in &p.frames {
                    frames[f] = Some(grid.clone());
                }
                false
            };
            // Another holder took the shared load's grid into the cache.
            if let Some(grid) = cache.peek(p.slot) {
                return keep(grid);
            }
            match server.load_state(&p.handle) {
                LoadState::Loaded => match take_grid(grids, &p.handle) {
                    Some(grid) => {
                        cache.insert(p.slot, grid.clone(), slots);
                        keep(grid)
                    }
                    None => {
                        failure = Some(format!("frame {} vanished after loading", p.frames[0]));
                        false
                    }
                },
                LoadState::Failed(e) => {
                    let path = server.get_path(&p.handle).map(|p| p.to_string());
                    failure = Some(describe_load_error(path.as_deref().unwrap_or("?"), &e));
                    false
                }
                _ if load_timed_out(p.started_s, now_s, None) => {
                    let path = server.get_path(&p.handle).map(|p| p.to_string());
                    failure = Some(format!(
                        "{}: the load timed out after {LOAD_TIMEOUT_S:.0} s",
                        path.as_deref().unwrap_or("?")
                    ));
                    false
                }
                _ => true,
            }
        });
        if let Some(why) = failure {
            self.pending.clear();
            return Some(Err(why.trim_end().to_string()));
        }
        self.pending
            .is_empty()
            .then(|| Ok(std::mem::take(&mut self.frames)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_cache_is_bounded_lru_and_keeps_the_current_slots() {
        let grid = |v: u8| {
            Arc::new(Grid {
                kind: timefly_core::formats::GridKind::Cube,
                dtype: timefly_core::formats::DType::F16,
                channels: 1,
                width: 1,
                height: 1,
                data: vec![v; 12],
            })
        };
        let mut cache = FrameCache::default();
        for slot in 0..4 {
            cache.insert(slot, grid(slot as u8), &[slot]);
        }
        assert_eq!(cache.len(), 4);
        // Slot 0 is touched, so slot 1 is the least recently used.
        assert_eq!(cache.get(0).unwrap().data[0], 0);
        cache.insert(4, grid(4), &[4]);
        assert_eq!(cache.len(), FRAME_CACHE_SLOTS);
        assert!(cache.get(1).is_none());
        assert!(cache.get(0).is_some());
        // The current request's slots survive even when they are the oldest.
        let mut cache = FrameCache::default();
        for slot in 0..4 {
            cache.insert(slot, grid(0), &[]);
        }
        cache.insert(9, grid(9), &[0, 1, 2, 9]);
        assert!(cache.get(3).is_none());
        assert!([0, 1, 2, 9].iter().all(|&s| cache.get(s).is_some()));
        assert!(!cache.is_empty());
    }

    /// A valid 1 × 1 single-channel f16 lat-lon `.tfgrid`, compressed with ruzstd.
    fn tiny_tfgrid() -> Vec<u8> {
        let raw = half::f16::from_f32(12.5).to_le_bytes().to_vec();
        let z = ruzstd::encoding::compress_to_vec(
            &raw[..],
            ruzstd::encoding::CompressionLevel::Fastest,
        );
        let mut out = b"TFGRID01".to_vec();
        out.extend_from_slice(&[1, 0, 1, 0]);
        out.extend_from_slice(&1u32.to_le_bytes());
        out.extend_from_slice(&1u32.to_le_bytes());
        out.extend_from_slice(&(z.len() as u32).to_le_bytes());
        out.extend_from_slice(&z);
        out
    }

    #[test]
    fn checks_size_and_hash_before_decoding() {
        let bytes = tiny_tfgrid();
        let good = CpuGridSettings {
            bytes: Some(bytes.len() as u64),
            sha256: Some(sha256_hex(&bytes).to_uppercase()),
        };
        let grid = decode_cpu_grid(&bytes, &good, "t.tfgrid").unwrap().0;
        assert_eq!((grid.width, grid.height, grid.channels), (1, 1, 1));
        assert_eq!(grid.texel_f32(0, 0, 0, 0), 12.5);
        // No settings: decode only.
        assert!(decode_cpu_grid(&bytes, &CpuGridSettings::default(), "t").is_ok());

        let truncated = &bytes[..bytes.len() - 3];
        assert_eq!(
            decode_cpu_grid(truncated, &good, "t").unwrap_err(),
            CpuGridError::Size {
                expected: bytes.len() as u64,
                got: bytes.len() as u64 - 3
            }
        );
        let mut flipped = bytes.clone();
        *flipped.last_mut().unwrap() ^= 1;
        let err = decode_cpu_grid(&flipped, &good, "t").unwrap_err();
        assert!(matches!(err, CpuGridError::Sha256 { .. }), "{err:?}");
        assert!(err.to_string().starts_with("sha256 "), "{err}");
        // Without a manifest entry, a truncated file is a format error, not a panic.
        let err = decode_cpu_grid(truncated, &CpuGridSettings::default(), "t").unwrap_err();
        assert!(matches!(err, CpuGridError::Format(_)), "{err:?}");
    }
}
