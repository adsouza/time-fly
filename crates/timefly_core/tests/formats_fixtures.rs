//! Fixtures written by the Python bake tool (`tools/bake/scripts/write_fixtures.py`) and read by
//! the Rust readers: they prove the two sides agree on the byte layouts.

use timefly_core::formats::{FormatError, read_tfgrid, read_tfplates, read_tfrot};
use timefly_core::geodynamics::PlateId;
use timefly_core::geodynamics::PlateIndex;

#[test]
fn reads_python_cube_fixture() {
    let g = read_tfgrid(include_bytes!("fixtures/cube2_rg16f.tfgrid")).unwrap();
    assert_eq!((g.width, g.height, g.channels), (2, 2, 2));
    assert_eq!(g.texel_f32(4, 1, 0, 1), 410.5); // face 4, row 1, col 0, channel 1
}

#[test]
fn reads_python_rot_fixture() {
    let t = read_tfrot(include_bytes!("fixtures/rot_small.tfrot")).unwrap();
    assert_eq!(t.plate_ids, vec![PlateId(101), PlateId(202), PlateId(303)]);
    assert_eq!(t.times_myr, vec![0.0, 1.0]);
    assert!((t.quats[3 + 2][1] - 0.7071068).abs() < 1e-6);
    assert!(!t.valid[3 + 1]);
    assert!(t.valid[0] && t.valid[3 + 2]);
}

#[test]
fn reads_python_footprints_fixture() {
    let f = read_tfplates(include_bytes!("fixtures/footprints_small.tfplates")).unwrap();
    assert_eq!(f.cube_size, 2);
    assert_eq!(f.offsets.len(), 25);
    assert_eq!((f.offsets[0], f.offsets[1]), (0, 0)); // cell 0 is empty
    assert_eq!(f.entries.len(), 3);
    assert_eq!(f.entries[0].plate_index, PlateIndex(1));
    // Cell 7 has two memberships.
    let cell7 = &f.entries[f.offsets[7] as usize..f.offsets[8] as usize];
    assert_eq!(cell7.len(), 2);
    assert_eq!(cell7[0].plate_index, PlateIndex(0));
    assert_eq!((cell7[0].younger_myr, cell7[0].older_myr), (100.0, 200.0));
    assert_eq!(cell7[1].plate_index, PlateIndex(2));
    assert_eq!((cell7[1].younger_myr, cell7[1].older_myr), (0.0, 540.0));
}

#[test]
fn rejects_bad_magic_and_truncation() {
    let good = include_bytes!("fixtures/cube2_rg16f.tfgrid");
    let mut bad = good.to_vec();
    bad[0] = b'X';
    assert!(matches!(read_tfgrid(&bad), Err(FormatError::BadMagic)));
    assert!(matches!(
        read_tfgrid(&good[..good.len() - 3]),
        Err(FormatError::Truncated)
    ));
    assert!(read_tfgrid(&good[..10]).is_err());
}
