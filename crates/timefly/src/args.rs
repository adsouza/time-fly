//! Launch arguments: one parser for native argv and the web URL query string.
//!
//! Task 10 adds `--scrub-bench`, `--target-fps` and `--window` here.

use crate::diagnostics::ProbeMode;

/// What the game was asked to do at launch.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LaunchArgs {
    /// Hidden `--diagnostics-probe=<mode>` (web: `?diagnostics-probe=<mode>`).
    pub diagnostics_probe: Option<ProbeMode>,
    /// Arguments this build does not understand. They are ignored, and logged as warnings once
    /// the log subscriber exists.
    pub unknown: Vec<String>,
}

const PROBE_FLAG: &str = "--diagnostics-probe";

/// Parses native arguments, without the program name (`std::env::args().skip(1)`).
///
/// Accepts `--diagnostics-probe=<mode>` and `--diagnostics-probe <mode>`. Anything else, including
/// a probe flag with an unknown mode, lands in [`LaunchArgs::unknown`].
pub fn parse_launch_args<I: IntoIterator<Item = String>>(args: I) -> LaunchArgs {
    let mut parsed = LaunchArgs::default();
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        let value = if let Some(value) = arg
            .strip_prefix(PROBE_FLAG)
            .and_then(|v| v.strip_prefix('='))
        {
            Some(value.to_string())
        } else if arg == PROBE_FLAG {
            args.next()
        } else {
            parsed.unknown.push(arg);
            continue;
        };
        match value.as_deref().and_then(ProbeMode::parse) {
            Some(mode) => parsed.diagnostics_probe = Some(mode),
            None => parsed.unknown.push(match value {
                Some(value) => format!("{PROBE_FLAG}={value}"),
                None => arg,
            }),
        }
    }
    parsed
}

/// Builds [`LaunchArgs`] from a web URL query string (`location.search`, with or without the
/// leading `?`). `key=value` becomes `--key=value` and a bare `key` becomes `--key`.
pub fn parse_query_args(query: &str) -> LaunchArgs {
    let query = query.strip_prefix('?').unwrap_or(query);
    parse_launch_args(
        query
            .split('&')
            .filter(|pair| !pair.is_empty())
            .map(|pair| format!("--{pair}")),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn no_args_is_default() {
        assert_eq!(parse_launch_args(args(&[])), LaunchArgs::default());
    }

    #[test]
    fn probe_with_equals() {
        let parsed = parse_launch_args(args(&["--diagnostics-probe=panic-after-frame"]));
        assert_eq!(parsed.diagnostics_probe, Some(ProbeMode::PanicAfterFrame));
        assert!(parsed.unknown.is_empty());
    }

    #[test]
    fn probe_with_separate_value() {
        let parsed = parse_launch_args(args(&["--diagnostics-probe", "abort"]));
        assert_eq!(parsed.diagnostics_probe, Some(ProbeMode::Abort));
    }

    #[test]
    fn every_probe_mode_parses() {
        for (text, mode) in [
            ("panic", ProbeMode::Panic),
            ("panic-after-frame", ProbeMode::PanicAfterFrame),
            ("abort", ProbeMode::Abort),
            ("error", ProbeMode::Error),
            ("clean", ProbeMode::Clean),
        ] {
            let parsed = parse_launch_args(args(&[&format!("--diagnostics-probe={text}")]));
            assert_eq!(parsed.diagnostics_probe, Some(mode), "{text}");
        }
    }

    #[test]
    fn unknown_args_are_kept_for_a_warning() {
        let parsed = parse_launch_args(args(&[
            "--frobnicate",
            "-psn_0_123",
            "--diagnostics-probe=boom",
        ]));
        assert_eq!(parsed.diagnostics_probe, None);
        assert_eq!(
            parsed.unknown,
            args(&["--frobnicate", "-psn_0_123", "--diagnostics-probe=boom"])
        );
    }

    #[test]
    fn probe_flag_without_value_is_unknown() {
        let parsed = parse_launch_args(args(&["--diagnostics-probe"]));
        assert_eq!(parsed.diagnostics_probe, None);
        assert_eq!(parsed.unknown, args(&["--diagnostics-probe"]));
    }

    #[test]
    fn web_query_matches_native() {
        assert_eq!(
            parse_query_args("?diagnostics-probe=panic"),
            parse_launch_args(args(&["--diagnostics-probe=panic"]))
        );
        assert_eq!(parse_query_args(""), LaunchArgs::default());
        assert_eq!(parse_query_args("?"), LaunchArgs::default());
    }

    #[test]
    fn web_query_unknown_keys() {
        let parsed = parse_query_args("utm_source=x&diagnostics-probe=clean&debug");
        assert_eq!(parsed.diagnostics_probe, Some(ProbeMode::Clean));
        assert_eq!(parsed.unknown, args(&["--utm_source=x", "--debug"]));
    }
}
