# Lumen

**A fast, offline music player for Windows 10 and 11, written in 100% Rust.**

Lumen plays your local music library with bit-perfect sound and time-synced
lyrics. It has a GPU-accelerated interface built with [GPUI](https://www.gpui.rs/),
the UI framework behind the Zed editor. It never touches the network: no
accounts, no telemetry, no update checks, no recommendations. Just your files.

> Lumen was built entirely with **[Claude Code](https://claude.com/claude-code)**,
> Anthropic's AI coding agent, working from a written design brief one batch
> at a time. See [How it was made](#how-it-was-made-with-claude).

## Features

### Sound
- **Bit-perfect playback** in WASAPI exclusive mode: the device opens at each
  file's own sample rate and bit depth, bypassing the Windows mixer.
- **Shared mode** when you want other apps to play at the same time, with
  high-quality resampling (rubato) only when the rates differ.
- **Honest output badges**: Now Playing shows the real source format
  ("FLAC 24-bit / 96 kHz") and what the device receives ("WASAPI Exclusive",
  or "Shared · resampled to 48 kHz"). Lumen never resamples silently.
- **Gapless playback** for live albums and DJ mixes, including MP3 and Vorbis
  encoder delay trimming.
- **ReplayGain** (track gain), digital volume, and a fixed output rate option.
- Pausing releases an exclusive device, so other apps can play while Lumen is paused.
- Formats: **FLAC, MP3, ALAC (.m4a), WAV and Ogg Vorbis**. Decoding is pure
  Rust (Symphonia), with no FFmpeg.
- The audio thread runs at MMCSS "Pro Audio" priority behind a lock-free ring
  buffer: no dropouts while scanning thousands of files or resizing the window.

### Library
- Add folders and Lumen scans them in the background, streaming results into the
  UI. Rescans only re-read files that changed.
- **Folder watching**: added, edited, renamed and deleted files show up while
  Lumen runs. Moved files keep their play counts, playlist entries and lyric
  offset. An unplugged drive is skipped, not wiped from the library.
- Embedded cover art with a thumbnail cache, falling back to `folder.jpg` and
  similar files.
- **Multilingual**: Latin with accents, Devanagari and Malayalam all render
  correctly (bundled Inter, Space Grotesk and Noto fonts). They sort with
  real Unicode collation, not byte order.

### Browsing
- **Songs**: a virtualized table that scrolls smoothly through 10,000+ tracks,
  sortable by any column, with filter chips (Lossless, FLAC only, Synced
  lyrics, Missing lyrics).
- **Albums** and **Artists**: responsive cover grids, with album and artist pages.
- **Search** (Ctrl+F) filters as you type and ignores case and accents:
  "nandu" finds "Ñandú".
- Right-click any song: Play next, Add to queue, Add to playlist, Show in
  Explorer, Edit lyrics offset.

### Lyrics
- **Synced lyrics** from `.lrc` files next to your music, or embedded in the
  file (ID3 `SYLT`/`USLT`, Vorbis `LYRICS`).
- The active line animates and stays in view. Click any line to jump there.
- **Word-by-word highlighting** for enhanced LRC files with word timings.
- **Full-screen lyrics** (F11) with adjustable text size.
- Per-track timing offset in ±0.5 s steps, saved in the library.

### Playlists
- Your own playlists: create, rename, delete, drag to reorder.
- **Import and export .m3u8**, with relative paths, so playlists work in other players.
- **Smart playlists**: Hi-res only, Most played (top 50), Recently added
  (30 days), Missing lyrics.

### Windows integration
- Media keys and the Windows media overlay (title, artist, cover, position)
  through System Media Transport Controls.
- A custom title bar with native drag, Aero Snap and double-click to maximize.
- Single instance: launching Lumen again brings the open window forward.
- Resumes where you left off: the queue, track, position, volume, shuffle and
  repeat are restored, paused, on launch.
- Play counts that mean something: a play counts after half the track (or
  4 minutes) of actual listening.

### Keyboard shortcuts

| Key | Action |
|---|---|
| Space | Play / pause |
| Ctrl+→ / Ctrl+← | Next / previous |
| → / ← | Seek 5 s |
| Ctrl+↑ / Ctrl+↓ | Volume ±5% |
| Ctrl+F | Search |
| Ctrl+1…5 | Now Playing, Songs, Albums, Artists, Playlists |
| F11 | Full-screen lyrics |
| Esc | Back, close a menu, or leave full screen |

## Performance

Measured on the release build against the targets in the brief:

| Target | Result |
|---|---|
| Cold start under 500 ms (2,500-track library) | 402–428 ms to a visible window |
| Smooth scrolling | 10,000-row table: about 5.6 ms of CPU per frame (the 60 fps budget is 16.7 ms) |
| Near-zero CPU when idle | 0.5–0.9% of one core, which is GPUI's vsync floor; Lumen itself does no work |
| No dropouts under load | 0 underruns during a 10,000-track scan plus 60 window resizes |
| Animated lyrics | 4.4% (line-synced) and 6.1% (word-by-word) of one core |

## How it was made with Claude

All of Lumen's own code, tests, build scripts and docs were written by
**Claude**, Anthropic's AI model, running in
[Claude Code](https://claude.com/claude-code). (The vendored GPUI in
`vendor/gpui` is Zed's code; Claude added the three small patches.) The human role was product
owner and tester: writing the brief, sending each batch, trying the app on
real hardware, and reporting what was wrong.

**1. A brief first.** Before any code, a detailed design brief described
the app: hard rules (offline only, 100% Rust, bit-perfect audio, no
recommendation system), the crate architecture, every screen's colours,
type, spacing and layout, the audio and lyrics behaviour, and performance
targets. It's kept unchanged in [`docs/SPEC.md`](docs/SPEC.md).

**2. Small batches.** The work was split into eight batches. Each prompt asked
Claude to do only that batch, then stop and report what it built, exactly how
to test it, and its known issues.

| Batch | What Claude built |
|---|---|
| 0 | Workspace, pinned and patched GPUI, theme, fonts, title bar, sidebar |
| 1 | Audio engine: decoding, WASAPI exclusive/shared output, gapless queue, ReplayGain, a CLI test player |
| 2 | Library: scanner, tags, covers, SQLite with migrations, folder watching, Unicode collation |
| 3 | Songs, Albums and Artists screens, search, context menu |
| 4 | Now Playing, transport bar, resume, play counts, media keys, single instance, shortcuts |
| 5 | Lyrics parser and index, animated lyrics panel, full-screen lyrics, word highlighting |
| 6 | User and smart playlists, .m3u8 import/export, the rest of Settings |
| 7 | Performance pass with measurements, crash logs, app icon, release profile, installer script |

**3. Verify, don't assume.** Claude wrote tests alongside the code (116 of
them, including bit-exact checks that exclusive-mode output matches the
source samples) and kept `cargo clippy -D warnings` and `cargo fmt` clean
after every batch. It measured performance on the release build instead of
estimating it, and drove the running app with scripts to check behaviour.
The numbers in this README come from those runs.

**4. Real bugs, found by using it.** Testing the app on a real library turned
up problems the tests hadn't. They were reported to Claude in plain words, and
it diagnosed and fixed them:

- *"When I press Add folder nothing happens."* An old prototype's database sat
  at the same path. Lumen now stamps its database with an application id and
  safely sets a foreign file aside.
- *"The maximize button does nothing."* A focus element covered the title bar
  and swallowed its clicks.
- *"Other apps can't play sound, even when Lumen is paused."* Exclusive mode
  held the device while paused. Pausing now releases it and resumes at the
  exact sample.

**5. Working around the framework.** GPUI is young, and Claude found and
worked around its gaps. Bundled fonts didn't work as fallbacks on Windows
(fixed with a small patch in [`vendor/PATCHES.md`](vendor/PATCHES.md)), and
there is no letter-spacing, text input or menu widget, so Lumen has its own.
The full list is in [Where GPUI differs from the brief](#where-gpui-022-differs-from-the-brief).

## Install

Download `lumen.exe` (or the installer) from the
[Releases](../../releases) page. Everything Lumen needs (fonts, icons) is
compiled into the exe, so it runs on its own. Then open **Settings › Library**
and add your music folder.

## Build from source

You need Windows 10 or 11 (x64) and:

1. **Rust** with the MSVC toolchain (`rustup`, default host
   `x86_64-pc-windows-msvc`). `rust-toolchain.toml` pins 1.99.1, so `rustup`
   installs it on the first build.
2. **Visual Studio 2022 Build Tools** with "Desktop development with C++".
   That gives you the MSVC linker and the Windows SDK, whose `fxc.exe` builds
   GPUI's shaders and whose `rc.exe` embeds the app icon.
3. Optional: **Inno Setup 6** to build the installer.

```powershell
cargo build --release             # target\release\lumen.exe (fat LTO, a few minutes to link)
cargo run -p lumen-app            # debug build, with a console for logs
cargo build --profile fast        # optimised, quicker to link (thin LTO)
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

The installer:

```powershell
cargo build --release
& "C:\Program Files (x86)\Inno Setup 6\ISCC.exe" installer\lumen.iss
```

This writes `target\installer\LumenSetup-0.1.0.exe`. It installs per user by
default (the setup dialog also offers all users), adds a Start menu entry and
an optional desktop icon. Uninstalling keeps your library and settings.

### Files Lumen writes

| Where | What |
|---|---|
| `%LOCALAPPDATA%\Lumen\library.db`, `thumbs\` | Library database (SQLite, WAL) and 320 px cover thumbnails |
| `%LOCALAPPDATA%\Lumen\playback.toml` | Queue and position to resume |
| `%LOCALAPPDATA%\Lumen\logs\lumen.log` | Warnings and errors (skipped files, device problems, dropouts); about 4 MB, one older copy |
| `%LOCALAPPDATA%\Lumen\logs\panics.log` | Any crash, with a backtrace |
| `%APPDATA%\Lumen\settings.toml` | Settings, saved as soon as they change |

`LUMEN_DATA_DIR` puts all of these in one other folder, for testing or
portable use. `LUMEN_LOG_INFO=1` also logs informational messages, and
`LUMEN_PROFILE=1` prints render and sort timings to stderr.

### Audio engine test player

`lumen-play` drives the audio engine from the command line and prints the
full signal path for every track, including whether the output is bit-perfect:

```powershell
cargo build --release -p lumen-audio
target\release\lumen-play.exe --list-devices
target\release\lumen-play.exe "D:\Music\Some Album"          # exclusive mode, gapless
target\release\lumen-play.exe --shared --rg album "D:\Music"   # shared mode, ReplayGain
target\release\lumen-play.exe --help                           # all options and keys
cargo run -p lumen-audio --example make_fixtures               # test files in target\fixtures
```

## Project layout

| Path | What |
|---|---|
| `crates/lumen-core` | Shared types: tracks, sort orders, filters, settings |
| `crates/lumen-audio` | Audio engine: decoding, WASAPI exclusive/shared output, gapless queue, ReplayGain, `lumen-play` |
| `crates/lumen-library` | Scanning, tags, covers, SQLite with migrations, folder watching, collation, playlists, smart playlists, .m3u8 |
| `crates/lumen-lyrics` | LRC (standard, enhanced, offset) and embedded lyrics, with a time-to-line/word index |
| `crates/lumen-app` | The GPUI app: window, theme, screens, player bridge, media controls (the only crate that uses GPUI) |
| `assets/` | Bundled fonts, Lucide icons and the app icon |
| `licenses/` | Licences for the bundled fonts (OFL) and icons (ISC) |
| `vendor/gpui` | gpui 0.2.2 with three small patches ([`vendor/PATCHES.md`](vendor/PATCHES.md)) |
| `installer/` | Inno Setup script |
| `tools/` | Scripts that build the app icon and the static Space Grotesk weights |
| `docs/SPEC.md` | The design brief Lumen was built from |

## Pinned versions

| Dependency | Version | Why |
|---|---|---|
| `gpui` | **=0.2.2**, patched through `[patch.crates-io]` | The newest crates.io release from Zed. It builds on Windows/MSVC with the DirectX 11 renderer and supports native caption hit-testing. The patches make bundled fonts usable as fallbacks on Windows and let release builds find `fxc.exe` in any installed Windows SDK. |
| Rust | 1.99.1 | Pinned in `rust-toolchain.toml` |
| `symphonia` | =0.6.1 | Pure-Rust decoding (FLAC, MP3, ALAC, WAV, Vorbis), with gapless trimming |
| `wasapi` | =0.27.0 | WASAPI wrapper with exclusive-mode format queries and event-driven streams |
| `windows` | =0.62.2 | MMCSS, media controls (SMTC), local-time dates |
| `rubato` | =5.1.0 | FFT resampler, used only when the rates differ |
| `rtrb` | =0.4.1 | Lock-free ring buffer between the decoder and the device thread |
| `lofty` | =0.25.5 | Tags, audio properties and embedded pictures |
| `rusqlite` | =0.43.0 (bundled SQLite) | Library database; WAL so the UI reads while the scanner writes |
| `notify`, `notify-debouncer-full` | =9.0.0, =0.8.0 | Folder watching with rename pairing |
| `image` | =0.25.10 | Decoding covers and writing thumbnails |
| `icu_collator`, `icu_normalizer` | =2.4.0 | Unicode collation with storable sort keys; accent-insensitive search |
| `crossbeam-channel`, `tracing`, `serde`, `toml`, `unicode-segmentation`, `raw-window-handle`, `futures` | see `Cargo.toml` | Channels, logging, settings, text editing, window handle, async bridge |
| `flacenc` | =0.5.1 | Dev-only: writes FLAC test fixtures |

## Fonts

| Family | Weights | Source |
|---|---|---|
| Inter 4.1 | 400, 500, 600 | rsms/inter release |
| Space Grotesk | 500, 600, 700 | Static instances built from Google Fonts' variable font with `tools/make_space_grotesk_statics.py` |
| Noto Sans Devanagari | 400, 500, 600, 700 | notofonts |
| Noto Sans Malayalam | 400, 500, 600, 700 | notofonts |
| Noto Sans | 600, 700 | notofonts, for the "Noto Sans" lyrics font option |

All are under the SIL Open Font License 1.1 (see `licenses/`). Icons are
[Lucide](https://lucide.dev/) (ISC).

## Where GPUI 0.2.2 differs from the brief

| Brief | GPUI 0.2.2 | What Lumen does |
|---|---|---|
| Bundled Noto fonts as fallbacks | Fallbacks were looked up among system fonts only | Fixed with a ~30-line patch (`vendor/PATCHES.md`); check with `lumen.exe --font-check` |
| Letter-spacing on small caps labels | No letter-spacing style | Lays out each character with a gap, for short uppercase labels only |
| Variable fonts | Picking a weight through DirectWrite is unreliable | Static TTFs only |
| Search box | No text input element | `text_input.rs`, adapted from GPUI's input example: IME, grapheme-aware editing, clipboard |
| Menus and selects | No menu widget | `deferred` + `anchored` panels over a backdrop (`popup.rs`) |
| Media keys, SMTC | Not part of GPUI | SMTC through the `windows` crate directly (`smtc.rs`) |
| Taskbar thumbnail buttons | Needs `WM_COMMAND` from GPUI's window procedure | Not built |
| Keyboard focus when the focused element leaves the screen | Keys dispatch from the window root, skipping view handlers | The root view tracks its own focus and takes it back |
| Snap-layout flyout on maximize | Supported through `WM_NCHITTEST` | Native; it's a Windows 11 feature, so it doesn't appear on Windows 10 |
