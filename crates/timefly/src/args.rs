//! Launch arguments: one parser for native argv and the web URL query string.
//!
//! Flags: `--diagnostics-probe <mode>` (hidden), `--check-data[=DIR]` (hidden), `--scrub-bench`,
//! `--target-fps N` and `--window WxH`. A value may follow its flag as `--flag=value` or as the
//! next argument; a next argument that starts with `--` is the next flag, never a value.
//! `--check-data` takes its optional value only as `--check-data=DIR`.

use crate::diagnostics::ProbeMode;

/// What the game was asked to do at launch.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LaunchArgs {
    /// Hidden `--diagnostics-probe=<mode>` (web: `?diagnostics-probe=<mode>`).
    pub diagnostics_probe: Option<ProbeMode>,
    /// `--scrub-bench` (web: `?scrub-bench`): run the automated scrub benchmark and report.
    pub scrub_bench: bool,
    /// `--target-fps N`: the frame rate the benchmark checks its 95th percentile against.
    pub target_fps: Option<u32>,
    /// `--window WxH`: the window's size in physical pixels (native only).
    pub window: Option<(u32, u32)>,
    /// Hidden `--check-data[=DIR]` (native only): load the globe data headlessly and exit.
    pub check_data: Option<DataCheck>,
    /// Unrecognized arguments. Diagnostics log only their count because URL query
    /// values and command-line arguments can contain private data.
    pub unknown: Vec<String>,
}

/// What `--check-data[=DIR]` checks.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DataCheck {
    /// The bake's output folder (the one holding `manifest.json`), relative to the working
    /// directory; `None` checks the game's own `assets/data/globe`.
    pub dir: Option<String>,
}

const CHECK_DATA_FLAG: &str = "--check-data";
const PROBE_FLAG: &str = "--diagnostics-probe";
const BENCH_FLAG: &str = "--scrub-bench";
const TARGET_FPS_FLAG: &str = "--target-fps";
const WINDOW_FLAG: &str = "--window";

/// `WxH` with both sides at least 1, e.g. `2560x1440`.
pub fn parse_window_size(text: &str) -> Option<(u32, u32)> {
    let (w, h) = text.split_once(['x', 'X'])?;
    let (w, h) = (w.parse::<u32>().ok()?, h.parse::<u32>().ok()?);
    (w > 0 && h > 0).then_some((w, h))
}

/// A frame rate from 1 to 1000.
fn parse_fps(text: &str) -> Option<u32> {
    text.parse::<u32>().ok().filter(|f| (1..=1000).contains(f))
}

