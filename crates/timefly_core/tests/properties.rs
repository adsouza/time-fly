//! Property tests for the functional core (proptest).
//!
//! Each block states an invariant from the module's documentation and checks it over generated
//! inputs. Several blocks also close gaps that `cargo mutants -p timefly_core` found: mutants that
//! changed behaviour without failing any test. See `docs/testing/mutation-testing.md`.

use bevy_math::{DQuat, DVec3, Quat};
use proptest::prelude::*;
use ruzstd::encoding::{CompressionLevel, compress_to_vec};
use timefly_core::cube::{CubeFace, bilinear_taps, dir_to_texel, texel_center_dir};
use timefly_core::deep_time::{
    MAX_YEARS_BP, STANDARD_SNAPSHOTS, YearsBp, format_years_bp, next_snapshot, prev_snapshot,
    slider_to_years, years_to_slider,
};
use timefly_core::formats::{DType, FormatError, GridKind, read_tfgrid, read_tfplates, read_tfrot};
use timefly_core::geo::{
    GeoDir, RenderDir, geo_quat_to_render, geo_to_latlon, geo_to_render, latlon_to_geo,
    render_to_geo,
};
use timefly_core::geodynamics::{
    CrustModel, CrustModelError, CrustPos, FootprintEntry, PlateFootprints, PlateId, PlateIndex,
    RotationTable, interval_accepts,
};
use timefly_core::globe::{
    RenderedSurface, blend_endpoints_m, displacement_km, endpoint_weights, relative_rotation,
    source_id_ranks, visible_land_m,
};
use timefly_core::surface::{FrameMeta, FrameSource, Timeline, TimelineError, bracket};

// ---------------------------------------------------------------------------------------------
// Generators
// ---------------------------------------------------------------------------------------------

/// A coordinate that is often exactly 0 or ±1, so face edges, corners and axis ties come up.
fn coord() -> impl Strategy<Value = f64> {
    prop_oneof![
        6 => -1.0f64..=1.0,
        1 => Just(1.0),
        1 => Just(-1.0),
        1 => Just(0.0),
    ]
}

/// A non-zero direction (not normalized) with frequent edge and tie cases.
fn direction() -> impl Strategy<Value = DVec3> {
    (coord(), coord(), coord())
        .prop_map(|(x, y, z)| DVec3::new(x, y, z))
        .prop_filter("non-zero", |v| v.length() > 1e-6)
}

fn unit_quat() -> impl Strategy<Value = DQuat> {
    (direction(), -std::f64::consts::PI..std::f64::consts::PI)
        .prop_map(|(axis, angle)| DQuat::from_axis_angle(axis.normalize(), angle))
}

fn face() -> impl Strategy<Value = CubeFace> {
    prop::sample::select(CubeFace::ALL.to_vec())
}

/// Ascending, distinct ages (Ma), at least one.
fn ascending_ages(max_len: usize) -> impl Strategy<Value = Vec<f64>> {
    prop::collection::vec(0.001f64..10.0, 1..max_len).prop_map(|gaps| {
        let mut t = 0.0;
        gaps.iter()
            .map(|g| {
                let a = t;
                t += g;
                a
            })
            .collect()
    })
}

// ---------------------------------------------------------------------------------------------
// deep_time
// ---------------------------------------------------------------------------------------------

/// The age a slider label stands for, and how far from the true age its rounding may be.
fn label_value(label: &str) -> Option<(f64, f64)> {
    if label == "Present" {
        return Some((0.0, 0.5));
    }
    let number = |s: &str| s.replace(',', "").parse::<f64>().ok();
    if let Some(n) = label.strip_suffix(" million years ago") {
        let v = number(n)?;
        let tol = if n.contains('.') || v < 10.0 {
            0.5e5
        } else {
            0.5e6
        };
        return Some((v * 1e6, tol));
    }
    if let Some(n) = label.strip_suffix(" thousand years ago") {
        return Some((number(n)? * 1e3, 500.0));
    }
    if label == "1 year ago" {
        return Some((1.0, 0.5));
    }
    let n = label.strip_suffix(" years ago")?;
    Some((number(n)?, 0.5))
}

