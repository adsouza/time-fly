// The permanent snowline, mirrored one-to-one by timefly_core::worldgen::biome::snowline_with_raise_m
// (and the surface byte's raise by timefly_core::worldgen::generate::decode_snowline_raise).
// Plain WGSL with no imports, so terrain.wesl imports it as `package::snowline` and the GPU parity
// test (tests/region_gpu.rs) compiles it on its own.
//
// 5,500 m up to 20 degrees of latitude, then 70 m lower per degree, raised by dryness (ruling
// R27: up to 1,000 m, the larger of the dry Koppen classes' raise and continentality, decided
// per texel by the generator and stored in the surface code's fourth byte). Glacial eras (ruling
// R18) lower it by 1,000 m per -120 m of sea-level anomaly, clamped to 0..1,200 m, because
// ICE-6G's 1-degree grid misses mountain glaciers. Never below sea level.

const MAX_GLACIAL_SNOWLINE_DROP_M: f32 = 1200.0;
const SNOWLINE_DROP_PER_ANOMALY_M: f32 = 1000.0 / 120.0;
const MAX_SNOWLINE_RAISE_M: f32 = 1000.0;
const SURFACE_RAISE_STEPS: u32 = 127u;

// The dryness raise (m) a surface code's fourth byte holds (its low 7 bits).
fn snowline_raise_from_byte(byte: u32) -> f32 {
    return f32(byte & SURFACE_RAISE_STEPS) / f32(SURFACE_RAISE_STEPS) * MAX_SNOWLINE_RAISE_M;
}

fn snowline_m(lat_deg: f32, sea_level_anomaly_m: f32, raise_m: f32) -> f32 {
    let drop = clamp(-sea_level_anomaly_m * SNOWLINE_DROP_PER_ANOMALY_M, 0.0, MAX_GLACIAL_SNOWLINE_DROP_M);
    return max(5500.0 - 70.0 * max(abs(lat_deg) - 20.0, 0.0) + max(raise_m, 0.0) - drop, 0.0);
}
