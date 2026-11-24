# The region bench over one chosen region

**Status:** short design, approved by the owner 2026-11-24. It lands on the `art-pass` branch,
next to `--no-dunes` (`2026-11-24-no-dunes-design.md`). The two use each other: a single-region
run over a sand sea is the quickest way to measure what the dunes cost.

## Why

The region bench always flies all eight stops, then a globe pan and ten memory cycles: about
10 minutes, and in Chrome the owner has to keep a window in front the whole time. The owner wants
to bench one place, such as a stop that failed or a sand sea, without the rest.

## Goals

- `--region-bench --region=LAT,LON,AGE_YEARS` (web: `?region-bench&region=LAT,LON,AGE_YEARS`)
  benches that one place. Any place works, not only the eight stops. The bench path already
  adapts to any centre, because it searches for a coast to fly along.
- It flies the same 60 s path once, with the same rules: the display-paced or fixed frame-rate
  rule (R14), the first-entry generation gate, and R11. The chosen place is the first region, so
  R11's ≤ 250 ms first-region worst frame applies to it.
- It skips the globe pan and the memory cycles. A run takes about a minute.
- The report says it was a single-region run and which place, so it is never read as the full
  bench.

## Non-goals

- Picking a stop by name (`--region-bench=red-sea`). A stop's coordinates do the same.
- More than one chosen region, or a chosen region plus the cycles.

## Approaches

- **Combine `--region-bench` with `--region`** (chosen): reuses the option the owner already
  uses, works for any place, and today the combination is simply ignored.
- A new value, `--region-bench=NAME`: only the eight stops, and a second spelling to learn.
- A separate `--bench-region` flag: a third option for the same place argument.

## The design

- `lib.rs` today lets `--region-bench` win over `--region`. With both, it builds the bench with
  one region instead.
- `RegionBenchConfig` carries the regions to fly (`Vec<BenchRegion>`, defaulting to
  `BENCH_REGIONS`) instead of the bench reading the constant. A single-region config has one
  `BenchRegion { name: "Chosen region", arg, pan_from_previous: false }` and `cycles: 0`.
- Everything that reads `BENCH_REGIONS`, `stop_count()` or `cycle_region()` reads the config's
  list instead. That covers the stage machine, the banner's "k of N", region blocks and the
  summary. With no cycles, the run goes from the last region straight to the summary. The summary's
  warm-generation and memory lines say "not measured (single-region run)", and they don't fail
  the result.
- The summary's first line after the header says `regions: single (LAT, LON at AGE years)` for a
  single run, or `regions: all 8` for the full bench.
- Name-keyed special cases stay keyed to the full bench's stops. For example, the Himalayas'
  "reported only" load window doesn't apply to a chosen region, even if it is at the same place.

## Left to the builder

- Whether the "Chosen region" name or the coordinates head the region block. The block's title
  line already prints the latitude, longitude and age.