proptest! {
    #[test]
    fn slider_round_trips(y in 0.0f64..=MAX_YEARS_BP) {
        let back = slider_to_years(years_to_slider(YearsBp(y))).0;
        prop_assert!((back - y).abs() <= 1e-9 * (y + 50.0), "{y} -> {back}");
    }

    #[test]
    fn slider_is_monotone_and_clamped(a in -1.0f64..2.0, b in -1.0f64..2.0) {
        let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
        let (ylo, yhi) = (slider_to_years(lo).0, slider_to_years(hi).0);
        prop_assert!(ylo <= yhi);
        prop_assert!((0.0..=MAX_YEARS_BP * (1.0 + 1e-12)).contains(&ylo));
        prop_assert!((0.0..=MAX_YEARS_BP * (1.0 + 1e-12)).contains(&yhi));
        let s = years_to_slider(YearsBp(a * 1e9));
        prop_assert!((0.0..=1.0).contains(&s));
    }

    #[test]
    fn myr_conversions(m in -1000.0f64..1000.0) {
        prop_assert_eq!(YearsBp::from_myr(m).0, m * 1.0e6);
        prop_assert!((YearsBp::from_myr(m).myr() - m).abs() <= 1e-12 * (1.0 + m.abs()));
        prop_assert_eq!(YearsBp::from_1950_bp(m).0, m + 76.0);
    }

    #[test]
    fn snapshot_steps_land_on_the_nearest_snapshot(y in -1.0e3f64..6.0e8) {
        let next = next_snapshot(YearsBp(y)).0;
        let prev = prev_snapshot(YearsBp(y)).0;
        prop_assert!(STANDARD_SNAPSHOTS.contains(&next));
        prop_assert!(STANDARD_SNAPSHOTS.contains(&prev));
        let oldest = STANDARD_SNAPSHOTS[STANDARD_SNAPSHOTS.len() - 1];
        if y < oldest {
            prop_assert!(next > y);
            prop_assert!(!STANDARD_SNAPSHOTS.iter().any(|&s| s > y && s < next));
        } else {
            prop_assert_eq!(next, oldest);
        }
        if y > 0.0 {
            prop_assert!(prev < y);
            prop_assert!(!STANDARD_SNAPSHOTS.iter().any(|&s| s < y && s > prev));
        } else {
            prop_assert_eq!(prev, 0.0);
        }
    }

    #[test]
    fn labels_name_the_age_to_their_precision(y in prop_oneof![
        0.0f64..20.0, 0.0f64..2.0e4, 0.0f64..2.0e6, 0.0f64..MAX_YEARS_BP,
    ]) {
        let label = format_years_bp(YearsBp(y));
        let (v, tol) = label_value(&label)
            .ok_or_else(|| TestCaseError::fail(format!("unparsed label {label:?}")))?;
        prop_assert!((v - y).abs() <= tol, "{y} -> {label:?}");
        // The unit follows the rounded value: no "10,000 years", "1000 thousand", "100.0 million".
        prop_assert!(!label.contains("10,000 years") && !label.contains("1000 thousand"));
        prop_assert!(!label.contains(".0 "));
    }
}

// ---------------------------------------------------------------------------------------------
// geo
// ---------------------------------------------------------------------------------------------

proptest! {
    #[test]
    fn latlon_round_trips(lat in -89.9f64..89.9, lon in -179.9f64..=180.0) {
        let v = latlon_to_geo(lat, lon);
        prop_assert!((v.0.length() - 1.0).abs() < 1e-12);
        let (lat2, lon2) = geo_to_latlon(v);
        prop_assert!((lat2 - lat).abs() < 1e-9);
        prop_assert!((lon2 - lon).abs() < 1e-9, "{lon} -> {lon2}");
    }

    #[test]
    fn longitude_is_canonical(d in direction()) {
        let (lat, lon) = geo_to_latlon(GeoDir(d.normalize()));
        prop_assert!((-90.0..=90.0).contains(&lat));
        prop_assert!(lon > -180.0 && lon <= 180.0);
    }

    #[test]
    fn frames_are_a_proper_rotation(a in direction(), b in direction(), q in unit_quat()) {
        let (ga, gb) = (GeoDir(a), GeoDir(b));
        prop_assert_eq!(render_to_geo(geo_to_render(ga)), ga);
        prop_assert_eq!(geo_to_render(render_to_geo(RenderDir(a))), RenderDir(a));
        // Cross products survive the change of frame (no mirroring).
        let cross = geo_to_render(GeoDir(a.cross(b))).0;
        let cross2 = geo_to_render(ga).0.cross(geo_to_render(gb).0);
        prop_assert!((cross - cross2).length() < 1e-12);
        // A geo rotation re-expressed in render axes acts the same way.
        let lhs = geo_quat_to_render(q) * geo_to_render(ga).0;
        let rhs = geo_to_render(GeoDir(q * a)).0;
        prop_assert!((lhs - rhs).length() < 1e-9);
    }
}

