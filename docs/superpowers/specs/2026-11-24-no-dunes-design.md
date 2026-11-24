# A launch option that turns the dunes off (`--no-dunes`)

**Status:** short design, for the owner's approval. Builds on the art pass
(`docs/superpowers/specs/2026-11-23-art-pass-design.md`) and lands on the `art-pass` branch, the
only place the dunes exist.

## Why

The art pass's sand dunes cost GPU time on every sand-sea pixel. In the gated Chrome bench
(`5434520`, 3200 × 1800) the Pangaea stop, a 250 Ma desert that is nearly all sand sea, ran at a
32.14 ms median, against 20–25 ms for the other stops and 16.95 ms at M2. On slower machines that
is the difference between a playable and an unplayable desert. The owner wants a way to turn the
dunes off there, and it must **skip the work**, not just hide the result.

## Goals

- `--no-dunes` (native) and `?no-dunes` (web) turn the dunes off for the whole session.
- When off, the terrain shader runs none of the dune work: no crest phases, no octave weights,
  no small-dune pattern, no dune shading tilt and no `bound_dune_tilt`.
- Sand seas stay where they are and keep their sand colour. They draw as flat, still sand: no
  crests and no creep.
- The region bench accepts it (`--region-bench --no-dunes`), and its report says which way the
  dunes were, so a run with them off is never mistaken for one with them on.
- A measurement shows what it saves: the region bench with and without the flag, Pangaea first.

## Non-goals

- No settings screen, no in-game toggle, no saved choice (approaches B and C below).
- No other quality options (swell, banding, shadows). Those can follow the same pattern later.
- No change to how the dunes look when they are on.

## Anti-goals

- A flag that only zeroes the dune strength while the shader still computes the crests. That
  saves nothing.
- A seam or colour jump where a sand sea meets stony plain because the off path drew the sand
  differently from the on path's base colour.

## Approaches

- **A. A launch option** (chosen by the owner): smallest, works on both platforms, and the bench
  can use it directly. A player must know the flag exists.
- B. A remembered in-game toggle: easier to find, but there is no settings screen to show it on
  yet.
- C. Turn the dunes off automatically when region frames run slow: best for players, but the most
  work, it changes what the bench measures, and it needs care not to flip back and forth.

B and C can reuse A's switch later.

## The design

- **Parsing.** `LaunchArgs` gains `no_dunes: bool`, parsed in `args.rs` like `--region-bench`
  (a bare flag; the web query `?no-dunes` maps to it through `parse_query_args`). A value after
  it is not consumed.
- **Into the look.** The flag reaches the region's `TerrainLook` as a value every pixel reads the
  same way (a uniform, not a per-vertex or per-texel value), so the GPU skips the dune branch
  as a whole. The art pass's `TerrainLook` is already the dunes' uniform.
- **In the shader.** `terrain.wesl` tests that value once, before any dune maths, and takes the
  flat-sand path when the dunes are off. The sand-sea weight and the stony-plain split stay, so
  sand seas keep their place and colour.
- **Bench report.** Every region block and the summary print `dunes: on` or `dunes: off`.
- **Docs.** `docs/testing/region-m2/performance.md` records the with/without numbers. The flag is
  listed where the other launch options are documented.

## Left to the builder

- Which spare `TerrainLook` component or new field carries the switch. A new field must keep the
  uniform's layout tests and the WESL mirror tests passing.
- Whether `region_tour` (the reference renders) takes the flag too.
- Where the flag-off path's flat-sand colour comes from, as long as it matches the dunes-on base
  colour at the sand-sea edge.
