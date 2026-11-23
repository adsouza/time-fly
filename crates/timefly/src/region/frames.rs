//! CPU copies of `.tfgrid` files: the globe frames a region's coarse field reads, the tiles and
//! the overview. The globe's own frames are GPU-only images ([`crate::data::TfgridLoader`]), so
//! region generation loads the few it needs again through [`CpuGridLoader`].
//!
//! Bevy picks the loader by the requested asset type: `server.load::<CpuGrid>(path)` uses this
//! loader and `server.load::<Image>(path)` the image loader, for the same `.tfgrid` file, with
//! no label (see the Task 5 report, "Deviations").

use std::fmt;

use bevy::asset::io::Reader;
use bevy::asset::{AssetLoader, LoadContext};
use bevy::prelude::*;
use serde::{Deserialize, Serialize};
use timefly_core::formats::{FormatError, Grid, read_tfgrid};
use timefly_core::sha256::sha256_hex;
use tracing::instrument;

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

#[cfg(test)]
mod tests {
    use super::*;

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
