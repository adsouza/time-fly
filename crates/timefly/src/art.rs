//! Shared constants for the art pass: shader animation clocks must loop seamlessly.

/// The period (seconds) at which Bevy wraps `Time<Real>`, and so `globals.time` in shaders.
/// Every looping shader animation's period must divide it exactly, or it jumps at the wrap.
pub const TIME_WRAP_S: f32 = 3600.0;

/// Panics unless `period_s` divides [`TIME_WRAP_S`] exactly, so an animation with that period
/// is continuous across the clock's wrap.
pub fn assert_divides_wrap(period_s: f32) {
    assert!(
        (TIME_WRAP_S / period_s).fract() == 0.0,
        "animation period {period_s} s does not divide the {TIME_WRAP_S} s time wrap"
    );
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use bevy::prelude::*;

    use super::*;

    #[test]
    fn time_wrap_matches_bevy() {
        assert_eq!(
            Time::<Real>::default().wrap_period(),
            Duration::from_secs_f32(TIME_WRAP_S)
        );
    }

    #[test]
    fn divides_wrap_accepts_divisors_and_rejects_others() {
        assert_divides_wrap(60.0);
        assert_divides_wrap(3600.0);
        assert!(std::panic::catch_unwind(|| assert_divides_wrap(7.0)).is_err());
    }
}
