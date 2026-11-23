# Region worlds (M2): performance and size

## Tile bake (Task 1)

`cd tools/bake && uv run --locked timefly-bake tiles --raw ../../data/raw --out ../../crates/timefly/assets/data/tiles`

Measured on 2026-11-22 (Apple M4, 16 GB, Python 3.13).

| Measure | Value |
|---|---|
| Tiles | 2,592 (36 × 72), each 301 × 301 × 2 float16, zstd 19 |
| Total tile bytes | 244,015,001 (about 244.0 MB; `du` reports 239 MiB on disk) |
| Largest tile | 172,862 bytes |
| Wall time | 178.5 s (peak resident set 3.3 GB) |
| Budget | design estimate 150–250 MB; stop threshold 300 MB; Pages site limit 1 GB |

The total is inside the design's 150–250 MB estimate and about a quarter of the Pages limit
(the 17 MB globe set shares it). A region needs at most 4 tiles (about 0.7 MB at the largest tile
size) on the web.

Inputs: `etopo/ETOPO_2022_v1_60s_N90W180_surface.nc` (cell-registered, averaged onto nodes) and
the 1991–2020 30″ GeoTIFF of `koppen/koppen_geiger_tif.zip` (2 × 2 mode onto nodes); see
`data/README.md`.