// ---------------------------------------------------------------------------------------------
// cube
// ---------------------------------------------------------------------------------------------

fn major_axis(f: CubeFace) -> usize {
    match f {
        CubeFace::PosX | CubeFace::NegX => 0,
        CubeFace::PosY | CubeFace::NegY => 1,
        CubeFace::PosZ | CubeFace::NegZ => 2,
    }
}

/// The largest angle between two unit vectors whose face-plane points are `d` apart. Radial
/// projection from the face plane onto the sphere never lengthens a chord.
fn chord_to_angle(d: f64) -> f64 {
    2.0 * (d / 2.0).min(1.0).asin()
}

/// `v` projected onto the face plane whose normal is `axis` (that component scaled to ±1).
fn onto_face(v: DVec3, axis: usize) -> DVec3 {
    v / v[axis].abs()
}

proptest! {
    #[test]
    fn texel_centres_round_trip(f in face(), n in 1u32..300, c in 0u32..300, r in 0u32..300) {
        let (c, r) = (c % n, r % n);
        let d = texel_center_dir(f, c, r, n);
        prop_assert!((d.0.length() - 1.0).abs() < 1e-12);
        prop_assert_eq!(dir_to_texel(d, n), (f, c, r));
    }

    /// Every direction, including exact edges and corners, lands in a texel on the face it points
    /// at, within half a texel diagonal of that texel's centre.
    #[test]
    fn every_direction_has_a_nearby_texel(d in direction(), n in 1u32..300) {
        let dir = RenderDir(d.normalize());
        let (f, c, r) = dir_to_texel(dir, n);
        prop_assert!(c < n && r < n, "({c}, {r}) on a {n} face");
        prop_assert!(d[major_axis(f)].abs() >= d.abs().max_element() - 1e-12);
        let centre = texel_center_dir(f, c, r, n).0;
        let angle = centre.dot(dir.0).clamp(-1.0, 1.0).acos();
        prop_assert!(angle <= chord_to_angle(std::f64::consts::SQRT_2 / f64::from(n)) + 1e-9);
    }

    /// Bilinear taps are a partition of unity over nearby texels, and on one face they reproduce
    /// the sample position exactly (bilinear interpolation is exact for affine functions).
    #[test]
    fn bilinear_taps_interpolate_the_position(d in direction(), n in 2u32..300) {
        let dir = RenderDir(d.normalize());
        let taps = bilinear_taps(dir, n);
        let total: f64 = taps.iter().map(|t| t.1).sum();
        prop_assert!((total - 1.0).abs() < 1e-12);
        // A tap centre is at most 1.5 texel diagonals from the sample (one off the face edge).
        let reach = chord_to_angle(3.0 * std::f64::consts::SQRT_2 / f64::from(n));
        for &((f, c, r), w) in &taps {
            prop_assert!((0.0..=1.0).contains(&w));
            prop_assert!(c < n && r < n);
            let centre = texel_center_dir(f, c, r, n).0;
            prop_assert!(centre.dot(dir.0).clamp(-1.0, 1.0).acos() <= reach + 1e-9);
        }
        let (home, _, _) = dir_to_texel(dir, n);
        if taps.iter().all(|t| t.0.0 == home) {
            let axis = major_axis(home);
            let mixed: DVec3 = taps
                .iter()
                .map(|&((f, c, r), w)| w * onto_face(texel_center_dir(f, c, r, n).0, axis))
                .sum();
            prop_assert!((mixed - onto_face(dir.0, axis)).length() < 1e-9);
        }
    }
}

// ---------------------------------------------------------------------------------------------
// formats
// ---------------------------------------------------------------------------------------------

