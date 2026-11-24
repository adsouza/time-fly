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

/// `name: type` fields of a struct declared in the WESL source `src`, in order, for tests that
/// check a Rust uniform against its WESL mirror.
#[cfg(test)]
pub(crate) fn wesl_struct_fields(src: &str, name: &str) -> Vec<(String, String)> {
    let start = src
        .find(&format!("struct {name} {{"))
        .unwrap_or_else(|| panic!("struct {name} in the WESL source"));
    let body = &src[start..];
    let body = &body[body.find('{').unwrap() + 1..body.find('}').unwrap()];
    body.lines()
        .map(|l| l.split("//").next().unwrap().trim())
        .filter(|l| !l.is_empty())
        .map(|l| {
            let (n, t) = l.trim_end_matches(',').split_once(':').unwrap();
            (n.trim().to_string(), t.trim().to_string())
        })
        .collect()
}

/// The value of `const NAME: f32 = value;` in the WESL source `src`, for tests that check a WGSL
/// constant against the Rust one it mirrors.
#[cfg(test)]
pub(crate) fn wesl_const_f32(src: &str, name: &str) -> f32 {
    let decl = format!("const {name}: f32 = ");
    let start = src
        .find(&decl)
        .unwrap_or_else(|| panic!("const {name} in the WESL source"))
        + decl.len();
    let end = start + src[start..].find(';').expect("the constant's semicolon");
    src[start..end]
        .trim()
        .parse()
        .unwrap_or_else(|e| panic!("const {name}: {e}"))
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
