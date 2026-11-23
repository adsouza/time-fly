//! The feature list a build reports (`BuildInfo`), shared with `build.rs` (which includes this
//! file by path), so it is honest per target: `system-spans` takes effect only natively (ruling
//! R29), so a wasm build never claims it.

/// The cargo features that take effect for `target_arch`, sorted, without `default`.
pub fn effective_features(mut features: Vec<String>, target_arch: &str) -> Vec<String> {
    features.retain(|f| f != "default" && !(target_arch == "wasm32" && f == "system-spans"));
    features.sort();
    features
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spans_are_claimed_only_where_they_run() {
        let given = || vec!["system-spans".to_string(), "default".to_string()];
        assert_eq!(effective_features(given(), "aarch64"), ["system-spans"]);
        assert_eq!(effective_features(given(), "x86_64"), ["system-spans"]);
        assert!(effective_features(given(), "wasm32").is_empty());
        let other = vec!["b".to_string(), "a".to_string()];
        assert_eq!(effective_features(other, "wasm32"), ["a", "b"]);
    }
}
