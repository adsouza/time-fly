//! Launch arguments: one parser for native argv and the web URL query string.
//!
//! Flags: `--diagnostics-probe <mode>` (hidden), `--check-data[=DIR]` (hidden), `--scrub-bench`,
//! `--region-bench`, `--journey-bench`, `--keep-going`, `--no-dunes`, `--target-fps N`,
//! `--window WxH`, `--region LAT,LON,AGE_YEARS` and `--from-shot <json>`. A value
//! may follow its flag as `--flag=value` or as the next argument; a next argument that starts
//! with `--` is the next flag, never a value.
//! `--check-data` takes its optional value only as `--check-data=DIR`.

use timefly_core::deep_time::MAX_YEARS_BP;

use crate::diagnostics::ProbeMode;

/// What the game was asked to do at launch.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LaunchArgs {
    /// Hidden `--diagnostics-probe=<mode>` (web: `?diagnostics-probe=<mode>`).
    pub diagnostics_probe: Option<ProbeMode>,
    /// `--scrub-bench` (web: `?scrub-bench`): run the automated scrub benchmark and report.
    pub scrub_bench: bool,
    /// `--region-bench` (web: `?region-bench`): fly the scripted path in the eight bench regions,
    /// then the enter/leave memory cycles, and report. It takes precedence over `--scrub-bench`.
    /// With `--region` it benches only that region: one flight, no globe pan or memory cycles.
    pub region_bench: bool,
    /// `--journey-bench` (web: `?journey-bench`): from the globe, dive into a region, hop twice
    /// to its neighbours and climb back out, several times, and report. A bare flag. It takes
    /// precedence over the scrub bench, and the region bench over it.
    pub journey_bench: bool,
    /// `--keep-going` (web: `?keep-going`): the benches run every stop even after a gate has
    /// failed, instead of stopping at the first failed gate. A bare flag.
    pub keep_going: bool,
    /// `--no-dunes` (web: `?no-dunes`): skip the terrain's dune shading, for slower machines.
    /// A bare flag: `--no-dunes=VALUE` is not it and goes to `unknown`.
    pub no_dunes: bool,
    /// `--target-fps N`: the frame rate the benchmarks check their frames against.
    pub target_fps: Option<u32>,
    /// `--window WxH`: the window's size in logical pixels (native only): on a 2× display a
    /// 1280x720 window is 2560 × 1440 physical pixels.
    pub window: Option<(u32, u32)>,
    /// Hidden `--check-data[=DIR]` (native only): load the globe data headlessly and exit.
    pub check_data: Option<DataCheck>,
    /// `--region=LAT,LON,AGE_YEARS` (web: `?region=LAT,LON,AGE_YEARS`): enter the region centred
    /// there once the globe data is ready.
    pub region: Option<RegionArg>,
    /// `--from-shot <json>` (native only): start at a bug shot's pose, read from its sidecar
    /// (`crate::ui::bug_shot`); it takes precedence over `--region`, the benches over it.
    pub from_shot: Option<String>,
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
const REGION_BENCH_FLAG: &str = "--region-bench";
const JOURNEY_BENCH_FLAG: &str = "--journey-bench";
const KEEP_GOING_FLAG: &str = "--keep-going";
const NO_DUNES_FLAG: &str = "--no-dunes";
const TARGET_FPS_FLAG: &str = "--target-fps";
const WINDOW_FLAG: &str = "--window";
const REGION_FLAG: &str = "--region";
const FROM_SHOT_FLAG: &str = "--from-shot";

/// Where `--region` asks to go: an era-frame latitude and longitude (degrees, as the globe shows
/// them at that age) and an age in years before present.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct RegionArg {
    pub lat_deg: f64,
    pub lon_deg: f64,
    pub years: f64,
}

// `parse_region` only builds finite values, so equality is reflexive.
impl Eq for RegionArg {}

