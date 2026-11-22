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
/// Accepts `--diagnostics-probe=<mode>` and `--diagnostics-probe <mode>`; a following argument
/// that starts with `--` is the next flag, not a value. Anything else, including a probe flag
/// with an unknown or missing mode, lands in [`LaunchArgs::unknown`].
pub fn parse_launch_args<I: IntoIterator<Item = String>>(args: I) -> LaunchArgs {
    let mut parsed = LaunchArgs::default();
    let mut args = args.into_iter().peekable();
    while let Some(arg) = args.next() {
        let value = if let Some(value) = arg
            .strip_prefix(PROBE_FLAG)
            .and_then(|v| v.strip_prefix('='))
        {
            Some(value.to_string())
        } else if arg == PROBE_FLAG {
            args.next_if(|next| !next.starts_with("--"))
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
/// leading `?`). `key=value` becomes `--key=value` and a bare `key` becomes `--key`; keys and
/// values are percent-decoded (`+` is a space).
pub fn parse_query_args(query: &str) -> LaunchArgs {
    let query = query.strip_prefix('?').unwrap_or(query);
    parse_launch_args(
        query
            .split('&')
            .filter(|pair| !pair.is_empty())
            .map(|pair| match pair.split_once('=') {
                Some((key, value)) => {
                    format!("--{}={}", percent_decode(key), percent_decode(value))
                }
                None => format!("--{}", percent_decode(pair)),
            }),
    )
}

/// Decodes `%XX` escapes and `+` (form encoding for a space). Malformed escapes are kept as
/// written; invalid UTF-8 is replaced.
pub fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let hex = |b: u8| char::from(b).to_digit(16).map(|d| d as u8);
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if let (Some(&h), Some(&l)) = (bytes.get(i + 1), bytes.get(i + 2))
                && let (Some(h), Some(l)) = (hex(h), hex(l)) =>
            {
                out.push(h * 16 + l);
                i += 3;
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
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
            ("error-exit", ProbeMode::ErrorExit),
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
    fn probe_flag_does_not_swallow_the_next_flag() {
        let parsed = parse_launch_args(args(&["--diagnostics-probe", "--target-fps=30"]));
        assert_eq!(parsed.diagnostics_probe, None);
        assert_eq!(
            parsed.unknown,
            args(&["--diagnostics-probe", "--target-fps=30"])
        );
        let parsed = parse_launch_args(args(&["--diagnostics-probe", "clean", "--window"]));
        assert_eq!(parsed.diagnostics_probe, Some(ProbeMode::Clean));
        assert_eq!(parsed.unknown, args(&["--window"]));
    }

    #[test]
    fn web_query_values_are_percent_decoded() {
        assert_eq!(
            parse_query_args("?diagnostics-probe=panic%2Dafter%2Dframe").diagnostics_probe,
            Some(ProbeMode::PanicAfterFrame)
        );
        assert_eq!(
            parse_query_args("note=a+b%20c").unknown,
            args(&["--note=a b c"])
        );
    }

    #[test]
    fn percent_decoding() {
        assert_eq!(percent_decode("a%20b+c%2Fd%C3%A9"), "a b c/dé");
        // Malformed escapes stay as written.
        assert_eq!(percent_decode("100%"), "100%");
        assert_eq!(percent_decode("%zz%4"), "%zz%4");
    }

    #[test]
    fn web_query_unknown_keys() {
        let parsed = parse_query_args("utm_source=x&diagnostics-probe=clean&debug");
        assert_eq!(parsed.diagnostics_probe, Some(ProbeMode::Clean));
        assert_eq!(parsed.unknown, args(&["--utm_source=x", "--debug"]));
    }
}
