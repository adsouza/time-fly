# Linux Vulkan check (Task 11, Step 7)

The owner's Linux machine is for testing only. CI (`.github/workflows/linux-package.yml`) builds
two packages from the same release binary and baked data:

| Artifact | Installs to | Logs |
|---|---|---|
| `time-fly_<version>_amd64.deb` (version `0.1.0~git<sha>`) | `/opt/time-fly`, launcher `/usr/bin/time-fly` | `~/.local/state/time-fly/logs/` |
| `time-fly-<sha>.flatpak` (app ID `io.github.adsouza.TimeFly`, runtime `org.freedesktop.Platform//26.08`) | inside the sandbox | `~/.var/app/io.github.adsouza.TimeFly/.local/state/time-fly/logs/` |

Each run makes a session directory under the logs folder (`session.log`, and `crash.txt` after a
panic). Send back the bench output blocks, the results of each check below, and the crash
files from the crash probe.

## 0. Get the packages

The workflow runs only when dispatched (or on a `v*` tag). Artifacts are kept for 14 days.

```sh
gh run list --workflow linux-package.yml --limit 5      # pick the green run
gh run view <run-id>                                    # lists its artifacts by name
gh run download <run-id> --dir time-fly-ci \
  -n time-fly_<version>_amd64.deb -n time-fly-<sha>.flatpak
```

Name the two packages with `-n`: a plain `gh run download` also pulls `staged-tree` (an
intermediate) and `build-logs` (the bake log and CI test output). When it downloads more than one
artifact, `gh` puts each in its own subdirectory named after the artifact, so the files land at
`time-fly-ci/time-fly_<version>_amd64.deb/time-fly_<version>_amd64.deb` and
`time-fly-ci/time-fly-<sha>.flatpak/time-fly-<sha>.flatpak`; sections 2 and 3 `cd` into them.

Or download `time-fly_<version>_amd64.deb` and `time-fly-<sha>.flatpak` from the run's page on
the Actions tab (each downloads as a `.zip` holding the file; unzip it first, and skip the `cd`
lines below).

## 1. Record the GPU

```sh
vulkaninfo --summary            # Debian/Ubuntu: sudo apt install vulkan-tools
```

Note the GPU name, `driverName`, `driverInfo` / driver version and `apiVersion`. A software
device (llvmpipe, lavapipe) doesn't count for acceptance: the check needs a hardware GPU.

## 2. The `.deb`

```sh
cd time-fly-ci/time-fly_<version>_amd64.deb             # from where you ran gh run download
sudo apt install ./time-fly_<version>_amd64.deb
cd ../..
```

- [ ] Installs without dependency errors (`apt` pulls `libvulkan1` and the recommended winit
      libraries).
- [ ] **Time Fly** appears in the application menu with the globe icon. Launch it from there once:
      the window opens, the dock/taskbar shows the same icon, and the globe renders.
- [ ] Quit, then from a terminal: `WGPU_BACKEND=vulkan time-fly`. The log line naming the
      adapter says Vulkan and the GPU from step 1 (also in `session.log`).
