//! Renders the globe headless to a PNG through the real shaders, for visual review.
//!
//! `cargo run -p timefly --example globe_shot -- <asset_root> <age_years> <lat> <lon> <out.png> [size]`

use std::path::PathBuf;

use timefly::globe::EARTH_RADIUS_KM;
use timefly::globe::capture::{CaptureRequest, capture_globe};

fn main() -> Result<(), String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() < 5 {
        return Err(
            "usage: globe_shot <asset_root> <age_years> <lat> <lon> <out.png> [size]".into(),
        );
    }
    let number = |i: usize| {
        args[i]
            .parse::<f64>()
            .map_err(|e| format!("{}: {e}", args[i]))
    };
    let size = args
        .get(5)
        .map_or(Ok(640), |s| s.parse::<u32>().map_err(|e| e.to_string()))?;
    let shot = capture_globe(&CaptureRequest {
        asset_root: PathBuf::from(&args[0]),
        age_years: number(1)?,
        size: (size, size),
        look_at_lat_lon: (number(2)?, number(3)?),
        distance_km: 2.6 * EARTH_RADIUS_KM,
    })?;
    shot.save_png(std::path::Path::new(&args[4]))?;
    println!("wrote {}", args[4]);
    Ok(())
}
