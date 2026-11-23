// The permanent snowline, mirrored one-to-one by timefly_core::worldgen::biome::snowline_m.
// Plain WGSL with no imports, so terrain.wesl imports it as `package::snowline` and the GPU parity
// test (tests/region_gpu.rs) compiles it on its own.
//
// 5,500 m up to 20 degrees of latitude, then 70 m lower per degree. Glacial eras (ruling R18)
// lower it by 1,000 m per -120 m of sea-level anomaly, clamped to 0..1,200 m, because ICE-6G's
// 1-degree grid misses mountain glaciers. Never below sea level.

const MAX_GLACIAL_SNOWLINE_DROP_M: f32 = 1200.0;
const SNOWLINE_DROP_PER_ANOMALY_M: f32 = 1000.0 / 120.0;

fn snowline_m(lat_deg: f32, sea_level_anomaly_m: f32) -> f32 {
    let drop = clamp(-sea_level_anomaly_m * SNOWLINE_DROP_PER_ANOMALY_M, 0.0, MAX_GLACIAL_SNOWLINE_DROP_M);
    return max(5500.0 - 70.0 * max(abs(lat_deg) - 20.0, 0.0) - drop, 0.0);
}