/// Parses native arguments, without the program name (`std::env::args().skip(1)`).
///
/// Anything this build does not understand, including a flag with a missing or malformed value
/// (an unknown probe mode, `--window 2560`), lands in [`LaunchArgs::unknown`] and changes nothing.
pub fn parse_launch_args<I: IntoIterator<Item = String>>(args: I) -> LaunchArgs {
    let mut parsed = LaunchArgs::default();
    let mut args = args.into_iter().peekable();
    while let Some(arg) = args.next() {
        if arg == BENCH_FLAG {
            parsed.scrub_bench = true;
            continue;
        }
        if arg == CHECK_DATA_FLAG {
            parsed.check_data = Some(DataCheck::default());
            continue;
        }
        if let Some(dir) = arg
            .strip_prefix(CHECK_DATA_FLAG)
            .and_then(|r| r.strip_prefix('='))
        {
            if dir.is_empty() {
                parsed.unknown.push(arg);
            } else {
                parsed.check_data = Some(DataCheck {
                    dir: Some(dir.to_string()),
                });
            }
            continue;
        }
        let Some(flag) = [PROBE_FLAG, TARGET_FPS_FLAG, WINDOW_FLAG]
            .into_iter()
            .find(|f| arg == *f || arg.strip_prefix(f).is_some_and(|r| r.starts_with('=')))
        else {
            parsed.unknown.push(arg);
            continue;
        };
        let value = match arg.strip_prefix(flag).and_then(|v| v.strip_prefix('=')) {
            Some(value) => Some(value.to_string()),
            None => args.next_if(|next| !next.starts_with("--")),
        };
        let accepted = value.as_deref().is_some_and(|v| {
            match flag {
                PROBE_FLAG => ProbeMode::parse(v).map(|m| parsed.diagnostics_probe = Some(m)),
                TARGET_FPS_FLAG => parse_fps(v).map(|f| parsed.target_fps = Some(f)),
                _ => parse_window_size(v).map(|s| parsed.window = Some(s)),
            }
            .is_some()
        });
        if !accepted {
            parsed.unknown.push(match value {
                Some(value) => format!("{flag}={value}"),
                None => arg,
            });
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
        assert_eq!(parsed.target_fps, Some(30));
        assert_eq!(parsed.unknown, args(&["--diagnostics-probe"]));
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
    fn scrub_bench_flags() {
        let parsed = parse_launch_args(args(&[
            "--scrub-bench",
            "--target-fps",
            "60",
            "--window",
            "2560x1440",
        ]));
        assert!(parsed.scrub_bench);
        assert_eq!(parsed.target_fps, Some(60));
        assert_eq!(parsed.window, Some((2560, 1440)));
        assert!(parsed.unknown.is_empty(), "{:?}", parsed.unknown);

        let parsed = parse_launch_args(args(&["--window=1280x720", "--target-fps=30"]));
        assert_eq!(parsed.window, Some((1280, 720)));
        assert_eq!(parsed.target_fps, Some(30));
    }

    #[test]
    fn malformed_window_and_fps_are_rejected() {
        for bad in [
            "2560",
            "2560x",
            "x1440",
            "0x1440",
            "2560x-1",
            "axb",
            "2560x1440x2",
        ] {
            let parsed = parse_launch_args(args(&["--window", bad]));
            assert_eq!(parsed.window, None, "{bad}");
            assert_eq!(parsed.unknown, vec![format!("--window={bad}")], "{bad}");
        }
        let parsed = parse_launch_args(args(&["--target-fps=0", "--target-fps", "fast"]));
        assert_eq!(parsed.target_fps, None);
        assert_eq!(
            parsed.unknown,
            args(&["--target-fps=0", "--target-fps=fast"])
        );
        // A missing value: the next flag is not swallowed.
        let parsed = parse_launch_args(args(&["--window", "--scrub-bench"]));
        assert_eq!(parsed.window, None);
        assert!(parsed.scrub_bench);
        assert_eq!(parsed.unknown, args(&["--window"]));
    }

    #[test]
    fn web_query_scrub_bench() {
        let parsed = parse_query_args("?scrub-bench&target-fps=30");
        assert!(parsed.scrub_bench);
        assert_eq!(parsed.target_fps, Some(30));
        assert!(parsed.unknown.is_empty());
    }

    #[test]
    fn check_data_flag() {
        let parsed = parse_launch_args(args(&["--check-data"]));
        assert_eq!(parsed.check_data, Some(DataCheck { dir: None }));
        assert!(parsed.unknown.is_empty());
        let parsed = parse_launch_args(args(&["--check-data=crates/timefly/assets/data/globe"]));
        assert_eq!(
            parsed.check_data,
            Some(DataCheck {
                dir: Some("crates/timefly/assets/data/globe".to_string())
            })
        );
        // The value only comes with `=`: a following argument is not swallowed.
        let parsed = parse_launch_args(args(&["--check-data", "--scrub-bench"]));
        assert_eq!(parsed.check_data, Some(DataCheck { dir: None }));
        assert!(parsed.scrub_bench);
        let parsed = parse_launch_args(args(&["--check-data="]));
        assert_eq!(parsed.check_data, None);
        assert_eq!(parsed.unknown, args(&["--check-data="]));
        let parsed = parse_launch_args(args(&["--check-database"]));
        assert_eq!(parsed.check_data, None);
        assert_eq!(parsed.unknown, args(&["--check-database"]));
    }

    #[test]
    fn web_query_unknown_keys() {
        let parsed = parse_query_args("utm_source=x&diagnostics-probe=clean&debug");
        assert_eq!(parsed.diagnostics_probe, Some(ProbeMode::Clean));
        assert_eq!(parsed.unknown, args(&["--utm_source=x", "--debug"]));
    }
}
