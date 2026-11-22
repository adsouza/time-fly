//! Readers for the globe binary formats written by the Python bake tool (`timefly_bake.formats`).
//!
//! Everything here is a pure function of a byte slice. Inputs may be corrupt or truncated web
//! downloads, so every size is computed with checked arithmetic, decompression is capped at the
//! expected length, and every failure is a [`FormatError`], never a panic.

use std::fmt;
use std::io::Read;

use ruzstd::decoding::StreamingDecoder;
use tracing::instrument;

use crate::geodynamics::{FootprintEntry, PlateFootprints, PlateIndex, RotationTable};

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum GridKind {
    Cube,
    LatLon,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum DType {
    F16,
    U16,
    F32,
}

impl DType {
    fn size(self) -> usize {
        match self {
            DType::F16 | DType::U16 => 2,
            DType::F32 => 4,
        }
    }
}

/// A decoded `.tfgrid`: `data` is the decompressed little-endian array, laid out
/// `[face][row][col][channel]` for cubes (faces in [`crate::cube::CubeFace`] order) and
/// `[row][col][channel]` for lat-lon grids.
#[derive(Clone, Debug, PartialEq)]
pub struct Grid {
    pub kind: GridKind,
    pub dtype: DType,
    pub channels: u8,
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>,
}

impl Grid {
    fn element_index(&self, face: usize, row: u32, col: u32, ch: u8) -> usize {
        assert!(
            row < self.height && col < self.width && ch < self.channels,
            "texel out of range"
        );
        assert!(
            face < if self.kind == GridKind::Cube { 6 } else { 1 },
            "face out of range"
        );
        let (h, w, c) = (
            self.height as usize,
            self.width as usize,
            self.channels as usize,
        );
        ((face * h + row as usize) * w + col as usize) * c + ch as usize
    }

    fn word(&self, index: usize) -> [u8; 2] {
        let at = index * 2;
        [self.data[at], self.data[at + 1]]
    }

    /// The raw 16-bit word of a texel (the bit pattern, for `F16`).
    ///
    /// Panics if the texel is out of range or the grid is `F32`.
    pub fn texel_u16(&self, face: usize, row: u32, col: u32, ch: u8) -> u16 {
        assert!(self.dtype != DType::F32, "texel_u16 on an f32 grid");
        u16::from_le_bytes(self.word(self.element_index(face, row, col, ch)))
    }

    /// A texel as `f32`; `F16` is widened and `U16` converted numerically.
    ///
    /// Panics if the texel is out of range.
    pub fn texel_f32(&self, face: usize, row: u32, col: u32, ch: u8) -> f32 {
        let index = self.element_index(face, row, col, ch);
        match self.dtype {
            DType::F16 => half::f16::from_le_bytes(self.word(index)).to_f32(),
            DType::U16 => f32::from(u16::from_le_bytes(self.word(index))),
            DType::F32 => {
                let at = index * 4;
                let d = &self.data;
                f32::from_le_bytes([d[at], d[at + 1], d[at + 2], d[at + 3]])
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FormatError {
    BadMagic,
    Truncated,
    UnknownKind(u8),
    UnknownDType(u8),
    SizeMismatch {
        expected: usize,
        got: usize,
    },
    Zstd(String),
    /// The structure decoded but its contents break a documented rule.
    Invalid(&'static str),
}

impl fmt::Display for FormatError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FormatError::BadMagic => write!(f, "bad magic"),
            FormatError::Truncated => write!(f, "truncated file"),
            FormatError::UnknownKind(k) => write!(f, "unknown grid kind {k}"),
            FormatError::UnknownDType(d) => write!(f, "unknown dtype {d}"),
            FormatError::SizeMismatch { expected, got } => {
                write!(f, "size mismatch: expected {expected} bytes, got {got}")
            }
            FormatError::Zstd(e) => write!(f, "zstd: {e}"),
            FormatError::Invalid(why) => write!(f, "invalid contents: {why}"),
        }
    }
}

impl std::error::Error for FormatError {}

fn le_u32(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

fn le_f64(b: &[u8]) -> f64 {
    f64::from_le_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]])
}

/// Checks the magic, then returns `(header bytes after the magic, zstd payload)`, where the
/// header is `head_len` bytes ending in the `zlen` field. The file must end with the payload.
fn split<'a>(
    bytes: &'a [u8],
    magic: &[u8; 8],
    head_len: usize,
) -> Result<(&'a [u8], &'a [u8]), FormatError> {
    let start = bytes.get(..8).ok_or(FormatError::Truncated)?;
    if start != magic {
        return Err(FormatError::BadMagic);
    }
    let head = bytes.get(8..8 + head_len).ok_or(FormatError::Truncated)?;
    let zlen = le_u32(head, head_len - 4) as usize;
    let end = (8 + head_len)
        .checked_add(zlen)
        .ok_or(FormatError::Truncated)?;
    let payload = bytes.get(8 + head_len..end).ok_or(FormatError::Truncated)?;
    if end != bytes.len() {
        return Err(FormatError::SizeMismatch {
            expected: end,
            got: bytes.len(),
        });
    }
    Ok((head, payload))
}

