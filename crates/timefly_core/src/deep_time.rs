//! Deep-time model: years before present, the log-spaced time slider, snapshot stepping and labels.

/// Years before present (the game's "now" is 2026 CE).
#[derive(Copy, Clone, Debug, PartialEq, PartialOrd)]
pub struct YearsBp(pub f64);

impl YearsBp {
    pub fn from_myr(m: f64) -> YearsBp {
        YearsBp(m * 1.0e6)
    }

    pub fn myr(self) -> f64 {
        self.0 / 1.0e6
    }

    /// Converts a conventional BP age (counted from 1950 CE) to years before the game's present.
    pub fn from_1950_bp(years: f64) -> YearsBp {
        YearsBp(years + (PRESENT_YEAR_CE - BP_REFERENCE_YEAR_CE) as f64)
    }
}

pub const MAX_YEARS_BP: f64 = 540.0e6;
pub const SLIDER_OFFSET_YEARS: f64 = 50.0;
pub const PRESENT_YEAR_CE: i32 = 2026;
pub const BP_REFERENCE_YEAR_CE: i32 = 1950;

/// Ratio spanned by the log slider: `(YM + Y0) / Y0`.
fn slider_span() -> f64 {
    (MAX_YEARS_BP + SLIDER_OFFSET_YEARS) / SLIDER_OFFSET_YEARS
}

/// Maps a slider position in [0, 1] to an age; out-of-range input is clamped.
pub fn slider_to_years(s: f64) -> YearsBp {
    let s = s.clamp(0.0, 1.0);
    YearsBp(SLIDER_OFFSET_YEARS * slider_span().powf(s) - SLIDER_OFFSET_YEARS)
}

/// Inverse of [`slider_to_years`]; ages outside [0, 540 Ma] are clamped.
pub fn years_to_slider(y: YearsBp) -> f64 {
    let y = y.0.clamp(0.0, MAX_YEARS_BP);
    ((y + SLIDER_OFFSET_YEARS) / SLIDER_OFFSET_YEARS).ln() / slider_span().ln()
}

/// Ages (years BP, ascending) at which baked snapshots exist.
pub const STANDARD_SNAPSHOTS: [f64; 24] = [
    0.0, 100.0, 500.0, 1e3, 2e3, 5e3, 1e4, 2.1e4, 5e4, 1e5, 1e6, 5e6, 1e7, 2e7, 3.5e7, 5e7, 6.6e7,
    1e8, 1.5e8, 2e8, 2.5e8, 3e8, 4e8, 5.4e8,
];

/// The next snapshot strictly older than `y`; stays at the oldest snapshot.
pub fn next_snapshot(y: YearsBp) -> YearsBp {
    let oldest = STANDARD_SNAPSHOTS[STANDARD_SNAPSHOTS.len() - 1];
    YearsBp(
        STANDARD_SNAPSHOTS
            .iter()
            .copied()
            .find(|&s| s > y.0)
            .unwrap_or(oldest),
    )
}

/// The next snapshot strictly younger than `y`; stays at the present.
pub fn prev_snapshot(y: YearsBp) -> YearsBp {
    YearsBp(
        STANDARD_SNAPSHOTS
            .iter()
            .copied()
            .rev()
            .find(|&s| s < y.0)
            .unwrap_or(0.0),
    )
}

/// Human-readable age label for the time slider.
///
/// The unit is chosen from the rounded value, so an age that rounds up to the next unit's
/// threshold is promoted (999,500 reads "1 million years ago", not "1000 thousand").
pub fn format_years_bp(y: YearsBp) -> String {
    let v = y.0;
    if v < 0.5 {
        return "Present".to_string();
    }
    let years = v.round() as u64;
    if years == 1 {
        return "1 year ago".to_string();
    }
    if years < 10_000 {
        return format!("{} years ago", group_thousands(years));
    }
    let thousands = (v / 1.0e3).round() as u64;
    if thousands < 1_000 {
        return format!("{thousands} thousand years ago");
    }
    // Tenths of a million, rounded.
    let tenths = (v / 1.0e5).round() as u64;
    if tenths < 100 {
        let (whole, frac) = (tenths / 10, tenths % 10);
        if frac == 0 {
            format!("{whole} million years ago")
        } else {
            format!("{whole}.{frac} million years ago")
        }
    } else {
        // Round the age itself, not the tenths: rounding twice turns 42.4985 Ma into
        // 425 tenths and then 43 million.
        format!("{} million years ago", (v / 1.0e6).round() as u64)
    }
}