fn frame(magic: &[u8; 8], head: &[u8], payload: &[u8]) -> Vec<u8> {
    let z = compress_to_vec(payload, CompressionLevel::Fastest);
    let mut out = magic.to_vec();
    out.extend_from_slice(head);
    out.extend_from_slice(&(z.len() as u32).to_le_bytes());
    out.extend_from_slice(&z);
    out
}

fn grid_head(kind: u8, dtype: u8, channels: u8, w: u32, h: u32) -> Vec<u8> {
    let mut head = vec![kind, dtype, channels, 0];
    head.extend(w.to_le_bytes());
    head.extend(h.to_le_bytes());
    head
}

/// Grid parameters: kind, dtype, channels, width, height, and enough random bytes for the payload.
fn grid_case() -> impl Strategy<Value = (u8, u8, u8, u32, u32, Vec<u8>)> {
    (0u8..2, 0u8..3, 1u8..5, 1u32..6, 1u32..6).prop_flat_map(|(kind, dtype, ch, w, h)| {
        let h = if kind == 0 { w } else { h };
        let faces = if kind == 0 { 6 } else { 1 };
        let size = if dtype == 2 { 4 } else { 2 };
        let len = faces * (w * h) as usize * usize::from(ch) * size;
        prop::collection::vec(any::<u8>(), len).prop_map(move |data| (kind, dtype, ch, w, h, data))
    })
}

fn plates_file(cube_size: u32, offsets: &[u32], entries: &[(u32, f64, f64)]) -> Vec<u8> {
    let mut p: Vec<u8> = offsets.iter().flat_map(|o| o.to_le_bytes()).collect();
    for (i, a, b) in entries {
        p.extend(i.to_le_bytes());
        p.extend(a.to_le_bytes());
        p.extend(b.to_le_bytes());
    }
    let mut head = cube_size.to_le_bytes().to_vec();
    head.extend((entries.len() as u32).to_le_bytes());
    frame(b"TFPLAT01", &head, &p)
}