/// `LAT,LON,AGE_YEARS` with latitude in [−90, 90], longitude in [−180, 180] and the age in
/// [0, 540 Ma]; every value finite, whitespace around each allowed.
pub fn parse_region(text: &str) -> Option<RegionArg> {
    let values: Vec<f64> = text
        .split(',')
        .map(|v| v.trim().parse::<f64>().ok().filter(|v| v.is_finite()))
        .collect::<Option<_>>()?;
    let [lat_deg, lon_deg, years] = values[..] else {
        return None;
    };
    let ok = (-90.0..=90.0).contains(&lat_deg)
        && (-180.0..=180.0).contains(&lon_deg)
        && (0.0..=MAX_YEARS_BP).contains(&years);
    ok.then_some(RegionArg {
        lat_deg,
        lon_deg,
        years,
    })
}

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
        if arg == REGION_BENCH_FLAG {
            parsed.region_bench = true;
            continue;
        }
        if arg == JOURNEY_BENCH_FLAG {
            parsed.journey_bench = true;
            continue;
        }
        if arg == KEEP_GOING_FLAG {
            parsed.keep_going = true;
            continue;
        }
        if arg == NO_DUNES_FLAG {
            parsed.no_dunes = true;
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
        let Some(flag) = [
            PROBE_FLAG,
            TARGET_FPS_FLAG,
            WINDOW_FLAG,
            REGION_FLAG,
            FROM_SHOT_FLAG,
        ]
        .into_iter()
        .find(|f| arg == *f || arg.strip_prefix(f).is_some_and(|r| r.starts_with('='))) else {
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
                REGION_FLAG => parse_region(v).map(|r| parsed.region = Some(r)),
                FROM_SHOT_FLAG => (!v.is_empty()).then(|| parsed.from_shot = Some(v.to_string())),
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

    #[test]
    fn no_dunes_is_a_bare_flag() {
        assert!(parse_launch_args(args(&["--no-dunes"])).no_dunes);
        assert!(!parse_launch_args(args(&[])).no_dunes);
        let valued = parse_launch_args(args(&["--no-dunes=1"]));
        assert!(!valued.no_dunes);
        assert_eq!(valued.unknown, args(&["--no-dunes=1"]));
    }

    #[test]
    fn no_dunes_on_the_web_in_any_order() {
        for q in ["?region-bench&no-dunes", "?no-dunes&region-bench"] {
            let parsed = parse_query_args(q);
            assert!(parsed.no_dunes && parsed.region_bench, "{q}");
        }
        assert!(!parse_query_args("?no-dunes=true").no_dunes);
    }

    #[test]
    fn keep_going_is_a_bare_flag_on_both_platforms() {
        let parsed = parse_launch_args(args(&["--region-bench", "--keep-going"]));
        assert!(parsed.keep_going && parsed.region_bench);
        assert!(parsed.unknown.is_empty(), "{:?}", parsed.unknown);
        assert!(!parse_launch_args(args(&["--region-bench"])).keep_going);
        let valued = parse_launch_args(args(&["--keep-going=1"]));
        assert!(!valued.keep_going);
        assert_eq!(valued.unknown, args(&["--keep-going=1"]));
        for q in ["?journey-bench&keep-going", "?keep-going&journey-bench"] {
            let parsed = parse_query_args(q);
            assert!(parsed.keep_going && parsed.journey_bench, "{q}");
        }
        assert!(!parse_query_args("?journey-bench").keep_going);
    }

    #[test]
    fn journey_bench_is_a_bare_flag() {
        let parsed = parse_launch_args(args(&["--journey-bench"]));
        assert!(parsed.journey_bench);
        assert!(!parsed.region_bench && !parsed.scrub_bench);
        assert!(parsed.unknown.is_empty(), "{:?}", parsed.unknown);
        assert!(!parse_launch_args(args(&[])).journey_bench);
        let valued = parse_launch_args(args(&["--journey-bench=1"]));
        assert!(!valued.journey_bench);
        assert_eq!(valued.unknown, args(&["--journey-bench=1"]));
        // Not a prefix match.
        let longer = parse_launch_args(args(&["--journey-benchmark"]));
        assert!(!longer.journey_bench);
        assert_eq!(longer.unknown, args(&["--journey-benchmark"]));
        // Vsync off on native, as the other benches, so frame times show the real cost.
        #[cfg(not(target_arch = "wasm32"))]
        assert_eq!(
            crate::primary_window(&parsed).present_mode,
            bevy::window::PresentMode::AutoNoVsync
        );
    }

    #[test]
    fn journey_bench_on_the_web_in_any_order() {
        for q in [
            "?journey-bench&target-fps=30",
            "?target-fps=30&journey-bench",
        ] {
            let parsed = parse_query_args(q);
            assert!(parsed.journey_bench, "{q}");
            assert_eq!(parsed.target_fps, Some(30), "{q}");
            assert!(parsed.unknown.is_empty(), "{q}: {:?}", parsed.unknown);
        }
        assert!(!parse_query_args("?journey-bench=true").journey_bench);
    }

    #[test]
    fn region_bench_with_a_region_in_either_order() {
        for q in [
            "?region=20,37.5,0&region-bench",
            "?region-bench&region=20,37.5,0",
        ] {
            let p = parse_query_args(q);
            assert!(p.region_bench && p.region.is_some(), "{q}");
        }
        let bad = parse_launch_args(["--region-bench", "--region=200,0,0"].map(String::from));
        assert!(bad.region_bench && bad.region.is_none());
        assert_eq!(bad.unknown, vec!["--region=200,0,0".to_string()]);
    }

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
    fn region_flag() {
        let want = Some(RegionArg {
            lat_deg: 46.5,
            lon_deg: 10.0,
            years: 0.0,
        });
        assert_eq!(
            parse_launch_args(args(&["--region=46.5,10,0"])).region,
            want
        );
        let parsed = parse_launch_args(args(&["--region", "46.5,10,0"]));
        assert_eq!(parsed.region, want);
        assert!(parsed.unknown.is_empty(), "{:?}", parsed.unknown);
        // Negative values are values, not flags; whitespace around a number is allowed.
        let parsed = parse_launch_args(args(&["--region", "-33.9, -151.25 ,2.5e8"]));
        assert_eq!(
            parsed.region,
            Some(RegionArg {
                lat_deg: -33.9,
                lon_deg: -151.25,
                years: 2.5e8,
            })
        );
        // The edges of each range are accepted.
        assert!(parse_region("90,180,540000000").is_some());
        assert!(parse_region("-90,-180,0").is_some());
    }

    #[test]
    fn from_shot_flag() {
        let path = "/Users/me/Pictures/Time Fly/timefly_20261126-143005_22.870N_71.420E.json";
        for argv in [
            vec![format!("--from-shot={path}")],
            vec!["--from-shot".into(), path.into()],
        ] {
            let parsed = parse_launch_args(argv);
            assert_eq!(parsed.from_shot.as_deref(), Some(path));
            assert!(parsed.unknown.is_empty(), "{:?}", parsed.unknown);
        }
        // With no value it is unknown and does nothing; the next flag is never its value.
        let parsed = parse_launch_args(args(&["--from-shot", "--no-dunes"]));
        assert_eq!(parsed.from_shot, None);
        assert!(parsed.no_dunes);
        assert_eq!(parsed.unknown, args(&["--from-shot"]));
        assert_eq!(parse_launch_args(args(&["--from-shot="])).from_shot, None);
    }

    #[test]
    fn malformed_region_is_rejected() {
        for bad in [
            "46.5,10",
            "46.5,10,0,1",
            "a,b,c",
            "46.5,,0",
            "",
            "90.5,0,0",
            "0,180.5,0",
            "0,0,-1",
            "0,0,540000001",
            "NaN,0,0",
            "0,inf,0",
        ] {
            let parsed = parse_launch_args(args(&[&format!("--region={bad}")]));
            assert_eq!(parsed.region, None, "{bad}");
            assert_eq!(parsed.unknown, vec![format!("--region={bad}")], "{bad}");
        }
        // A missing value: the next flag is not swallowed.
        let parsed = parse_launch_args(args(&["--region", "--scrub-bench"]));
        assert_eq!(parsed.region, None);
        assert!(parsed.scrub_bench);
        assert_eq!(parsed.unknown, args(&["--region"]));
    }

    #[test]
    fn web_query_region() {
        let want = parse_launch_args(args(&["--region=46.5,10,21000"])).region;
        assert!(want.is_some());
        assert_eq!(parse_query_args("?region=46.5,10,21000").region, want);
        assert_eq!(parse_query_args("?region=46.5%2C10%2C21000").region, want);
        let parsed = parse_query_args("?region=north");
        assert_eq!(parsed.region, None);
        assert_eq!(parsed.unknown, args(&["--region=north"]));
    }

    /// `--region` and `?region=` start directly in a region; a malformed value is ignored and
    /// lands in `unknown`, which the session start-up logs as a `warn` (by count only).
    #[test]
    fn region_arg_parses() {
        let want = Some(RegionArg {
            lat_deg: -22.0,
            lon_deg: -18.0,
            years: 250e6,
        });
        for native in [
            args(&["--region=-22,-18,250000000"]),
            args(&["--region", "-22,-18,2.5e8"]),
            args(&["--scrub-bench", "--region= -22 , -18 , 250000000 "]),
        ] {
            let parsed = parse_launch_args(native.clone());
            assert_eq!(parsed.region, want, "{native:?}");
            assert!(parsed.unknown.is_empty(), "{native:?}");
        }
        for web in [
            "?region=-22,-18,250000000",
            "region=-22%2C-18%2C2.5e8",
            "?scrub-bench&region=-22,-18,250000000",
        ] {
            assert_eq!(parse_query_args(web).region, want, "{web}");
        }
        for bad in ["--region=-22,-18", "--region=north", "--region=0,0,6e8"] {
            let parsed = parse_launch_args(args(&[bad]));
            assert_eq!(parsed.region, None, "{bad}");
            assert_eq!(parsed.unknown, args(&[bad]), "{bad}");
        }
        let parsed = parse_query_args("?region=0,200,0");
        assert_eq!(parsed.region, None);
        assert_eq!(parsed.unknown, args(&["--region=0,200,0"]));
    }

    #[test]
    fn web_query_unknown_keys() {
        let parsed = parse_query_args("utm_source=x&diagnostics-probe=clean&debug");
        assert_eq!(parsed.diagnostics_probe, Some(ProbeMode::Clean));
        assert_eq!(parsed.unknown, args(&["--utm_source=x", "--debug"]));
    }
}