/// Decompresses exactly `expected` bytes, reading no more than `expected + 1` so a hostile
/// frame cannot allocate unboundedly.
fn decompress(payload: &[u8], expected: usize) -> Result<Vec<u8>, FormatError> {
    let mut decoder =
        StreamingDecoder::new(payload).map_err(|e| FormatError::Zstd(e.to_string()))?;
    let mut out = Vec::new();
    let cap = u64::try_from(expected)
        .unwrap_or(u64::MAX)
        .saturating_add(1);
    (&mut decoder)
        .take(cap)
        .read_to_end(&mut out)
        .map_err(|e| FormatError::Zstd(e.to_string()))?;
    if out.len() != expected {
        return Err(FormatError::SizeMismatch {
            expected,
            got: out.len(),
        });
    }
    Ok(out)
}

fn mul(a: usize, b: usize) -> Result<usize, FormatError> {
    a.checked_mul(b)
        .ok_or(FormatError::Invalid("size overflow"))
}

fn add(a: usize, b: usize) -> Result<usize, FormatError> {
    a.checked_add(b)
        .ok_or(FormatError::Invalid("size overflow"))
}

/// Reads a `.tfgrid` file.
#[instrument(level = "debug", skip_all, fields(len = bytes.len()), err)]
pub fn read_tfgrid(bytes: &[u8]) -> Result<Grid, FormatError> {
    let (head, payload) = split(bytes, b"TFGRID01", 16)?;
    let kind = match head[0] {
        0 => GridKind::Cube,
        1 => GridKind::LatLon,
        k => return Err(FormatError::UnknownKind(k)),
    };
    let dtype = match head[1] {
        0 => DType::F16,
        1 => DType::U16,
        2 => DType::F32,
        d => return Err(FormatError::UnknownDType(d)),
    };
    let channels = head[2];
    if head[3] != 0 {
        return Err(FormatError::Invalid("reserved byte is not zero"));
    }
    let (width, height) = (le_u32(head, 4), le_u32(head, 8));
    if channels == 0 || width == 0 || height == 0 {
        return Err(FormatError::Invalid("zero grid dimension"));
    }
    if kind == GridKind::Cube && width != height {
        return Err(FormatError::Invalid("cube faces must be square"));
    }
    let faces = if kind == GridKind::Cube { 6 } else { 1 };
    let texels = mul(mul(faces, height as usize)?, width as usize)?;
    let expected = mul(mul(texels, usize::from(channels))?, dtype.size())?;
    let data = decompress(payload, expected)?;
    Ok(Grid {
        kind,
        dtype,
        channels,
        width,
        height,
        data,
    })
}