proptest! {
    /// Every texel reads back as the bytes that were written, through both accessors.
    #[test]
    fn tfgrid_round_trips((kind, dtype, ch, w, h, data) in grid_case(), probe in any::<u64>()) {
        let g = read_tfgrid(&frame(b"TFGRID01", &grid_head(kind, dtype, ch, w, h), &data)).unwrap();
        prop_assert_eq!(g.kind, if kind == 0 { GridKind::Cube } else { GridKind::LatLon });
        prop_assert_eq!(g.dtype, [DType::F16, DType::U16, DType::F32][dtype as usize]);
        prop_assert_eq!((g.channels, g.width, g.height), (ch, w, h));
        prop_assert_eq!(&g.data, &data);
        let faces = if kind == 0 { 6 } else { 1 };
        let texels = faces * (w * h) as u64 * u64::from(ch);
        for k in 0..8u64 {
            let i = (probe.wrapping_add(k.wrapping_mul(0x9e37_79b9_7f4a_7c15))) % texels;
            let c = (i % u64::from(ch)) as u8;
            let col = ((i / u64::from(ch)) % u64::from(w)) as u32;
            let row = ((i / u64::from(ch) / u64::from(w)) % u64::from(h)) as u32;
            let f = (i / u64::from(ch) / u64::from(w) / u64::from(h)) as usize;
            let at = i as usize;
            match g.dtype {
                DType::F32 => {
                    let b = &data[at * 4..at * 4 + 4];
                    let want = f32::from_le_bytes([b[0], b[1], b[2], b[3]]);
                    prop_assert_eq!(g.texel_f32(f, row, col, c).to_bits(), want.to_bits());
                }
                DType::U16 | DType::F16 => {
                    let word = u16::from_le_bytes([data[at * 2], data[at * 2 + 1]]);
                    prop_assert_eq!(g.texel_u16(f, row, col, c), word);
                    let want = if g.dtype == DType::U16 {
                        f32::from(word)
                    } else {
                        half::f16::from_bits(word).to_f32()
                    };
                    let got = g.texel_f32(f, row, col, c);
                    prop_assert!(got.to_bits() == want.to_bits() || (got.is_nan() && want.is_nan()));
                }
            }
        }
    }

    /// Any single zero dimension, a set reserved byte, or a non-square cube is rejected.
    #[test]
    fn tfgrid_rejects_bad_headers(which in 0usize..5, w in 1u32..5, h in 1u32..5) {
        let (mut ch, mut w2, mut h2) = (1u8, w, h);
        let mut kind = 1u8;
        let mut reserved = 0u8;
        match which {
            0 => ch = 0,
            1 => w2 = 0,
            2 => h2 = 0,
            3 => reserved = 1,
            _ => { kind = 0; h2 = w + 1; }
        }
        let mut head = grid_head(kind, 1, ch, w2, h2);
        head[3] = reserved;
        let len = (w2 * h2) as usize * usize::from(ch) * 2 * if kind == 0 { 6 } else { 1 };
        let err = read_tfgrid(&frame(b"TFGRID01", &head, &vec![0; len])).unwrap_err();
        prop_assert!(matches!(err, FormatError::Invalid(_)), "{err:?}");
        prop_assert!(!err.to_string().is_empty());
    }

    /// The readers never panic, whatever the bytes: random input, or a valid file with one byte
    /// changed or the tail cut off.
    #[test]
    fn readers_never_panic(
        noise in prop::collection::vec(any::<u8>(), 0..64),
        (kind, dtype, ch, w, h, data) in grid_case(),
        flip in any::<usize>(),
        value in any::<u8>(),
        cut in any::<usize>(),
    ) {
        for magic in [&b"TFGRID01"[..], b"TFROT002", b"TFPLAT01"] {
            let mut bytes = magic.to_vec();
            bytes.extend(&noise);
            let _ = (read_tfgrid(&bytes), read_tfrot(&bytes), read_tfplates(&bytes));
        }
        let good = frame(b"TFGRID01", &grid_head(kind, dtype, ch, w, h), &data);
        let mut bad = good.clone();
        let at = flip % bad.len();
        bad[at] = value;
        let _ = read_tfgrid(&bad);
        let _ = read_tfgrid(&good[..cut % good.len()]);
    }

    /// Footprint entries with a zero-length interval or repeated keys are valid.
    #[test]
    fn tfplates_accepts_degenerate_but_valid_entries(age in 0.0f64..=540.0, plate in 0u32..4) {
        let cells = 6;
        let mut offsets = vec![0u32; cells + 1];
        offsets[cells] = 2;
        let entries = [(plate, age, age), (plate, age, age)];
        let f = read_tfplates(&plates_file(1, &offsets, &entries)).unwrap();
        prop_assert_eq!(f.entries.len(), 2);
        prop_assert_eq!(f.entries[0].younger_myr, f.entries[0].older_myr);
    }
}

// ---------------------------------------------------------------------------------------------
// geodynamics
// ---------------------------------------------------------------------------------------------

fn valid_rotations(times: Vec<f64>, plates: u32) -> RotationTable {
    let records = times.len() * plates as usize;
    RotationTable {
        plate_ids: (0..plates).map(|p| PlateId(100 + p)).collect(),
        times_myr: times,
        quats: vec![[0.0, 0.0, 0.0, 1.0]; records],
        valid: vec![true; records],
    }
}

fn valid_footprints(cube_size: u32, entries: Vec<FootprintEntry>) -> PlateFootprints {
    let cells = (6 * cube_size * cube_size) as usize;
    let mut offsets = vec![0u32; cells + 1];
    offsets[cells] = entries.len() as u32;
    PlateFootprints {
        cube_size,
        offsets,
        entries,
    }
}

