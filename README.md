# vdisplay

**Run any macOS app on a virtual display of any resolution, and show it in part of your screen.**

vdisplay is a small command-line tool for macOS. It creates a virtual monitor (4K, 5K, 8K, ultrawide, or any `WIDTHxHEIGHT`), launches an app on it in native full screen, and shows that monitor live in a window on the left half, a third or a quarter of your real screen. The app believes it is running on a real display of that size, and your other windows stay where they are.

```bash
brew install adamkpn/tap/vdisplay
vdisplay run --view left "Microsoft Edge"
```

That puts Microsoft Edge in native full screen on its own virtual display, shown in the left half of your screen. Move the pointer into it and you are using Edge; move it out and you are back on the rest of your screen.

> **Status:** v0.1, early. Works with Microsoft Edge on macOS 27 (Apple Silicon). Please [open an issue](../../issues) if something misbehaves on your Mac.

## Why

- **Full screen in half a monitor.** Give a browser, IDE or video its own native full screen in half (or a third) of an ultrawide, without fighting a window manager.
- **Big screens on a small one.** Run an app at 5K or 8K on a laptop and see it scaled down, to check responsive layouts or take high-resolution screenshots.
- **A display just for sharing.** Screen-share or record one virtual display at a fixed resolution instead of your whole desktop.
- **Off-screen apps.** Keep an app running on an invisible display, out of the way.

## Features

- **Any resolution:** `WIDTHxHEIGHT` up to 16384x16384, presets (`1080p`, `1440p`, `4k`, `5k`, `6k`, `8k`, `ultrawide`, `superwide`), your screen's `native` resolution, and Retina rendering with `--hidpi`.
- **`--view`:** shows the virtual display live in a window on a half, third or quarter of a real screen. The pointer moves onto the virtual display while it is over the window, so clicks, scrolling and typing go straight to the app.
- **`--mirror`:** makes a whole physical screen mirror the virtual display, scaled to fit. macOS reverts it automatically when vdisplay exits, even after a crash.
- **Window placement:** moves the app's window onto the virtual display and enters native full screen; on exit it takes the window out of full screen and hands it back.
- **`suggest`:** reads your screen's real native resolution and suggests sizes that fit it.
- **One small binary:** universal (Apple Silicon and Intel), no runtime dependencies.

## Install

### Homebrew

```bash
brew install adamkpn/tap/vdisplay
```

### Download

Get the universal binary from the [latest release](../../releases/latest):

```bash
VERSION=0.1.0
curl -LO https://github.com/adamkpn/vdisplay/releases/download/v$VERSION/vdisplay-$VERSION-macos-universal.tar.gz
curl -LO https://github.com/adamkpn/vdisplay/releases/download/v$VERSION/vdisplay-$VERSION-macos-universal.tar.gz.sha256
shasum -a 256 -c vdisplay-$VERSION-macos-universal.tar.gz.sha256
tar -xzf vdisplay-$VERSION-macos-universal.tar.gz
sudo mv vdisplay-$VERSION/vdisplay /usr/local/bin/
```

The binary is not notarized. If you downloaded it with a browser, run `xattr -d com.apple.quarantine vdisplay` first.

### From source

Needs a Rust toolchain (`brew install rustup && rustup-init`):

```bash
git clone https://github.com/adamkpn/vdisplay && cd vdisplay
make install    # or: cargo install --path .
```

## Before the first run

1. **Accessibility:** vdisplay moves other apps' windows. Allow your terminal app (Terminal, iTerm2, Ghostty, …) in *System Settings → Privacy & Security → Accessibility*.
2. **Screen Recording** (only for `--view`): allow the same terminal in *System Settings → Privacy & Security → Screen & System Audio Recording*.
3. Restart the terminal after granting either one.
4. Turn on *System Settings → Desktop & Dock → Displays have separate Spaces*, so native full screen stays on the virtual display.
5. **Quit the browser** if you are launching Edge, Chrome or Brave. A running Chromium browser takes over a new launch and the new process exits.

## Usage

### Run an app on a virtual display