/// Reads a `.tfrot` file.
#[instrument(level = "debug", skip_all, fields(len = bytes.len()), err)]
pub fn read_tfrot(bytes: &[u8]) -> Result<RotationTable, FormatError> {
    use crate::geodynamics::PlateId;

    let (head, payload) = split(bytes, b"TFROT002", 12)?;
    let n_plates = le_u32(head, 0) as usize;
    let n_times = le_u32(head, 4) as usize;
    if n_times == 0 {
        return Err(FormatError::Invalid("no rotation times"));
    }
    let records = mul(n_times, n_plates)?;
    let ids_len = mul(n_plates, 4)?;
    let times_len = mul(n_times, 8)?;
    let quats_len = mul(records, 16)?;
    let expected = add(add(add(ids_len, times_len)?, quats_len)?, records)?;
    let data = decompress(payload, expected)?;

    let (ids, rest) = data.split_at(ids_len);
    let (times, rest) = rest.split_at(times_len);
    let (quats, valid) = rest.split_at(quats_len);

    let plate_ids: Vec<PlateId> = ids.chunks_exact(4).map(|c| PlateId(le_u32(c, 0))).collect();
    let mut sorted: Vec<u32> = plate_ids.iter().map(|p| p.0).collect();
    sorted.sort_unstable();
    if sorted.windows(2).any(|w| w[0] == w[1]) {
        return Err(FormatError::Invalid("duplicate plate id"));
    }

    let times_myr: Vec<f64> = times.chunks_exact(8).map(le_f64).collect();
    if times_myr[0] != 0.0
        || !times_myr.iter().all(|t| t.is_finite())
        || times_myr.windows(2).any(|w| w[0] >= w[1])
    {
        return Err(FormatError::Invalid(
            "times must be finite, ascending and start at 0",
        ));
    }

    let mut flags = Vec::with_capacity(records);
    for &b in valid {
        flags.push(match b {
            0 => false,
            1 => true,
            _ => return Err(FormatError::Invalid("validity byte outside 0..=1")),
        });
    }

    let mut quat_values = Vec::with_capacity(records);
    for (i, q) in quats.chunks_exact(16).enumerate() {
        let f = |k: usize| f32::from_le_bytes([q[k * 4], q[k * 4 + 1], q[k * 4 + 2], q[k * 4 + 3]]);
        let quat = [f(0), f(1), f(2), f(3)];
        if flags[i] && !quat.iter().all(|v| v.is_finite()) {
            return Err(FormatError::Invalid(
                "non-finite quaternion in a valid record",
            ));
        }
        quat_values.push(quat);
    }

    Ok(RotationTable {
        plate_ids,
        times_myr,
        quats: quat_values,
        valid: flags,
    })
}

/// Footprint intervals lie within the 540 Ma the model covers.
const MAX_AGE_MYR: f64 = 540.0;
const ENTRY_BYTES: usize = 20;

/// Reads a `.tfplates` file.
#[instrument(level = "debug", skip_all, fields(len = bytes.len()), err)]
pub fn read_tfplates(bytes: &[u8]) -> Result<PlateFootprints, FormatError> {
    let (head, payload) = split(bytes, b"TFPLAT01", 12)?;
    let cube_size = le_u32(head, 0);
    let n_entries = le_u32(head, 4) as usize;
    if cube_size == 0 {
        return Err(FormatError::Invalid("zero cube size"));
    }
    let cells = mul(mul(6, cube_size as usize)?, cube_size as usize)?;
    let offsets_len = mul(add(cells, 1)?, 4)?;
    let expected = add(offsets_len, mul(n_entries, ENTRY_BYTES)?)?;
    let data = decompress(payload, expected)?;
    let (off_bytes, ent_bytes) = data.split_at(offsets_len);

    let offsets: Vec<u32> = off_bytes.chunks_exact(4).map(|c| le_u32(c, 0)).collect();
    if offsets[0] != 0
        || offsets[cells] as usize != n_entries
        || offsets.windows(2).any(|w| w[0] > w[1])
    {
        return Err(FormatError::Invalid(
            "offsets must start at 0, end at n_entries and not decrease",
        ));
    }

    let entries: Vec<FootprintEntry> = ent_bytes
        .chunks_exact(ENTRY_BYTES)
        .map(|c| FootprintEntry {
            plate_index: PlateIndex(le_u32(c, 0)),
            younger_myr: le_f64(&c[4..12]),
            older_myr: le_f64(&c[12..20]),
        })
        .collect();
    for e in &entries {
        let in_range = |v: f64| (0.0..=MAX_AGE_MYR).contains(&v); // false for NaN
        if !in_range(e.younger_myr) || !in_range(e.older_myr) || e.younger_myr > e.older_myr {
            return Err(FormatError::Invalid(
                "footprint interval outside 0..=540 Ma or reversed",
            ));
        }
    }
    for w in offsets.windows(2) {
        let cell = &entries[w[0] as usize..w[1] as usize];
        let key = |e: &FootprintEntry| (e.plate_index, e.younger_myr);
        if cell.windows(2).any(|p| key(&p[0]) > key(&p[1])) {
            return Err(FormatError::Invalid(
                "cell entries not sorted by plate index then younger bound",
            ));
        }
    }

    Ok(PlateFootprints {
        cube_size,
        offsets,
        entries,
    })
}