proptest! {
    #[test]
    fn crust_pos_longitude_is_canonical(lat in -90.0f64..=90.0, lon in -1.0e4f64..1.0e4) {
        let p = CrustPos::new(PlateId(1), lat, lon);
        if lat.abs() >= 90.0 {
            prop_assert_eq!(p.lon_deg, 0.0);
        } else {
            prop_assert!(p.lon_deg > -180.0 && p.lon_deg <= 180.0);
            let turns = (p.lon_deg - lon) / 360.0;
            prop_assert!((turns - turns.round()).abs() < 1e-9);
        }
        prop_assert_eq!(p.lat_deg, lat);
    }

    #[test]
    fn interval_rule_is_closed(a in 0.0f64..540.0, b in 0.0f64..540.0, t in -1.0f64..541.0) {
        prop_assert_eq!(interval_accepts(a, b, t), a <= t && t <= b);
        prop_assert!(interval_accepts(a, a, a));
        prop_assert!(!interval_accepts(a, b, f64::NAN));
    }

    /// A footprint accepts exactly the plate and ages its cell lists, at that cell's centre.
    #[test]
    fn footprints_contain_what_their_cells_list(
        f in face(), n in 1u32..6, c in 0u32..6, r in 0u32..6,
        young in 0.0f64..300.0, span in 0.0f64..240.0, t in 0.0f64..540.0,
    ) {
        let (c, r) = (c % n, r % n);
        let cell = (f as usize * n as usize + r as usize) * n as usize + c as usize;
        let cells = (6 * n * n) as usize;
        let mut offsets = vec![0u32; cells + 1];
        for o in offsets.iter_mut().skip(cell + 1) {
            *o = 1;
        }
        let fp = PlateFootprints {
            cube_size: n,
            offsets,
            entries: vec![FootprintEntry {
                plate_index: PlateIndex(0),
                younger_myr: young,
                older_myr: young + span,
            }],
        };
        let dir = texel_center_dir(f, c, r, n);
        prop_assert_eq!(fp.contains(PlateIndex(0), dir, t), young <= t && t <= young + span);
        prop_assert!(!fp.contains(PlateIndex(1), dir, young));
        prop_assert!(!fp.contains(PlateIndex(0), RenderDir(DVec3::NAN), young));
        prop_assert!(CrustModel::new(valid_rotations(vec![0.0, 540.0], 1), fp).is_ok());
    }

    /// `CrustModel::new` rejects a table that breaks any one structural rule.
    #[test]
    fn crust_model_rejects_any_single_defect(which in 0usize..10, times in ascending_ages(6)) {
        let mut rot = valid_rotations(times, 2);
        // A zero-length interval is valid.
        let entry = FootprintEntry { plate_index: PlateIndex(0), younger_myr: 1.0, older_myr: 1.0 };
        let mut fp = valid_footprints(1, vec![entry]);
        prop_assert!(CrustModel::new(rot.clone(), fp.clone()).is_ok());
        // Each case breaks exactly one rule, so no other check can mask a weakened one.
        match which {
            0 => rot.times_myr[0] = rot.times_myr.get(1).copied().unwrap_or(1.0) / 2.0, // not 0
            1 => { let last = rot.times_myr.len() - 1; rot.times_myr[last] = f64::INFINITY; }
            2 => { rot.times_myr.push(rot.times_myr[rot.times_myr.len() - 1]);
                   rot.quats.extend([[0.0, 0.0, 0.0, 1.0]; 2]); rot.valid.extend([true; 2]); }
            3 => fp.offsets.push(1),                         // wrong offsets length
            4 => fp.offsets.iter_mut().for_each(|o| *o = 1), // does not start at 0
            5 => { let last = fp.offsets.len() - 1; fp.offsets[last] = 2; } // wrong end
            6 => { fp.offsets[3] = 1; fp.offsets[4] = 0; }  // decreasing
            7 => fp.entries[0].younger_myr = -1.0,           // younger out of range
            8 => fp.entries[0].older_myr = 541.0,            // older out of range
            _ => fp.entries[0].older_myr = 0.5,              // reversed
        }
        let err = CrustModel::new(rot, fp).unwrap_err();
        prop_assert!(!err.to_string().is_empty());
        let expect_rotations = which < 3;
        prop_assert_eq!(matches!(err, CrustModelError::Rotations(_)), expect_rotations, "{:?}", err);
    }

    /// Rotations: exact at samples, a unit-length slerp between valid neighbours, `None` outside.
    #[test]
    fn rotation_lookup(
        times in ascending_ages(6), a in unit_quat(), b in unit_quat(), s in 0.0f64..1.0,
    ) {
        let qa = a.as_quat().normalize();
        let qb = b.as_quat().normalize();
        let k = times.len();
        let mut quats = Vec::new();
        for t in 0..k {
            quats.push(if t % 2 == 0 { qa } else { qb }.to_array());
        }
        let table = RotationTable {
            plate_ids: vec![PlateId(7)],
            times_myr: times.clone(),
            quats,
            valid: vec![true; k],
        };
        for (t, &age) in times.iter().enumerate() {
            prop_assert_eq!(table.rotation_at(PlateIndex(0), age).unwrap().to_array(), table.quats[t]);
        }
        prop_assert_eq!(table.rotation_at(PlateIndex(1), 0.0), None);
        prop_assert_eq!(table.rotation_at(PlateIndex(0), -0.1), None);
        prop_assert_eq!(table.rotation_at(PlateIndex(0), times[k - 1] + 0.1), None);
        prop_assert_eq!(table.rotation_at(PlateIndex(0), f64::NAN), None);
        if k >= 2 {
            let age = times[0] + s * (times[1] - times[0]);
            let q = table.rotation_at(PlateIndex(0), age).unwrap();
            prop_assert!((q.length() - 1.0).abs() < 1e-4);
            // The slerp lies on the arc: its angles to both ends add up to the arc.
            let (to_a, to_b, arc) = (q.angle_between(qa), q.angle_between(qb), qa.angle_between(qb));
            prop_assert!((to_a + to_b - arc).abs() < 2e-3, "{to_a} + {to_b} vs {arc}");
        }
    }
}