- [ ] Visual checks (section 4).
- [ ] Bench (section 5): `time-fly --scrub-bench --target-fps 60 --window 2560x1440`
      (add `WGPU_BACKEND=vulkan` in front if Vulkan isn't the default). Paste the whole output
      block back.
- [ ] Crash probe (section 6) with `time-fly`.

## 3. The Flatpak

```sh
cd time-fly-ci/time-fly-<sha>.flatpak                   # from where you ran gh run download
flatpak remote-add --if-not-exists --user flathub https://dl.flathub.org/repo/flathub.flatpakrepo
flatpak install --user ./time-fly-<sha>.flatpak         # pulls the 26.08 runtime from Flathub
cd ../..
flatpak run --env=WGPU_BACKEND=vulkan io.github.adsouza.TimeFly
```

On NVIDIA's proprietary driver, Flatpak needs the `org.freedesktop.Platform.GL.nvidia-<version>`
extension matching the host driver; Flatpak normally installs it from Flathub automatically
(check `flatpak list --runtime | grep -i nvidia` if the game falls back to llvmpipe).

- [ ] Installs and runs; the adapter log line says Vulkan on the hardware GPU.
- [ ] Launch once from the application menu as well (same icon, same window).
- [ ] Visual checks (section 4).
- [ ] Bench: `flatpak run --env=WGPU_BACKEND=vulkan io.github.adsouza.TimeFly --scrub-bench
      --target-fps 60 --window 2560x1440`. Paste the output block back.
- [ ] Crash probe (section 6) with `flatpak run io.github.adsouza.TimeFly`.

## 4. Visual checks (Task 10 Step 5 list)

Use the slider (or `[` / `]` to step older / younger), drag to orbit, scroll to zoom, `Esc` for
credits. Compare with the Mac screenshots in `docs/testing/globe-m1/`.

- [ ] Present day: coastlines match a modern map (`globe-present.png`).
- [ ] About 21 ka: ice sheets over North America and Scandinavia, and the Bering land bridge
      between Siberia and Alaska (`globe-21ka-bering.png`).
- [ ] Bridge, 26-30 ka: scrubbing through it crossfades smoothly from the ICE-6G_C frames to the
      deep-time terrain, with no pop or black frame (`globe-28ka-bridge.png`).
- [ ] About 250 Ma: Pangaea as one landmass (`globe-250ma.png`).
- [ ] 540 Ma: the slider's oldest end shows the Cambrian continents (`globe-540ma.png`).
- [ ] Full-range scrubbing both ways runs without stalls, gaps or flicker.
- [ ] Orbit limits: zoom stops between about 1.15 and 6 Earth radii from the centre, and
      dragging stops at 89 degrees latitude, so the camera never flips over a pole.
- [ ] The age label tracks the slider and the stepped ages.
- [ ] `Esc` opens the credits with every data citation; `Esc` again closes them.

## 5. Bench

The bench scrubs the full range for 20 s each way, prints one plain-text block (session, adapter,
OS, resolution, bake, cold load, memory, per-direction median / p95 / worst, and the rule that
decided PASS or FAIL), then exits. On native it asks for an uncapped present mode
(`AutoNoVsync`); if the driver still paces frames to the display, the block says so and judges
missed frames instead of p95 (ruling R14). It exits 0 on PASS and 3 on FAIL; if the globe fails
to load or render, it prints a FAIL block with the reason instead of waiting. Paste the block
back unedited.

Without a usable GPU driver the game doesn't start: it shows a dialog ("No compatible GPU/driver
found (Vulkan/Metal)") with the log path and exits 1, without a crash notice on the next launch.
If that happens on the hardware GPU, send back `vulkaninfo --summary` and that `session.log`.

## 6. Crash probe

```sh
time-fly --diagnostics-probe=panic                     # .deb
time-fly                                               # normal launch
flatpak run io.github.adsouza.TimeFly --diagnostics-probe=panic
flatpak run io.github.adsouza.TimeFly
```

- [ ] The probe exits with a panic and prints where the crash report was saved.
- [ ] The next normal launch shows the crash notice. The path in it is under that package's log
      location (table above; home shown as `~`).
- [ ] **Show log** opens the file manager at the crash file (Flatpak: through the manifest's
      `--talk-name=org.freedesktop.FileManager1` D-Bus permission, a session-bus name the
      sandbox may talk to, not a portal).
- [ ] Send the `crash.txt` and `session.log` from that session directory back.

## 7. Uninstall

```sh
sudo apt remove time-fly
flatpak uninstall --user io.github.adsouza.TimeFly
```

Logs stay behind in the folders above; delete them by hand if you like.

## Acceptance (Milestone 1)

Milestone 1 is accepted when both packages install and run on Vulkan on a hardware GPU, the visual
checks pass in both, and the `.deb` bench block ends `result: PASS` against 60 fps (the Flatpak
block is recorded too). Under ruling R14 a display-paced run (the usual case, even with
`AutoNoVsync` requested, when the driver or compositor paces to the display) passes when at most
1% of frames in each direction are missed (longer than 25 ms, 1.5x the 16.67 ms interval) and
each direction's median is at most 17.5 ms (the interval +5%); a run that is really uncapped
passes when each direction's p95 is at most 16.67 ms. The raw p95 is always reported. If the
target is missed, treat it as a Task 10 Step 6 strategy change and rerun. Results go
into `data/README.md` (Performance and memory) next to the Mac numbers.

## Results

_Not run yet._

| Package | GPU / driver | Backend | Visual checks | Pacing | Older median / p95 / missed | Younger median / p95 / missed | Bench result | Crash notice |
|---|---|---|---|---|---|---|---|---|
| `.deb` | | | | | | | | |
| Flatpak | | | | | | | | |