/// Checks that every footprint entry names a plate present in the rotation table.
pub fn check_footprints_against_rotations(
    footprints: &PlateFootprints,
    rotations: &RotationTable,
) -> Result<(), FormatError> {
    let n_plates = rotations.plate_ids.len();
    if footprints
        .entries
        .iter()
        .any(|e| e.plate_index.0 as usize >= n_plates)
    {
        return Err(FormatError::Invalid(
            "footprint plate index beyond the rotation table",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ruzstd::encoding::{CompressionLevel, compress_to_vec};

    fn frame(magic: &[u8; 8], head: &[u8], payload: &[u8]) -> Vec<u8> {
        let z = compress_to_vec(payload, CompressionLevel::Fastest);
        let mut out = magic.to_vec();
        out.extend_from_slice(head);
        out.extend_from_slice(&(z.len() as u32).to_le_bytes());
        out.extend_from_slice(&z);
        out
    }

    fn u32s(v: &[u32]) -> Vec<u8> {
        v.iter().flat_map(|x| x.to_le_bytes()).collect()
    }

    fn rot_file(valid: &[u8], times: &[f64], q: [f32; 4]) -> Vec<u8> {
        // 1 plate, `times.len()` times.
        let mut p = u32s(&[7]);
        p.extend(times.iter().flat_map(|t| t.to_le_bytes()));
        for _ in times {
            p.extend(q.iter().flat_map(|v| v.to_le_bytes()));
        }
        p.extend_from_slice(valid);
        frame(b"TFROT002", &[u32s(&[1, times.len() as u32])].concat(), &p)
    }

    fn plates_file(offsets: &[u32], entries: &[(u32, f64, f64)]) -> Vec<u8> {
        let mut p = u32s(offsets);
        for (i, a, b) in entries {
            p.extend(i.to_le_bytes());
            p.extend(a.to_le_bytes());
            p.extend(b.to_le_bytes());
        }
        frame(b"TFPLAT01", &u32s(&[1, entries.len() as u32]), &p)
    }

    fn offsets_for(n_entries: u32, last_cell_start: u32) -> Vec<u32> {
        // cube_size 1 => 6 cells, 7 offsets.
        let mut o = vec![0; 7];
        o[6] = n_entries;
        o[5] = last_cell_start;
        o
    }

    #[test]
    fn rot_round_trip_and_bad_validity() {
        let q = [0.0, 0.0, 0.0, 1.0];
        let ok = read_tfrot(&rot_file(&[1, 0], &[0.0, 5.0], q)).unwrap();
        assert_eq!(ok.valid, vec![true, false]);
        let err = read_tfrot(&rot_file(&[1, 2], &[0.0, 5.0], q)).unwrap_err();
        assert!(matches!(err, FormatError::Invalid(_)));
    }

    #[test]
    fn rot_rejects_bad_times() {
        let q = [0.0, 0.0, 0.0, 1.0];
        for times in [
            [1.0, 2.0],
            [0.0, 0.0],
            [0.0, f64::NAN],
            [0.0, f64::INFINITY],
        ] {
            assert!(
                read_tfrot(&rot_file(&[1, 1], &times, q)).is_err(),
                "{times:?}"
            );
        }
    }

    #[test]
    fn rot_non_finite_only_matters_when_valid() {
        let q = [f32::NAN, 0.0, 0.0, 1.0];
        assert!(read_tfrot(&rot_file(&[1, 1], &[0.0, 1.0], q)).is_err());
        assert!(read_tfrot(&rot_file(&[0, 0], &[0.0, 1.0], q)).is_ok());
    }

    #[test]
    fn plates_rejects_bad_offsets() {
        let e = [(0, 0.0, 10.0), (1, 0.0, 10.0)];
        assert!(read_tfplates(&plates_file(&offsets_for(2, 1), &e)).is_ok());
        let mut decreasing = offsets_for(2, 1);
        decreasing[3] = 2;
        assert!(read_tfplates(&plates_file(&decreasing, &e)).is_err());
        let mut nonzero_start = offsets_for(2, 1);
        nonzero_start[0] = 1;
        assert!(read_tfplates(&plates_file(&nonzero_start, &e)).is_err());
        let short_end = offsets_for(1, 1);
        assert!(read_tfplates(&plates_file(&short_end, &e)).is_err());
    }

    #[test]
    fn plates_rejects_bad_intervals_and_order() {
        let o = offsets_for(2, 0);
        for e in [
            [(0, 5.0, 1.0), (1, 0.0, 1.0)],
            [(0, 0.0, 541.0), (1, 0.0, 1.0)],
            [(0, -1.0, 1.0), (1, 0.0, 1.0)],
            [(0, f64::NAN, 1.0), (1, 0.0, 1.0)],
            [(1, 0.0, 1.0), (0, 0.0, 1.0)], // all in one cell, unsorted by plate
        ] {
            assert!(read_tfplates(&plates_file(&o, &e)).is_err(), "{e:?}");
        }
        // Sorted by plate then younger bound is fine, including a repeated plate.
        let ok = [(0, 0.0, 1.0), (0, 2.0, 3.0)];
        assert!(read_tfplates(&plates_file(&o, &ok)).is_ok());
    }

    #[test]
    fn rejects_mis_sized_payload_and_trailing_bytes() {
        let head = [0u8, 2, 1, 0, 1, 0, 0, 0, 1, 0, 0, 0]; // cube, u16, 1 channel, 1x1
        let short = frame(b"TFGRID01", &head, &[0; 11]); // wants 12
        assert!(matches!(
            read_tfgrid(&short),
            Err(FormatError::SizeMismatch {
                expected: 24,
                got: 11
            })
        ));
        let long = frame(b"TFGRID01", &head, &[0; 100]);
        assert!(matches!(
            read_tfgrid(&long),
            Err(FormatError::SizeMismatch { expected: 24, .. })
        ));
        let mut trailing = frame(b"TFGRID01", &head, &[0; 24]);
        assert!(read_tfgrid(&trailing).is_ok());
        trailing.push(0);
        assert!(read_tfgrid(&trailing).is_err());
    }

    #[test]
    fn rejects_unknown_kind_dtype_and_garbage_payload() {
        let bad_kind = frame(b"TFGRID01", &[9, 0, 1, 0, 1, 0, 0, 0, 1, 0, 0, 0], &[0; 12]);
        assert!(matches!(
            read_tfgrid(&bad_kind),
            Err(FormatError::UnknownKind(9))
        ));
        let bad_dtype = frame(b"TFGRID01", &[0, 9, 1, 0, 1, 0, 0, 0, 1, 0, 0, 0], &[0; 12]);
        assert!(matches!(
            read_tfgrid(&bad_dtype),
            Err(FormatError::UnknownDType(9))
        ));
        let mut garbage = b"TFGRID01".to_vec();
        garbage.extend([0, 2, 1, 0, 1, 0, 0, 0, 1, 0, 0, 0, 4, 0, 0, 0, 1, 2, 3, 4]);
        assert!(matches!(read_tfgrid(&garbage), Err(FormatError::Zstd(_))));
    }

    #[test]
    fn huge_dimensions_error_without_allocating() {
        let mut head = vec![0u8, 2, 255, 0];
        head.extend(u32::MAX.to_le_bytes());
        head.extend(u32::MAX.to_le_bytes());
        let f = frame(b"TFGRID01", &head, &[0; 8]);
        assert!(read_tfgrid(&f).is_err());
        let mut p = b"TFPLAT01".to_vec();
        p.extend(u32s(&[u32::MAX, u32::MAX, 0]));
        assert!(read_tfplates(&p).is_err());
    }

    #[test]
    fn footprint_indices_are_checked_against_rotations() {
        let rot = read_tfrot(&rot_file(&[1], &[0.0], [0.0, 0.0, 0.0, 1.0])).unwrap();
        let fits = read_tfplates(&plates_file(&offsets_for(1, 0), &[(0, 0.0, 1.0)])).unwrap();
        assert!(check_footprints_against_rotations(&fits, &rot).is_ok());
        let beyond = read_tfplates(&plates_file(&offsets_for(1, 0), &[(1, 0.0, 1.0)])).unwrap();
        assert!(check_footprints_against_rotations(&beyond, &rot).is_err());
    }
}