// ---------------------------------------------------------------------------------------------
// globe
// ---------------------------------------------------------------------------------------------

proptest! {
    #[test]
    fn endpoint_weights_are_a_partition(available in any::<[bool; 2]>(), w in 0.0f64..=1.0) {
        match endpoint_weights(available, w) {
            None => prop_assert_eq!(available, [false, false]),
            Some(ws) => {
                prop_assert!((ws[0] + ws[1] - 1.0).abs() < 1e-12);
                prop_assert!(available[0] || ws[0] == 0.0);
                prop_assert!(available[1] || ws[1] == 0.0);
            }
        }
    }

    /// A zero-weight endpoint never leaks into the blend, even as NaN.
    #[test]
    fn blend_ignores_zero_weight_endpoints(e in -1.0e4f64..1.0e4, a in -200.0f64..200.0) {
        prop_assert_eq!(blend_endpoints_m([1.0, 0.0], [e, f64::NAN], [a, f64::NAN]), e - a);
        prop_assert_eq!(blend_endpoints_m([0.0, 1.0], [f64::NAN, e], [f64::NAN, a]), e - a);
    }

    #[test]
    fn land_and_displacement(e in -1.0e4f64..1.0e4, f in -1.0e4f64..1.0e4) {
        prop_assert_eq!(visible_land_m(e).is_some(), e > 0.0);
        prop_assert!(displacement_km(e) >= 0.0);
        if e <= f {
            prop_assert!(displacement_km(e) <= displacement_km(f));
        }
        prop_assert!(visible_land_m(f64::NAN).is_none());
    }

    #[test]
    fn relative_rotation_maps_current_to_source(s in unit_quat(), c in unit_quat(), d in direction()) {
        let (s, c) = (s.as_quat(), c.as_quat());
        prop_assert_eq!(relative_rotation(s, s), Quat::IDENTITY);
        let v = d.normalize().as_vec3();
        let via = relative_rotation(s, c) * (c * v);
        prop_assert!((via - s * v).length() < 1e-4);
    }

    #[test]
    fn source_id_ranks_follow_id_order(ids in prop::collection::hash_set(0u32..10_000, 0..20)) {
        let ids: Vec<u32> = ids.into_iter().collect();
        let table = valid_rotations(vec![0.0], 0);
        let table = RotationTable { plate_ids: ids.iter().map(|&i| PlateId(i)).collect(), ..table };
        let ranks = source_id_ranks(&table);
        let mut sorted = ranks.clone();
        sorted.sort_unstable();
        prop_assert_eq!(sorted, (0..ids.len() as u32).collect::<Vec<_>>());
        for i in 0..ids.len() {
            for j in 0..ids.len() {
                prop_assert_eq!(ids[i] < ids[j], ranks[i] < ranks[j]);
            }
        }
    }
}

#[test]
fn only_the_ocean_is_not_land() {
    assert!(!RenderedSurface::Ocean.is_land());
    assert!(RenderedSurface::GapLand { elevation_m: 1.0 }.is_land());
    assert!(
        RenderedSurface::Land {
            plate_id: PlateId(1),
            elevation_m: 1.0
        }
        .is_land()
    );
}

