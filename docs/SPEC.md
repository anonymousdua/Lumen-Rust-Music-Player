# Build "Lumen": a Windows desktop music player in Rust + GPUI

You are building **Lumen**, a fast, stable, offline music player for Windows 10/11, written in 100% Rust with the UI in **GPUI** (Zed's GPU-accelerated UI framework). This is a fresh project. Read this whole brief before writing any code.

## Hard rules

- **Offline only.** Lumen never makes a network request: no metadata lookups, cover-art downloads, telemetry or update checks. It only plays local files.
- **100% Rust at runtime.** No FFmpeg, no C audio libraries, no web views. (`rusqlite` with the `bundled` feature is fine.)
- ~~**No recommendation system.**~~ **Overridden by Batch 8** (see the end of this file): Lumen has an on-device recommendation system, and the "For you", "Made for you" and "Recommendations" elements are back as the mockup shows them.
- **Audio quality comes first.** Bit-perfect output in WASAPI exclusive mode, no needless resampling, and no dropouts while the UI is busy.
- **The UI must match the design spec below**: colors, type, spacing, radii and layout.
- **Work only on the batch I give you.** I'll send batches one at a time (Batch 0, then Batch 1, and so on). Do only the current batch, never anything from a later one. When it's done, stop and report: what you built, how I can test it (exact commands plus what to click), and known issues.
- The project must build with `cargo build --release` on Windows (MSVC toolchain). Keep `cargo clippy -- -D warnings` clean, and use `cargo fmt`.

## Library context

- My library is roughly 2,400 tracks, mostly FLAC (16/44.1 up to 24/96), plus MP3, ALAC (.m4a), WAV and OGG Vorbis.
- Songs are in **Hindi, Malayalam, Spanish and English**, so titles, artists and lyrics contain Devanagari and Malayalam script plus accented Latin. All of them must render correctly everywhere. Sorting must handle them sensibly; use Unicode-aware collation, not byte order.
- Lyrics are mostly `.lrc` files with the same name in the same folder as the audio. Some files have embedded lyrics instead (ID3 `SYLT`/`USLT` frames, Vorbis `LYRICS` comments), and a few `.lrc` files are "enhanced" with word timings (`<mm:ss.xx>` inside lines).

## Architecture

A Cargo workspace with these crates. Only `lumen-app` depends on GPUI.

| Crate | Responsibility |
|---|---|
| `lumen-core` | Shared types (TrackId, Track, Album, Artist, Playlist, SortKey, SortDir, Filter), errors, settings model |
| `lumen-audio` | Decoding (symphonia), output (WASAPI exclusive and shared), playback state machine, gapless, ReplayGain, volume, seeking |
| `lumen-library` | Folder scanning, tag reading (lofty), embedded cover art extraction plus a thumbnail cache, SQLite DB (rusqlite), folder watching (notify), play counts, playlists and smart playlists |
| `lumen-lyrics` | LRC parser (standard and enhanced word timing, `[offset:]` tag), embedded lyric extraction, a time-to-line/word index |
| `lumen-app` | GPUI application: window, theme, screens, components, keyboard shortcuts, Windows media controls (SMTC) |

Suggested crates, which you can swap if you find something better (pin versions and give the reason):
`gpui` (pin a known-good git rev or crates.io version that builds on Windows, and record it in the README), `symphonia` (features: flac, mp3, isomp4/alac, wav, pcm, vorbis), `wasapi`, `rubato` (only for shared mode when the rates differ), `lofty`, `rusqlite` (bundled), `notify`, `walkdir`, `image` (cover thumbnails), `souvlaki` (SMTC media keys and the Windows media overlay), `serde` + `toml` (settings), `crossbeam-channel`, `tracing`, `icu_collator` or `unicode-collation` for sorting.

You may use `gpui-component` for text inputs, selects, switches, scrollbars or virtualized lists if it saves time and works with the pinned GPUI version. Restyle everything to the design tokens below so nothing looks like library defaults.

### Threading

- The audio engine runs on its own high-priority thread (use MMCSS "Pro Audio" via `AvSetMmThreadCharacteristics`) with a lock-free ring buffer between decoder and output. It must never wait on the UI or the DB.
- The UI talks to the engine through a command channel (Play, Pause, Seek, Next, Prev, SetVolume, SetQueue, and so on) and receives events (Position, TrackChanged, StateChanged, Error). Position updates arrive at about 30 Hz for smooth lyric sync.
- Library scanning and thumbnail generation run on background threads and stream results into the UI.

## Design spec

I've attached a link to the UI mockup: [Lumen – Music Player UI](https://claude.ai/artifact/M78EmUooD91gCF2tu5WAP8). If you can open it, use it as the visual reference. If you can't, everything you need is written out below, and this text is the source of truth either way.

The design is a dark "night listening" look with one amber accent. Build a `Theme` struct in `lumen-app` holding exactly these values. Never hard-code colors inside views.

### Color tokens

| Token | Hex | Use |
|---|---|---|
| `bg` | `#0B0C0F` | App background, main content |
| `bg_titlebar` | `#08090B` | Custom title bar |
| `surface` | `#0E0F13` | Sidebar, transport bar, mini player, cards, settings sections, table header |
| `surface_2` | `#14161B` | Inputs, selects, secondary buttons, info boxes |
| `surface_hover` | `#15171C` | Row and card hover |
| `button_hover` | `#1F2128` | Icon-button hover |
| `nav_hover` | `#1A1C22` | Sidebar item hover |
| `border` | `#1A1C22` | Dividers, section and card borders |
| `border_row` | `#15171C` | Table row separators |
| `border_input` | `#2A2C34` | Inputs, selects, outline buttons |
| `border_soft` | `#23252D` | Small outline buttons, badges, sidebar search |
| `track` | `#23252D` | Progress and volume track background |
| `text` | `#E8E6E1` | Primary text |
| `text_2` | `#C9C7C1` | Secondary text, inactive nav, artist names |
| `text_3` | `#A9A7A1` | Tertiary text, times, labels |
| `text_muted` | `#8C8A85` | Meta text, section labels, column headers |
| `accent` | `#F2B544` | Play button, progress fill, active nav, active lyric, sort arrows, links |
| `accent_hover` | `#FFD27A` | Link hover |
| `on_accent` | `#0B0C0F` | Icons and text on accent |
| `accent_bg` | `#1A1812` | Active sidebar item background |
| `accent_bg_2` | `#241F12` | Active chip background, hi-res badge background |
| `accent_border` | `#5A4720` | Active chip and selected playlist border |
| `playing_row_bg` | `#17150F` | Currently playing row in tables, selected playlist |
| `switch_knob` | `#F5F3EE` | Toggle knob |
| `art_placeholder` | `#1B1A17` | Cover placeholder (border `#2A2822`) |
| `lyric_past` | `#5E5C58` | Lyric lines already sung (`#4E4C48` in full-screen) |
| `lyric_future` | `#9C9A94` | Upcoming lyric lines (`#8C8A85` in full-screen) |

Placeholder art and artist avatar palette (bg / fg pairs), assigned deterministically by hashing the album or artist name:
`#1B1A17/#F2B544`, `#13202A/#7FB7D9`, `#221816/#E08A6C`, `#18172A/#A99BE8`, `#162219/#86C79A`, `#22201A/#D9C27F`, `#24161E/#E58FB5`, `#14201F/#79CFC4`, `#231B12/#E3A55A`, `#1A1B1F/#C9C7C1`.

### Typography

- **UI font:** Inter (400, 500, 600). Body 14px; meta 12–13px; section labels 11px uppercase with 0.08em letter-spacing in `text_muted`; table headers 11px uppercase with 0.06em spacing, weight 600.
- **Display font:** Space Grotesk (500, 600, 700). Used for page titles (32px, 700), the Now Playing track title (28px, 700), settings section titles (18px), the playlist title (26px, 700), all lyrics, and the "Lumen" wordmark in the title bar (14px, 600).
- **Fallbacks:** Noto Sans Devanagari and Noto Sans Malayalam, set as font fallbacks for both families so Hindi and Malayalam render everywhere, lyrics included.
- **Bundle all fonts** inside the binary with `include_bytes!` and register them with GPUI at startup. They are all OFL-licensed; include the licenses in `/licenses`. Never rely on fonts being installed.
- Times, durations, track numbers and counts use tabular figures.

### Shape and spacing

- Radii: 8px for buttons, inputs and nav items; 10–12px for cards, tables and info boxes; 14px for settings sections and large cover art; 999px for chips and badges; circles for round icon buttons.
- Icon buttons are 44×44 circles. The main play/pause button is a 52×52 accent circle. Minimum hit target is 44px.
- Heights: title bar 40, sidebar nav item 40, select/button 38, small button 32–36, chip 32, table header 40, table row 44 (Songs) or 48 (Playlists), transport bar about 80, mini player 72.
- Content padding is 32px 40px on library screens and 40px 48px on Now Playing.
- Icons are 1.5–2px stroke line icons (Lucide style). Embed them as SVG assets. No emoji.
- Hover states are instant background changes. Lyric line changes animate size and color over about 350ms with an ease curve.

### App shell (every screen except full-screen lyrics)

```
┌ Title bar (40) ───────────────────────────────────────────────────┐
├ Sidebar (240) ┬ Screen content ───────────────────────────────────┤
│               │                                                   │
├───────────────┴───────────────────────────────────────────────────┤
│ Mini player (72), or the full transport bar on Now Playing        │
└───────────────────────────────────────────────────────────────────┘
```

**Title bar.** Frameless window with a custom title bar, background `bg_titlebar` and a 1px bottom border. On the left: an amber logo mark and "Lumen" in Space Grotesk. On the right: minimize, maximize/restore and close buttons, each 46×40 with `text_3` glyphs. Close turns red on hover, as in Windows. The bar must drag the window, double-click must maximize, and Windows snap layouts (hovering maximize) should work if GPUI supports it.

**Sidebar.** 240px wide, `surface` background, 1px right border, padding 20px 12px.
1. Search input ("Search library", 38px tall, `surface_2`, border `border_soft`). Typing filters the current screen live, and Enter jumps to Songs filtered by the query. Ctrl+F focuses it.
2. "LIBRARY" section label, then nav items: Now Playing, Songs, Albums, Artists, Playlists. The active item uses `accent` text on an `accent_bg` background; inactive items use `text_2`; weight 500.
3. A flexible spacer.
4. Settings nav item, styled like the others.
5. Status box (`surface_2`, radius 10, 12px text in `text_muted`): "2,418 tracks · local only" and "Last scan 2 min ago". Use live data; while scanning, show "Scanning… 812 / 2,418".

**Mini player** (on every library screen). `surface` background with a 1px top border. Contents: a 44px cover thumbnail, title (600) and "Artist · FLAC 24/96" in muted text; clicking this area opens Now Playing. Then prev, play/pause (accent circle) and next. Then elapsed time, a progress bar (4px tall, `track` background, `accent` fill, click and drag to seek) and the total time. Then a "Lyrics" outline button that opens full-screen lyrics. Then a volume icon with a 90px volume bar.

### Screens

**1. Now Playing** (sidebar item "Now Playing")
- **Left column** (about 300px):
  - Cover art (up to 240px square, radius 14), using embedded art or the colored placeholder.
  - Title (Space Grotesk 28/700), artist (15px, `text_2`), and "Album · Year" (13px, `text_muted`).
  - Badges: the format badge first, in accent style ("FLAC 24-bit / 96 kHz", using the real bit depth and rate). Then outline badges for the output mode ("WASAPI Exclusive" or "Shared · resampled to 48 kHz") and ".lrc synced" when synced lyrics exist.
  - "UP NEXT": the next 3 queue items, each showing title, "· artist" and duration, with row dividers. Clicking one plays it.
- **Right column, lyrics:**
  - Header: "LYRICS · CLICK A LINE TO SEEK" label, a "Full screen" accent link, and on the right two small buttons, "Offset −0.5s" and "+0.5s". The offset is saved per track in the DB.
  - Lines are Space Grotesk. The active line is 34px/700 in `accent`; other lines are 24px/600 in `lyric_past` or `lyric_future`. The list auto-scrolls to keep the active line about 40% down the panel.
  - Clicking a line seeks to its timestamp. Mouse-wheel scrolling pauses auto-scroll for 4 seconds.
  - With "Word-by-word highlight" on and an enhanced LRC, the sung words in the active line fill with accent progressively.
  - Plain lyrics with no timestamps scroll as static text. No lyrics shows a muted empty state: "No lyrics found. Put an .lrc file with the same name next to the audio file."
- **Transport bar** (replaces the mini player on this screen): shuffle, prev, play/pause (52px), next and repeat (off / all / one). Inactive toggles use `text_3`; active ones use accent. Then elapsed time, a seek bar and the total time, with a time tooltip on hover. Then the volume control with a 110px bar.

**2. Songs**
- Header: "Songs" (32/700) and a subline: "2,418 tracks · 168 GB · 1,902 with synced lyrics" (live).
- On the right: a "Sort by" select (Title, Artist, Album, Duration, Date modified, Date added, Play count), a direction button ("↑ Ascending" / "↓ Descending") and a "Play all" accent button.
- Filter chips: All, Lossless, FLAC only, Has synced lyrics, Missing lyrics. The active chip uses `accent_bg_2` background, `accent_border` border and `accent` text.
- Table inside a bordered, radius-12 container. Columns: # (48px), Title (2.2fr), Artist (1.4fr), Album (1.4fr), Format (96px, a small badge), Time (72px), Modified (120px, "24 Nov 2026"), Plays (64px).
  - The header is sticky. Clicking a sortable header sorts by it; clicking again flips direction. The active column header is accent and shows ↑ or ↓.
  - The currently playing row has a `playing_row_bg` background and an accent title.
  - Double-click or Enter plays the song, using the current sorted and filtered list as the queue.
  - Right-click opens a menu: Play next, Add to queue, Add to playlist ▸, Show in Explorer, Edit lyrics offset.
  - The table **must be virtualized** and smooth at 10k+ rows. Below about 900px of width it scrolls horizontally.
- Default sort comes from Settings (date modified, newest first). The sort choice persists.

**3. Albums**
- Header: "Albums", "186 albums", a sort select (Album title, Artist, Release year, Date added, Date modified, Track count) and a direction button.
- Responsive grid of cards (min 176px wide, 20px gap). Each card shows a square cover (radius 10), the title (600, single line with ellipsis), the artist (13px, `text_3`) and "2024 · 11 tracks" (12px, muted). Card hover uses `surface_hover`.
- Clicking a card opens an album detail view: a large cover, the title, artist, year, total time, Play and Shuffle buttons, and a track list using the Songs table style sorted by disc and track number. A back arrow returns to the grid.

**4. Artists**
- Header: "Artists", "94 artists", a sort select (Artist name, Track count, Album count, Total plays, Recently played, Date added) and a direction button.
- Grid of cards (min 280px, 12px gap, `surface` bg, 1px border, radius 12, padding 14). Each card has a 64px circle avatar showing initials in Space Grotesk 22/700, colored from the palette; the name (600, 15px); "2 albums · 21 tracks"; and "412 plays · last played Today".
- Clicking a card opens the artist's view: the albums grid filtered to that artist, then all songs.

**5. Playlists**
- **Left column** (260px): "Playlists" (32/700) with a "+" button (40×40, radius 10) for a new playlist. Below it, a list of playlist buttons showing name (600) and "Playlist · 18 tracks". The selected playlist uses `playing_row_bg` background, `accent_border` border and an accent name.
  - User playlists support rename, delete, reorder tracks by drag, and import/export of `.m3u8` with relative paths.
  - **Smart playlists** (built in and read-only): "Hi-res only" (bit depth ≥24 or rate >48 kHz), "Most played" (top 50), "Recently added" (last 30 days) and "Missing lyrics".
- **Right column:** the kind label ("PLAYLIST · 18 TRACKS" or "SMART PLAYLIST · 640 TRACKS") and the title (26/700). Then "Sort by" (Custom order, Title, Artist, Date added to playlist, Date modified, Duration), a direction button and a "Play" accent button. Then the track table with columns #, Title, Artist, "Added 24 Nov" and Time (right-aligned), with 48px rows. Dragging to reorder only works under "Custom order".

**6. Full-screen lyrics** (opened from the Now Playing link, the mini player's Lyrics button or F11; Esc exits)
- No sidebar or title bar; the window goes borderless fullscreen.
- Top row: a 48px cover, the title, "Artist · Album", then "A−" and "A+" buttons (scale 0.7× to 1.3×, persisted) and an "Exit full screen" outline button.
- Centered lyrics showing 7 lines around the active one. The active line is 56px/700 in accent; the others are 32px/600. Both scale with A−/A+. Clicking a line seeks.
- Bottom: play/pause (52px) and a seek bar up to 560px wide with times.

**7. Settings** (cards in a responsive grid with min column width 380px, max content width 1100px; each card uses `surface`, a 1px border, radius 14 and padding 20)
- **Audio output:**
  - Output device select (lists real WASAPI devices plus "System default").
  - Sample rate select ("Match source file (bit-perfect)", 44.1, 48, 96 kHz).
  - Toggles: WASAPI exclusive mode ("Bypasses the Windows mixer for bit-perfect output"), Gapless playback ("No silence between album tracks"), ReplayGain ("Normalise loudness using file tags").
- **Library:**
  - Music folders list, each row showing its path and track count, with a remove (×) button on hover.
  - "Add folder" (native folder picker) and "Rescan now" buttons.
  - Default song sort select plus a direction select.
  - Toggles: "Watch folders for changes" and "Track file date modified".
  - A "Reset play counts" outline button (moved here from the removed Recommendations card) with a confirm step.
- **Lyrics:**
  - Toggles: "Load .lrc next to audio file" ("Same file name, same folder"), "Use embedded lyrics tags" ("SYLT / LYRICS tags in FLAC and MP3"), and "Word-by-word highlight" ("For enhanced .lrc files with word timings").
  - Lyrics font select: "Space Grotesk + Noto Sans fallback", "Segoe UI", "Noto Sans".
- **Toggle style:** a 48×28 pill, `accent` when on and `track` when off, with a 22px `switch_knob` knob sliding left or right. Each toggle has a label (500) above a 12px muted hint.
- Settings persist to `%APPDATA%\Lumen\settings.toml`. The DB and thumbnail cache live in `%LOCALAPPDATA%\Lumen\`.

### Changes from the mockup (required)

Batch 8 reversed the first three removals: the "For you" section, the "Made for you" playlists and the Settings › Recommendations card are back, exactly as the mockup shows them. "Reset play counts" stays in the Library card as well.

- ~~Remove the sidebar's "For you" section ("Late-night mix", "Rediscover").~~
- ~~Remove the Settings "Recommendations" card. Its "Reset listening history" becomes "Reset play counts" in the Library card.~~
- ~~Remove the "Made for you" playlists. Playlists contain only user and smart playlists.~~
- Replace the mockup's placeholder data (artist names, lyrics, counts) with real library data. The mockup's numbers only show the format.

## Behavior requirements

- **Playback:** a gapless queue, shuffle that never repeats until it has gone through everything, repeat off/all/one, and "previous" that restarts the track if more than 3 seconds have played. Resume the last track and position on launch, paused.
- **WASAPI exclusive:** open the device at the file's native rate and bit depth. If the device rejects a format, fall back to the nearest supported one, or to shared mode with a visible badge saying so. Never resample silently.
- **Shared mode:** resample with `rubato` only when the rates differ. ReplayGain applies in shared mode, and in exclusive mode only when the toggle is on (it changes samples, so the badge must say so). Volume in exclusive mode is digital volume applied before output; show a note in the volume tooltip.
- **Windows integration:** media keys and the Windows media overlay via SMTC (title, artist, cover, play state), and a taskbar thumbnail toolbar (prev, play/pause, next) if feasible. Keep a single instance: launching Lumen again focuses the existing window.
- **Keyboard:**
  - Space: play/pause
  - Ctrl+→ / Ctrl+←: next / previous
  - → / ←: seek 5s
  - Ctrl+↑ / Ctrl+↓: volume
  - Ctrl+F: search
  - Ctrl+1…5: switch screens
  - F11: full-screen lyrics
  - Esc: back or exit full screen
- **Library:**
  - The first scan streams results into the UI as they arrive. Rescans are incremental (path, size and mtime). Watching picks up added, changed, removed and renamed files and `.lrc` files.
  - Read the file mtime for "Date modified" and the first time Lumen saw the file for "Date added".
  - Count a play after 50% of the track or 4 minutes, whichever comes first.
- **Performance targets:**
  - Cold start to an interactive window under 500ms with a 2,500-track library already scanned.
  - Scrolling holds a steady 60fps or better.
  - Idle CPU stays near zero while paused; repaint only while something animates.
  - No audio dropouts while scanning or resizing.
- **Robustness:** corrupt or unsupported files are skipped with a logged warning and never crash the app. A missing file in the queue is skipped with a small toast. Panics in background threads are caught and logged to `%LOCALAPPDATA%\Lumen\logs`.

## Batch 0: do this now

1. Save this entire brief, unchanged, to `docs/SPEC.md` in the repo. Later batches will tell you to re-read it.
2. Before writing code, briefly list the GPUI version you're pinning and any spec item you think GPUI can't do as written, with your proposed alternative.
3. Build the scaffold:
   - Create the Cargo workspace with all five crates (empty stubs are fine for now) and pin GPUI.
   - Open a frameless window with the custom title bar (drag, double-click to maximize, minimize/maximize/close).
   - Add the `Theme` with every token above, and register the bundled fonts. Show a test string in Devanagari and Malayalam.
   - Add the sidebar with working navigation between empty screen placeholders, and a static mini player.

Then stop and report. Test: the window opens, moves, snaps and maximizes, and Hindi and Malayalam text renders correctly.

## Batch 8: on-device recommendations

This batch overrides two earlier rules: the "No recommendation system" rule no longer applies, and the "Changes from the mockup" removals are reversed (the "For you" sidebar section, the "Made for you" playlists and the Settings › Recommendations card come back exactly as the mockup and the design tokens describe them). Every other rule still holds: offline only, 100% Rust, no audio dropouts.

### New crate: `lumen-reco`

Depends on `lumen-core` and `lumen-library`, never on GPUI. Combines audio similarity (bliss-rs) with personal listening signals.

### 1. Audio analysis with bliss-rs

- Use `bliss-audio` with its Symphonia decoder and FFmpeg disabled; pin the version; no FFmpeg or C audio library may be linked.
- Analyze every track in a background job after the library scan, on a low-priority pool of at most `cores − 1` threads, pausable. Analysis must never cause audio dropouts; the playback thread has priority.
- Store each track's vector in SQLite (new migration) as a BLOB with the bliss feature-version number and the analysis date. Re-analyze when the file changes (mtime or size) or the feature version changes. Skip tracks that fail, log them, retry on the next rescan.
- Progress in the sidebar status box ("Analyzing sound… 640 / 2,418") and in Settings.
- Nearest-neighbour search: an in-memory index of all vectors loaded at startup, brute force (under 5 ms per query at ~10k tracks), using bliss's distance function.

### 2. Listening signals (new tables via migration)

- Play events: track, timestamp, time listened, completed or skipped, session id. A new session starts after 30 minutes of inactivity.
- A song skipped within 30 s counts as a skip; skips are negatives only when "Count skips as dislikes" is on.
- Co-play matrix: how often two tracks are played in the same session, updated incrementally.

### 3. Scoring

```
score = 0.55 · audio_similarity(seed, candidate)      // 1 − normalized bliss distance
      + 0.20 · coplay(seed, candidate)                // normalized co-play count
      + 0.15 · affinity(candidate)                    // play count and completion rate, log-scaled
      + 0.05 · same_artist_or_album bonus
      − 0.25 · skip_rate(candidate)                   // only when "skips as dislikes" is on
      − recency_penalty(candidate)                    // strong if played in the last ~2 hours
```

- Weights live in one config struct.
- Diversity: no track already in the queue; at most 2 tracks by the same artist in any 10 recommended; never the same album back-to-back unless the seed came from that album.
- Deterministic for a given state; a seeded RNG only breaks ties.

### 4. Features

- "Made for you" mixes, regenerated on launch and at most once every 6 hours:
  - Late-night mix (24 tracks): seeded from tracks often played after 9 pm, biased toward lower tempo and loudness (bliss features).
  - Rediscover (30 tracks): high past affinity, not played in 60+ days, each followed by its nearest neighbours.
- Radio / endless queue: when the queue is about to run out and Radio is on, append 10 tracks seeded from the last 3 played. A Radio toggle sits in the transport bar next to repeat (accent when on).
- "Play similar" in the Songs, Albums and Artists context menus: a 25-track queue from the selected item.
- "MORE LIKE THIS" (5 tracks) on Now Playing, under Up Next.

### 5. UI (mockup and theme tokens)

- Sidebar: the "FOR YOU" section label with "Late-night mix" and "Rediscover" links that open those playlists.
- Playlists screen: both mixes at the top, kind "Made for you · N tracks"; read-only, with "Save as playlist" to make an editable copy.
- Settings › Recommendations card: "On-device recommendations" ("Learns from your plays, nothing leaves this PC"; off hides every recommendation element and stops analysis), "Count skips as dislikes" ("Skipped within 30 seconds"), the analysis status line with "Re-analyze library", and "Reset listening history" (confirm step; clears play events and co-play data, not the audio analysis). "Reset play counts" stays in the Library card.

### 6. Tests and checks

Unit tests for scoring, diversity rules, session splitting, skip detection, the recency penalty and the Late-night / Rediscover rules; a benchmark of query time with 10k synthetic vectors (under 5 ms) and analysis throughput; `cargo tree` shows no FFmpeg or ONNX dependency.