```bash
# Edge in native full screen, shown in the left half of your screen
vdisplay run --view left "Microsoft Edge"

# Same, but Edge sees a bigger screen, scaled down into the half
vdisplay run --view left -r 2580x2112 "Microsoft Edge"

# Safari in the right third, on another monitor (IDs from `vdisplay list`)
vdisplay run --view right-third --view-on 2 Safari

# A 5K virtual display mirrored onto your whole main screen
vdisplay run -r 5k --mirror "Microsoft Edge"

# Maximized instead of native full screen; app by bundle id or by path
vdisplay run -r 3440x1440 --no-fullscreen com.microsoft.edgemac
vdisplay run -r 4k /Applications/Figma.app

# Arguments after -- go to the app
vdisplay run -r 4k --new-instance "Google Chrome" -- --user-data-dir=/tmp/chrome-4k
```

The app is found by name, by `.app` path, or by bundle id. vdisplay keeps running until you press Ctrl-C or quit the app; then it takes the window out of full screen, puts it back on your screen and removes the virtual display.

### `--view`: a virtual display in part of your screen

`--view` takes `left`, `right`, `left-third`, `center-third`, `right-third`, `top-left`, `top-right`, `bottom-left` or `bottom-right`. The area stays inside the menu bar and Dock, and `--view-on <ID>` picks another monitor.

Without `-r`, the virtual display is exactly the size of the area, so the window shows it pixel for pixel; on a Retina screen it becomes a Retina display. With `-r`, the picture is scaled to fit the area.

- **Mouse:** when the pointer enters the window, it moves onto the virtual display. Push past the window's side or bottom to come back. The top edge is left alone so the full-screen toolbar can still slide down.
- **Keyboard:** typing goes to the app, as usual. vdisplay never takes keyboard focus.
- **Windows:** the viewer is a normal window; other apps can still cover it.
- **Smoothness:** the virtual display runs at your screen's refresh rate (85 Hz on an 85 Hz monitor), so scrolling doesn't judder, and the pointer is drawn by the viewer rather than captured, so it keeps up with your hand. To check, run with `VDISPLAY_STATS=1`: every 2 seconds it prints the captured frame rate, the capture-to-window delay and the longest pause in vdisplay's loop.

### Other commands

```bash
vdisplay create -r 6k --mirror   # just a virtual display, until Ctrl-C; drag windows to it yourself
vdisplay list                    # displays and their IDs
vdisplay suggest                 # native resolution of your screen and sizes that fit it
```

`vdisplay suggest` on a 34" ultrawide:

```
Display 3 (main): 3440x1440 px native, 2.39:1, 34.1" at 109 ppi
Current mode: 3440x1440 points (3440x1440 px) @ 85 Hz
Usable area (no menu bar / Dock): 3440x1409 at 0,31 points

Suggested sizes (same aspect ratio, so --mirror fills the screen):
  -r 3440x1440             3440x1440 px   1:1 with the panel when mirrored
  -r 3440x1440 --hidpi     6880x2880 px   same space, rendered at 2x: sharper text when mirrored
  -r 5160x2160             5160x2160 px   1.5x the space, downscaled when mirrored
  -r 6880x2880             6880x2880 px   2x the space, small UI when mirrored
Presets with this aspect ratio: ultrawide

Part of the screen (1:1 with that part of the panel):
  -r 1720x1440             half     left, right
  -r 1146x1440             third    left-third, center-third, right-third
  -r 1720x720              quarter  top-left, top-right, bottom-left, bottom-right
  --mirror stretches to the whole screen, so these show black bars when mirrored.
  To show a virtual display in that part of the screen: vdisplay run --view left "Microsoft Edge"
```

The native resolution is the display mode macOS marks as native, so a Retina MacBook reports its panel (e.g. 3024x1964), not the scaled "looks like" size.

### Options (`run` and `create`)

| Flag | Default | Meaning |
|---|---|---|
| `-r, --resolution` | `--view` area, else `4k` | `WIDTHxHEIGHT`, a preset (`1080p 1440p 4k 5k 6k 8k ultrawide superwide`), or `native` |
| `--view <AREA>` | off | Show the virtual display in a window on part of a real screen |
| `--view-on <ID>` | main display | Physical display for the `--view` window |
| `-m, --mirror` | off | Mirror the virtual display onto a whole physical screen |
| `--mirror-to <ID>` | main display | Physical display to mirror onto |
| `--hidpi` | off (on for `--view` without `-r` on a Retina screen) | Retina rendering: pixels = 2 × resolution |
| `--refresh` | `--view` screen's rate, else `60` | Refresh rate (Hz) |
| `--ppi` | `109` / `218` | Pixel density used for the reported physical size |
| `--name` | `vdisplay` | Display name in System Settings |
| `--no-fullscreen` | off | `run` only: maximize instead of native full screen |
| `--new-instance` | off | `run` only: launch a new instance even if the app is running |
| `--keep-alive` | off | `run` only: keep the display after the app quits |
| `--window-timeout` | `30` | `run` only: seconds to wait for the app's window |