// ---------------------------------------------------------------------------------------------
// surface (timeline)
// ---------------------------------------------------------------------------------------------

/// A valid timeline: `recent` ice6g frames, the modern bridge, then `deep` older PaleoDEMs.
fn timeline_case() -> impl Strategy<Value = (Vec<FrameMeta>, Vec<(f64, f64)>)> {
    (
        1usize..4,
        0usize..4,
        prop::collection::vec((0.001f64..0.01, -130.0f64..10.0), 1..5),
    )
        .prop_map(|(recent, deep, knots)| {
            let mut frames = Vec::new();
            for i in 0..recent {
                frames.push(FrameMeta {
                    age_myr: 0.002 * i as f64,
                    source: FrameSource::Ice6g,
                    image_slot: i as u32,
                });
            }
            let bridge = 0.002 * recent as f64;
            frames.push(FrameMeta {
                age_myr: bridge,
                source: FrameSource::PaleoDem {
                    source_rotation_age_myr: 0.0,
                },
                image_slot: recent as u32,
            });
            for d in 0..deep {
                let age = 1.0 + 10.0 * d as f64;
                frames.push(FrameMeta {
                    age_myr: age,
                    source: FrameSource::PaleoDem {
                        source_rotation_age_myr: age,
                    },
                    image_slot: (recent + 1 + d) as u32,
                });
            }
            let mut t = 0.0005;
            let knots = knots
                .into_iter()
                .map(|(gap, v)| {
                    let k = (t, v);
                    t += gap;
                    k
                })
                .collect();
            (frames, knots)
        })
}

proptest! {
    #[test]
    fn bracket_interpolates_the_age(ages in ascending_ages(8), x in -1.0f64..60.0) {
        let b = bracket(&ages, x);
        prop_assert!(b.younger <= b.older && b.older < ages.len());
        prop_assert!((0.0..1.0).contains(&b.older_weight));
        let (ya, oa) = (ages[b.younger], ages[b.older]);
        let at = ya + b.older_weight * (oa - ya);
        let clamped = x.clamp(ages[0], ages[ages.len() - 1]);
        prop_assert!((at - clamped).abs() < 1e-9, "{x}: {b:?}");
        prop_assert_eq!(bracket(&ages, f64::NAN).younger, 0);
    }

    #[test]
    fn timeline_accessors_and_sea_level((frames, knots) in timeline_case(), probe in 0.0f64..1.0) {
        let recent = frames.iter().filter(|f| f.source == FrameSource::Ice6g).count();
        let t = Timeline::new(frames.clone(), knots.clone()).unwrap();
        let ages: Vec<f64> = frames.iter().map(|f| f.age_myr).collect();
        prop_assert_eq!(t.ages_myr(), ages.as_slice());
        prop_assert_eq!(t.last_recent_index(), recent - 1);
        prop_assert_eq!(t.bridge_frame_index(), recent);
        // Held at the youngest knot before the curve and for NaN.
        prop_assert_eq!(t.sea_level_anomaly_m(0.0), knots[0].1);
        prop_assert_eq!(t.sea_level_anomaly_m(f64::NAN), knots[0].1);
        // Exact at every knot, and between neighbouring knots within their range.
        for &(age, v) in &knots {
            prop_assert_eq!(t.sea_level_anomaly_m(age), v);
        }
        for w in knots.windows(2) {
            let age = w[0].0 + probe * (w[1].0 - w[0].0);
            let got = t.sea_level_anomaly_m(age);
            prop_assert!(got >= w[0].1.min(w[1].1) - 1e-9 && got <= w[0].1.max(w[1].1) + 1e-9);
        }
    }

    #[test]
    fn timeline_rejects_a_bad_sea_level_curve((frames, mut knots) in timeline_case(), which in 0usize..3) {
        match which {
            0 => knots.clear(),
            1 => knots[0].1 = f64::NAN,
            _ => { let k = knots[0]; knots.push(k); }
        }
        let err = Timeline::new(frames, knots).unwrap_err();
        let curve_error =
            matches!(err, TimelineError::EmptySeaLevelCurve | TimelineError::BadSeaLevelKnot { .. });
        prop_assert!(curve_error, "{:?}", err);
        prop_assert!(!err.to_string().is_empty());
    }
}
