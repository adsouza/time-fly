# Manual checks on the Mac

Checks that need a person at the screen: native dialogs, clicks, Finder. Everything else is in
`cargo test -p timefly` (unit tests and `tests/crash_flow.rs`).

Run cargo with rustup's toolchain (see `docs/dev/build-disk.md`):

```sh
export PATH="/opt/homebrew/opt/rustup/bin:$PATH"
```

Sessions go to `~/Library/Logs/Time Fly/`. To keep these checks out of your real logs, run them
with `TIMEFLY_LOG_DIR=/tmp/timefly-checks` set on every command (then **Show log** points there).

## Crash notice (Task 2)

Already verified automatically (2026-11-22, from session files and logs, no clicks): each probe
records the right outcome, and the next launch prints the notice to stderr and picks the right
route (system dialog for a crash before the first frame, in-game overlay otherwise). What is left
is how the notices look and what the buttons do.

### 1. Crash before the first frame: system dialog

- [ ] `cargo run -p timefly -- --diagnostics-probe=panic` exits with a panic and prints
      "A crash report is saved at …/crash.txt".
- [ ] `cargo run -p timefly` shows a system dialog **before** the game window, with the notice
      text, the `crash.txt` path (home shown as `~`) and the buttons **Show log** and **Not now**.
- [ ] **Show log** reveals `crash.txt` in Finder, then the game window opens.
- [ ] Quit the game and run `cargo run -p timefly` again: no dialog, no overlay.
- [ ] Repeat the probe and answer **Not now**: the game opens, and the next launch after that
      shows nothing.

### 2. Crash after the first frame: in-game overlay

- [ ] `cargo run -p timefly -- --diagnostics-probe=panic-after-frame` opens a window, renders,
      then exits with a panic.
- [ ] `cargo run -p timefly` opens the game window with no system dialog, and after the first
      frame shows a dark overlay with the notice text, the `crash.txt` path and the buttons
      **Show log**, **Report problem** and **Dismiss**. The text is readable and the buttons
      respond to the mouse.
- [ ] **Show log** reveals `crash.txt` in Finder. The overlay stays.
- [ ] **Dismiss** removes the overlay.
- [ ] Quit and relaunch: no notice.

### 3. Abort: notice points at the session log

- [ ] `cargo run -p timefly -- --diagnostics-probe=abort` ends with "Abort trap".
- [ ] `cargo run -p timefly` shows the system dialog with a path ending in `session.log`
      (there is no crash report for an abort). **Show log** reveals `session.log`.

### 4. Report problem (after the first push to GitHub)

The issue form (`.github/ISSUE_TEMPLATE/crash_report.yml`) only exists on GitHub once this
branch is pushed, so do this after Task 11's first push.

- [ ] Create the `crash` label once (GitHub applies it only if it exists):
      `gh label create crash --description "Time Fly didn't close normally"`.
- [ ] Repeat check 2 and click **Report problem**: the browser opens the "Crash report" form with
      the title `Crash report <session id>`, the session ID and the OS and graphics card filled in.
- [ ] Drag `crash.txt` (from **Show log**) into the Log box: GitHub attaches it.

### 5. Web (the controller runs these in a browser)

- [ ] Open the `wasm-server-runner` URL with `?diagnostics-probe=panic`; the console shows the
      panic.
- [ ] Reload without the query: an HTML overlay with **Download log**, **Report problem** and
      **Dismiss** appears before the canvas renders.
- [ ] **Download log** saves `timefly-<session id>.log` containing the crash report and the
      recent events.
- [ ] After any button, the next reload shows nothing.
- [ ] `localStorage` holds at most three sessions (`timefly.sessions`).

## Globe, time slider, camera and credits (Task 10)

Already verified automatically (2026-11-22): the slider mapping, thumb sync, idle fade, Esc
toggle, orbit step and zoom limits are unit tests; the scrub bench drove the full range both
ways on Metal and screenshots of the UI are in `docs/testing/globe-m1/ui/`. What is left is how
it feels with a real mouse and keyboard. Run `cargo run --release -p timefly`.

### 6. Loading overlay

- [ ] On launch the window shows "Loading Earth's history..." on a dark background, and it stays
      until the globe appears (no half-drawn globe or blank frame between them).
- [ ] With the data missing (rename `crates/timefly/assets/data/globe/manifest.json` for one
      run, then rename it back) the overlay shows "Time Fly could not show the globe." and the
      reason, instead of loading forever.

### 7. Orbit camera

- [ ] Dragging on the globe turns it: the surface follows the pointer left/right and up/down,
      and stops short of the poles (no flip at the top or bottom).
- [ ] Dragging that starts on the slider moves the slider, not the globe.
- [ ] Scrolling zooms in until the globe nearly fills the window (1.15 Earth radii) and no
      further; scrolling out stops with the globe small but clearly visible (6 radii).
- [ ] The globe never jitters or lags behind the drag while scrubbing through 26-30 ka (the
      bridge crossfade), i.e. both halves of the crossfade move together.

### 8. Time slider and keys

- [ ] The slider runs along the bottom with 16 px gutters; the left end reads "540 Ma", the right
      end "Present", and the label above shows the age (e.g. "21 thousand years ago").
- [ ] Drag the thumb from right to left and back: the globe changes smoothly through the ice
      age, the bridge crossfade (26-30 ka) and deep time, in both directions, with no frozen
      frames.
- [ ] Clicking on the track jumps the thumb there.
- [ ] `[` steps to the next older snapshot and `]` to the next younger one; the thumb and the
      label follow.

### 9. Idle fade

- [ ] Leave the mouse and keyboard alone: after about 3 s the slider and label fade to about
      30% opacity.
- [ ] Hovering the slider, pressing a key, scrolling or clicking brings them back to full
      opacity at once.

### 10. Credits

- [ ] `Esc` opens the credits over the globe; they list the PaleoDEMs and plate model
      (Scotese & Wright 2018, CC BY 4.0), ICE-6G_C (Argus et al. 2014, Peltier et al. 2015) and
      the sea-level stack (Spratt & Lisiecki 2016, NOAA NCEI).
- [ ] The scroll wheel scrolls the credits (and does not zoom the globe while they are open).
- [ ] `Esc` again closes them.