## How it works

1. **Virtual display:** creates a real (headless) display through CoreGraphics' private `CGVirtualDisplay` API, the mechanism DeskPad and BetterDisplay also use. macOS and every app treat it like a connected monitor with exactly the resolution you asked for.
2. **Launch:** starts the app through `NSWorkspace` to get its process ID.
3. **Place:** uses the Accessibility API to move the app's window onto the virtual display, resize it, and switch it to native full screen.
4. **Show:** `--view` captures the virtual display with ScreenCaptureKit and draws it into a borderless window, moving the pointer between the window and the virtual display. `--mirror` uses an app-scoped CoreGraphics display configuration instead.
5. **Clean up:** on Ctrl-C or when the app quits, the window leaves full screen and comes back to your screen, and the virtual display is removed.

## Things to know

- **Without `--view` or `--mirror`, the virtual display is invisible.** It is an extended desktop next to your screen: useful for screen sharing or for apps you only need running somewhere.
- **Already-running apps:** vdisplay moves the app's front window and ignores launch arguments. Chromium browsers hand a new launch to the running instance; quit them first, or use `--new-instance` with a separate `--user-data-dir`.
- **Resolution limits:** very large sizes (e.g. 8K with `--hidpi`, 15360×8640 px) can exceed what your GPU accepts. `applySettings failed` means try a smaller size or drop `--hidpi`.
- **`create` and full screen:** `create` doesn't know which windows you moved onto the display. Take them out of full screen before pressing Ctrl-C, or macOS moves their full-screen Space to your main screen.
- **Private API:** `CGVirtualDisplay` is undocumented and may change in a future macOS release. Everything else is public API.

## Compared with DeskPad and BetterDisplay

[DeskPad](https://github.com/Stengo/DeskPad) and [BetterDisplay](https://github.com/waydabber/BetterDisplay) are apps that create virtual displays you manage by hand. vdisplay is a command-line tool built around one job: start a given app on a display of a given size, put it in full screen there, and show it where you want. That makes it easy to script, alias or bind to a key.

## Requirements

- macOS 12 or newer (12.3+ for `--view`), Apple Silicon or Intel.
- Accessibility permission; Screen Recording permission for `--view`.

## Development

`make` lists every target. The ones you will use most:

```bash
make tools      # once: cargo-audit + the x86_64/arm64 rustup targets
make verify     # rustfmt check, clippy -D warnings, tests, release build (same as CI)
make security   # cargo audit, Trivy, Gitleaks, and a check for home paths / e-mails
make dist       # universal binary + tarball + sha256 in target/dist
make run ARGS="suggest"
```

The Trivy, Gitleaks and actionlint targets run in Docker, so there is nothing else to install.

### Releasing

1. Bump `version` in `Cargo.toml` and commit.
2. Tag and push: `git tag v0.2.0 && git push origin v0.2.0`.

[`release.yml`](.github/workflows/release.yml) checks the tag matches `Cargo.toml`, runs `make verify` and `make dist`, publishes the tarball and checksum as a GitHub release, and updates the formula in [adamkpn/homebrew-tap](https://github.com/adamkpn/homebrew-tap). Running the workflow by hand builds the same files as a downloadable artifact without publishing.

### Project layout

```
src/
├── main.rs      CLI (clap), orchestration, Ctrl-C handling
├── vdisplay.rs  CGVirtualDisplay creation, display listing, native resolution, mirroring
├── viewer.rs    --view window: ScreenCaptureKit capture, pointer handoff
├── layout.rs    --view geometry: screen areas, fitting, pointer mapping
├── suggest.rs   Size suggestions from a native resolution
├── window.rs    Accessibility: find, move, resize, full-screen windows
├── launch.rs    NSWorkspace app resolution and launch
├── objc.rs      msg! macro (typed objc_msgSend), CFString/pool helpers, run loop
└── ffi.rs       Hand-written bindings (objc, CF, CG, AX, CoreMedia, dispatch)
```

No objc2 or core-foundation crates: the FFI surface is small, and hand-written bindings avoid their API churn.

## License

[MIT](LICENSE)
