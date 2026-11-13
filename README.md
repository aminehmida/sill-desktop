# Sill Desktop

An unofficial desktop client for [Sill](https://getsill.app): a Mac's windows, or its whole desktop, on another computer, used with that computer's keyboard and mouse.

The Mac runs the free **Sill for Mac** app (0.6.1 or later for everything here). This client speaks the same protocol as Sill's iPhone and iPad app, so the Mac needs nothing else.

> **Alpha.** It streams and takes input, but most of it has only been tested against a synthetic host and on very few machines. Features are missing and there will be bugs. It is not made or supported by Sill's author.

## Platforms

| System | Status |
|---|---|
| Mac with Apple silicon (macOS 12+) | Released |
| Windows 10/11, 64-bit Intel or AMD | Released; the picture is decoded in software so far |
| Linux | Coming: builds and decodes, not yet released |
| Intel Macs, Windows on ARM | Coming |

Get a build from [Releases](../../releases). Neither is signed with a paid certificate:

- **Mac:** unzip, move *Sill Desktop* to Applications, right-click it and choose **Open**.
- **Windows:** unzip the folder and run *Sill Desktop.exe*; on the SmartScreen notice choose **More info › Run anyway**.

## Using it

1. On the Mac, choose **Pair iPhone or iPad…** in Sill's menu. It shows an address and a 12-digit code.
2. In Sill Desktop choose **Add a Mac…**, type both, and **Pair**. A Mac on the same network is listed by itself: click it and type the code it shows.
3. Pick a window in the bar, or **Desktop**.

A window on its own fills the app's window, as if its app ran on this computer. For the bar, move the pointer to the top edge or click the small handle there; **Pin** keeps it.

Keys go to the Mac by their position, as from a keyboard plugged into it. On a PC keyboard the Windows key is ⌘ and Alt is ⌥; **Settings › Ctrl is ⌘** makes Ctrl+C copy on the Mac instead.

### What is there

Pairing and reconnecting, window and desktop streaming (HEVC), keyboard, mouse and scrolling, the Mac's pointer, the Mac's sound (AAC-ELD), the clipboard both ways, the Mac's menu bar, text size (Aa), launching apps, closing/minimizing windows, and the Mac's stream settings.

### Not there yet

Trackpad gestures, input methods (IME), reordering thumbnails, the clipboard's "Paste on Mac" chip, the first-run tour, hardware decoding on Windows, installers, updates.

## Building

Rust (stable) and FFmpeg 9 with its headers.

```sh
# macOS, for development
brew install ffmpeg pkg-config
cargo run -p sill-app

# Windows (Git Bash)
export FFMPEG_DIR=$(cygpath -w "$(scripts/fetch-ffmpeg.sh win64)")
export PATH="$FFMPEG_DIR/bin:$PATH"
cargo run -p sill-app
```

`scripts/install-macos.sh` builds and installs `/Applications/Sill Desktop.app` for this Mac. The released Mac app instead ships a small LGPL FFmpeg built by `scripts/build-ffmpeg-macos.sh`.

### Layout

| Crate | What |
|---|---|
| `crates/sill-core` | The protocol and the client: framing, pairing, TLS with pinned keys, sessions, discovery, settings, menus, the clipboard's rules. No GUI, no FFmpeg. |
| `crates/sill-media` | The picture and the sound: HEVC and AAC-ELD through FFmpeg, playout timing, audio output. |
| `crates/sill-app` | The app: winit, wgpu and egui. |
| `crates/sill-testhost` | A synthetic Sill host for tests: pairing, a looping HEVC clip, sound, menus, and a log of what the device sent. |

### Testing

```sh
cargo test --workspace
scripts/bench.sh            # the app against the synthetic host, end to end, in a real window
scripts/bench-windows.sh    # the same inside a Windows VM (bench/windows/make-vm.sh builds it, with VMware Fusion)
```

The bench pairs, streams, sends keys, a click, a scroll and a paste, takes screenshots of every screen into `bench/out`, and checks the host's log for what arrived.

## Credits and license

Sill is by [Noah Saffer](https://github.com/Saffsanity/sill). This client is a Rust port of its device side, by way of Omar Marei's [Android client](https://github.com/aminehmida/sill-android). The design follows Sill's own apps.

Apache License 2.0; see `LICENSE` and `NOTICE`. The released apps bundle FFmpeg (LGPL) as separate libraries and the Inter typeface (OFL).