fn group_thousands(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    fn close(a: f64, b: f64, rel: f64) -> bool {
        (a - b).abs() <= rel * b.abs().max(1.0)
    }

    #[test]
    fn slider_endpoints() {
        assert_eq!(slider_to_years(0.0).0, 0.0);
        assert!(close(slider_to_years(1.0).0, 540.0e6, 1e-9));
    }
    #[test]
    fn slider_midpoints_are_log_spaced() {
        assert!(close(slider_to_years(0.25).0, 2816.328442963292, 1e-9));
        assert!(close(slider_to_years(0.5).0, 164266.77485880742, 1e-9));
        assert!(close(slider_to_years(0.75).0, 9419666.908675905, 1e-9));
    }
    #[test]
    fn slider_round_trips_and_known_ages() {
        for s in [0.0, 0.1, 0.37, 0.8, 1.0] {
            assert!((years_to_slider(slider_to_years(s)) - s).abs() < 1e-9);
        }
        assert!((years_to_slider(YearsBp(66.0e6)) - 0.8702126358373475).abs() < 1e-9);
        assert!((years_to_slider(YearsBp(21_000.0)) - 0.3731158781360922).abs() < 1e-9);
    }
    #[test]
    fn slider_clamps() {
        assert_eq!(slider_to_years(-0.5).0, 0.0);
        assert!(close(slider_to_years(1.5).0, 540.0e6, 1e-9));
        assert_eq!(years_to_slider(YearsBp(-10.0)), 0.0);
        assert_eq!(years_to_slider(YearsBp(1.0e12)), 1.0);
    }
    #[test]
    fn bp_reference_year() {
        assert_eq!(YearsBp::from_1950_bp(0.0).0, 76.0);
        assert_eq!(YearsBp::from_1950_bp(21_000.0).0, 21_076.0);
    }
    #[test]
    fn snapshot_stepping() {
        assert_eq!(next_snapshot(YearsBp(0.0)).0, 100.0);
        assert_eq!(next_snapshot(YearsBp(150.0)).0, 500.0);
        assert_eq!(prev_snapshot(YearsBp(150.0)).0, 100.0);
        assert_eq!(prev_snapshot(YearsBp(0.0)).0, 0.0);
        assert_eq!(next_snapshot(YearsBp(540.0e6)).0, 540.0e6);
    }
    #[test]
    fn labels() {
        assert_eq!(format_years_bp(YearsBp(0.0)), "Present");
        assert_eq!(format_years_bp(YearsBp(100.0)), "100 years ago");
        assert_eq!(format_years_bp(YearsBp(9_999.0)), "9,999 years ago");
        assert_eq!(format_years_bp(YearsBp(21_000.0)), "21 thousand years ago");
        assert_eq!(
            format_years_bp(YearsBp(1_500_000.0)),
            "1.5 million years ago"
        );
        assert_eq!(format_years_bp(YearsBp(5_000_000.0)), "5 million years ago");
        assert_eq!(format_years_bp(YearsBp(66.0e6)), "66 million years ago");
        assert_eq!(format_years_bp(YearsBp(540.0e6)), "540 million years ago");
    }
    #[test]
    fn label_unit_boundaries() {
        let l = |v: f64| format_years_bp(YearsBp(v));
        assert_eq!(l(0.49), "Present");
        assert_eq!(l(0.5), "1 year ago");
        assert_eq!(l(1.4), "1 year ago");
        assert_eq!(l(9_999.4), "9,999 years ago");
        assert_eq!(l(9_999.5), "10 thousand years ago");
        assert_eq!(l(999_499.0), "999 thousand years ago");
        assert_eq!(l(999_500.0), "1 million years ago");
        assert_eq!(l(9_949_999.0), "9.9 million years ago");
        assert_eq!(l(9_950_000.0), "10 million years ago");
        assert_eq!(l(10_000_000.0), "10 million years ago");
        // Rounded once, not via tenths (425 tenths would round on to 43).
        assert_eq!(l(42_498_528.9), "42 million years ago");
    }
    #[test]
    fn snapshots_ascending_and_prev_from_oldest() {
        assert!(STANDARD_SNAPSHOTS.windows(2).all(|w| w[0] < w[1]));
        assert_eq!(prev_snapshot(YearsBp(540.0e6)).0, 400.0e6);
    }
}
