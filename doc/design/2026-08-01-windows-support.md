# Assessment: native Windows support

Status: **implemented (pending hardware/VM QA)** (branch `experimental-windows-support`).
Scope: Windows 10 1607+ / Windows 11, native (not WSL), x86_64, MSVC ABI.

## TL;DR

kyoku has **no dependency-level blockers** for native Windows — every crate in
the tree is already cross-platform, and the platform-specific surface is small
and already partly `cfg`-gated. The work is surgical, not architectural:
a Windows player-detection table, an `explorer.exe` branch for "open dir",
a config-path decision, a handful of NTFS-semantics fixes, and release/dist
plumbing. Estimate: **2–3 focused days for a solid MVP**, plus optional
packaging extras (scoop/winget). The main compromises on Windows are
file-locking semantics, path-length limits, and cover-preview availability
being terminal-dependent (same degradation story as on Unix, slightly worse
median terminal).

## 1. What already works / is ready

Dependency audit (`Cargo.toml`):

| Crate | Windows status |
|---|---|
| ratatui + crossterm | First-class: raw mode, alternate screen, truecolor, resize, mouse, wide/CJK chars. Windows Terminal is crossterm's best-supported backend host. |
| ratatui-image 10 | Windows querying works since late 2024 (PRs #50 "query stdio capabilities on Windows", #52 "Windows reorganize"; fix for WT sixel detection in #37 — all closed/completed). Remaining known gap: ConPTY-wrapped contexts may swallow query responses (open #164) → falls back to halfblocks, which kyoku already treats as "no preview". |
| rusqlite (bundled) | Compiles with MSVC/clang-cl; needs a C toolchain only when *building on* Windows (contributors/CI provide it; prebuilt users don't care). |
| reqwest + rustls | Pure-Rust TLS — the deliberate rustls choice pays off here; no OpenSSL/Schannel work. |
| lofty, image (pure-Rust decoders), walkdir, strsim, unicode-width, serde/toml, thiserror/anyhow, tracing | Pure Rust, no platform code. |
| inquire 0.9 | Crossterm-based; setup wizard works on Windows. |
| dirs 6 | Maps to `%APPDATA%` / `%LOCALAPPDATA%` correctly. |

Code that already anticipates Windows:

- `src/core/player.rs`: `is_executable` has a `#[cfg(not(unix))]` stub; `Os` is
  an enum with exhaustive matches — the compiler will point at every match arm
  that needs a Windows arm, which is a feature.
- `src/tui/app/render.rs` + `src/tui/views/collections.rs`: status-bar key
  lists and the `o` binding are already `#[cfg]`-split (currently *disabling*
  open-dir on Windows rather than implementing it — to be finished, see §3).
- `src/core/template.rs::sanitize_path_component` already replaces the full
  Windows-invalid set `/ \ : * ? " < > |` and trims trailing dots/spaces, and
  the 255-byte component cap matches NTFS limits. Filename sanitization is
  already Windows-shaped.
- `src/core/organizer.rs::move_file` already falls back to copy+delete on
  `CrossesDevices` — covers C:→D: moves identically.
- Tests that touch Unix specifics are already gated: `tests/play_e2e.rs`
  (`#[cfg(unix)]` chmod), `src/core/paths.rs` symlink test, `src/core/importer.rs`
  non-UTF8-path test.
- Console text: on Windows, Rust std writes to consoles via `WriteConsoleW`
  (UTF-16), so Japanese output is mojibake-free regardless of the console
  codepage; crossterm input uses `ReadConsoleInputW`. No CP932/CP65001 work
  needed.
- Multiplexers: tmux/zellij don't exist natively on Windows (WSL only), and
  Windows Terminal panes are not escape-sequence muxes — the existing
  `TMUX`/`ZELLIJ` graphics gate keeps working and never false-positives.

Glyph use in the TUI is limited to `✓` and `▶` (covered by Windows font
fallback via Segoe UI Symbol) — **no Nerd Font requirement** for a clean UI.

## 2. How it renders in Windows Terminal (and others)

Base UI (ratatui widgets, status bars, themes): pixel-identical to Unix.
Windows Terminal is a full VT host: 24-bit color, CJK wide chars with proper
wcwidth behavior, box drawing, alternate screen, bracketed paste; GPU-rendered,
so scroll perf is fine. IME: WT's CJK IME compose-in-place has had active bug
fixing in the 1.24/1.25 lines (relevant for the global search).

Cover-art preview per terminal (driven by `Picker::from_query_stdio`, which in
v10 works on Windows):

| Host | Protocol | kyoku behavior |
|---|---|---|
| Windows Terminal stable (1.22-preview introduced sixel; current stable ships and hardens it — sixel crash/alloc fixes appear in the 1.24/1.25 stable notes) | **sixel** ✓ | Preview works. |
| WezTerm (native Windows builds) | **iTerm2** ✓ (upstream matrix rates it the most bug-free WezTerm path) | Preview works. |
| Rio | iTerm2/sixel ✓ | Preview works. |
| CF integrated terminals (VS Code, JetBrains) and other ConPTY intermediaries | query responses may not round-trip (open upstream issue ratatui-image#164) | `from_query_stdio` errors or returns halfblocks → kyoku disables the preview slot silently (existing graceful path). |
| Legacy conhost / bare console | no graphics | No preview, everything else fine. |

Also verify at QA time: `alacritty` on Windows (no sixel → no preview, fine).
Worst case everywhere is "preview slot disappears", never garbage — that
fallback is already the design.

## 3. Work items

Ordered, with the files they touch. Roughly 250–400 new/changed lines total.

### 3.1 Player detection & hand-off (`src/core/player.rs`) — the biggest item

- Add `Os::Windows`. `RealProbes::os()` gets a `cfg!(windows)` arm. Because
  `resolve_player`'s matches are exhaustive, the compiler lists every site.
- **Fix `which()` for PATHEXT.** On Windows, `mpv` lives on PATH as
  `mpv.exe`; the current `dir.join(bin).is_file()` probe never matches.
  Windows variant: iterate `PATHEXT` (`.EXE;.CMD;.BAT;…`) suffixes, plus the
  bare name. Keep the executable-bit check Unix-only (already stubbed).
- New `WINDOWS_PLAYERS` detect table. Two detection modes are needed, because
  the popular Windows players mostly are **not on PATH**:
  - PATH binaries (scoop/choco/manual): `mpv --playlist={playlist}`,
    `vlc {playlist}`.
  - Well-known install dirs (extend `DetectEntry` with an absolute-path probe,
    e.g. a `path: Option<&str>` field expanded against `%ProgramFiles%` /
    `%ProgramFiles(x86)%`): foobar2000 (`foobar2000.exe {playlist}` — loads
    m3u8), VLC (`VideoLAN\VLC\vlc.exe`), MusicBee, AIMP. Registry
    `Uninstall` key probing is possible but needs a `windows-sys` dep —
    skip for v1.
- Default-handler fallback: **`explorer.exe {target}`** (opens the Shell
  association for .m3u8/audio; `spawn()` is success-only so explorer's quirky
  exit codes don't matter, and — unlike `cmd /c start` — it doesn't flash a
  console window, and dodges `start`'s "first quoted arg is a title" quoting
  trap).
- `[player].app` (`open -a`) stays macOS-only; on Windows the equivalent is
  `[player].command` (docs/settings comment update).
- M3U8 with backslash paths is accepted by mpv/VLC/fb2k; the existing
  newline/comma `filter_playable` rules stay valid. E2E: extend
  `tests/play_e2e.rs` with a `.cmd` fake player (or gate the file to unix and
  add a Windows twin via `tests/play_e2e_windows.rs`).

### 3.2 Open-directory (`src/tui/views/detail.rs`, `collections.rs`, `render.rs`)

- Add a `#[cfg(target_os = "windows")]` arm to `open_directory`:
  `explorer.exe <path>`, spawn fire-and-forget.
- Remove the existing cfg-splits that *hide* the `o` binding/hints on Windows
  (they were placeholders for exactly this).

### 3.3 Config / data / cache locations (`src/config/paths.rs`)

Compiles unchanged today (`%USERPROFILE%\.config\kyoku` + dirs-based data/cache
in `%APPDATA%` / `%LOCALAPPDATA%`). Decide and document one of:

- **(a) Keep XDG-style `~/.config\kyoku` on Windows** — consistent with the
  existing "one portable XDG path" rationale (and matches some TUI tools).
  Downside: mildly un-Windows-y; a `~/.config` dir in `%USERPROFILE%`.
- **(b) `dirs::config_dir()` on Windows only** → `%APPDATA%\kyoku`, matching the
  Nicotine+ convention this codebase already reads (`setup.rs` uses
  `dirs::config_dir()` for Nicotine+ detection — which on Windows resolves to
  `%APPDATA%\nicotine`, so detection just works).

Either is a one-line change plus README/`kyoku paths` note; (a) is zero code,
(b) is more native. No strong technical constraint — pick per taste.
Nicotine+ inbox detection and `default_music_dir` (`<home>\Music`) already do
the right thing on Windows.

### 3.4 NTFS / filesystem semantics

- **Reserved device names**: `sanitize_path_component` doesn't guard
  `CON, PRN, AUX, NUL, COM1–9, LPT1–9` (case-insensitive, incl. `NUL.flac`-
  style stems). An artist/album named "AUX" currently produces un-creatable
  paths on Windows. Fix in `template.rs`: map reserved names to `name_` —
  unconditional (also protects libraries synced to Windows later).
- **Case-insensitive collisions**: the organizer's occupied-path
  `HashSet<String>` (`mark_occupied`) is case-sensitive; NTFS (default
  case-insensitive) can't hold `Track.mp3` and `track.mp3` side by side, so
  collision detection must fold case — same problem class as the existing
  NFC/NFD handling. Add `to_lowercase()` variants to the occupied set.
- **File locking (documented compromise)**: Windows denies move/rename/delete
  of files a player has open. Unix kyoku happily moves a file mpv is playing;
  Windows surfaces `AccessDenied` in the organize/delete result. No code fix —
  note in the troubleshoot docs: "close the player and retry".
- **Long paths**: total-path (>260 chars) operations fail on stock Windows —
  plausible with verbose CJK album titles in `{album_artist}/{album} ({year})/…`
  trees (component cap is fine; *total* path isn't). Per MS docs, opting in
  needs both the `LongPathsEnabled` registry/GPO value (admin, machine-wide)
  **and** a `longPathAware` manifest. Options: ship the manifest via a
  `build.rs` + `winresource`/`embed-resource` build dep (one-time, ~10 lines —
  helps only users who flipped the registry bit; harmless otherwise), plus a
  troubleshooting note. MVP can ship docs-only.
- **`canonicalize()` returns verbatim `\\?\C:\…` paths**: both `to_db_path`'s
  canonical fallback and `mark_occupied` compare canonical-to-canonical, so
  equality stays correct; just ensure no canonicalized path is ever *stored*
  in the DB or substituted into an argv/notice — currently neither happens
  (fallback stores `path.display()` of the *original*). Add one Windows unit
  test to lock that invariant.
- Non-UTF8 filenames: effectively impossible via Rust APIs on Windows (wide
  APIs everywhere; the scanner's `skipped_non_utf8` class mostly disappears).
  The Unix-only test stays Unix-only.
- `strip_prefix` is case-sensitive: `c:\Music` vs `C:\Music` mismatches degrade
  to storing absolute paths (correct, just less portable). Normalize
  `music_dir` once at load (`canonicalize` on Windows where it exists) if this
  proves noisy in practice.

### 3.5 Terminal/UX details

- In legacy conhost, `Ctrl+S` (save keybinding) is safe — Windows consoles
  don't do XON/XOFF flow control.
- `Ctrl+C` arrives as a key event in raw mode (no SIGINT semantics) — kyoku
  already handles it explicitly; behavior identical.
- Panic hook + alternate screen restore work as-is (crossterm).

### 3.6 Verification

`cargo check --target x86_64-pc-windows-msvc` can't run end-to-end from this
Linux box (no Windows C toolchain for `ring`/`libsqlite3-sys` build scripts) —
do the first real compile on a Windows machine or with `cargo xwin check`.
Expected fallout beyond §3: near-zero; every platform seam found has a cfg
already. For CI: add a `windows-latest` job (`cargo test`); today there is
only `release.yml`, no CI workflow at all — worth adding even pre-Windows.

## 4. Compromises on Windows (feature parity ledger)

| Area | Unix/macOS | Windows | Verdict |
|---|---|---|---|
| TUI chrome, CJK, themes | ✓ | ✓ identical in WT/WezTerm | full parity |
| Cover preview | kitty/iTerm2/sixel terminals | sixel (WT stable) / iTerm2 (WezTerm); none on conhost/IDE terminals | parity where terminals cooperate; degrade = silent, by design |
| Play hand-off | mpv/VLC/… PATH + `open -a` + xdg-open | mpv/VLC PATH + Program Files probes + `explorer.exe` | parity w/ slightly messier detection |
| Open dir | `open` / D-Bus FileManager1 | `explorer.exe` | parity |
| Moving open files | allowed | blocked by file locks | **platform compromise** (error surfaced, retry) |
| Path length | ~unlimited in practice | 260 unless longPathsEnabled+manifest | **platform compromise** (mitigatable) |
| Case/Unicode collisions | NFC/NFD quirks | case-insensitive quirks | equal-but-different; needs the §3.4 fix |
| Nicotine+ inbox detect | ✓ | ✓ (`%APPDATA%\nicotine`) | parity |
| Symlink-heavy setups | fine | fine for *reading*; kyoku never creates symlinks | parity |
| WSL interop | — | Native build must *not* be pointed at `\\wsl$` trees as `music_dir` (perf is bad over 9P) | doc note |

No feature needs to be *removed* on Windows. The current branch's approach of
hiding `o` was scaffolding, not a permanent cut.

## 5. Distribution

Packaging & channels, cheapest first:

1. **GitHub Releases zip (day one).** Extend `release.yml`'s matrix with
   `x86_64-pc-windows-msvc` on `windows-latest` (`can_run: true` — native smoke
   test of `kyoku.exe --help`). Package as `.zip` for Windows
   (`tar -a -c -f $STAGING.zip $STAGING` in bash, or `Compress-Archive`) while
   keeping `.tar.gz` for unix. Static-link the CRT
   (`RUSTFLAGS=-C target-feature=+crt-static`) so the exe runs on machines
   without the VC++ redistributable. Optional later:
   `aarch64-pc-windows-msvc` (x64 emulation already covers ARM machines).
2. **crates.io + `cargo install kyoku --locked`.** Trivial for Rust users.
   Note: the repo has a second `[[bin]]` (`create_fixtures` under tests/) —
   exclude it from publishing (`[[bin]] ... path` bins all get installed) or
   document `--bin kyoku`.
3. **Scoop.** One JSON manifest in a personal bucket (`scoop bucket add kyoku
   <url>`; `scoop install kyoku`) — 30 minutes of work, the idiomatic channel
   for Rust CLIs on Windows. The official `main` bucket has notability
   criteria; go personal-bucket first.
4. **WinGet.** Submit a manifest PR to `microsoft/winget-pkgs` (portable-zip
   install type supported): free, automated validation + human review; the PR
   is a yaml file pointing at the GH Release. Good discoverability to
   non-Rust users; do it once the Windows build has survived a release or two.
5. **Code signing / SmartScreen.** Unsigned exes get a one-time Defender
   SmartScreen "unknown publisher" prompt on first run — the exact parallel of
   the existing macOS Gatekeeper note. Recommended: document the bypass in the
   README (same `$ xattr…`-style one-liner, e.g. PS `Unblock-File`), revisit
   signing (Azure Trusted Signing ~$9/mo; OV certs are $$$/hardware-token now)
   only if a real user base appears.
6. **Skip:** Chocolatey (declining, redundant with Scoop/winget),
   MS Store/MSIX (wrong shape for a TUI).

README changes on release: replace "runs on Windows via WSL" with the native
support statement + a small terminal/cover-preview matrix; add Windows to the
auto-detect player tables section; SmartScreen note next to the Gatekeeper note.

## 6. Effort estimate

| Chunk | Size | Time |
|---|---|---|
| 3.1 player (PATHEXT `which`, table + abs-path probes, explorer fallback, tests) | ~150–200 LoC + unit tests via existing `FakeProbes` | 1 day |
| 3.2 open-dir + un-hide keys | ~30 LoC | 1 h |
| 3.3 config-path decision + docs | 1 line + README | 1 h |
| 3.4 NTFS fixes (reserved names, case-fold occupied set, canonicalize invariant test) | ~80 LoC + tests | half day |
| 3.6 first real Windows compile + fixes | unknown but expected small | half day buffer |
| Manual QA on real hardware/VM: setup wizard, import, organize, play, locking note, WT sixel + WezTerm cover checks | — | half–1 day |
| 5. release matrix + zip + crt-static | ~30 yaml lines | half day |
| scoop/winget/crates.io (optional, post-MVP) | manifests | half day + review latency |

**MVP ≈ 2–3 focused days.** No architectural changes; the enum-exhaustiveness
and existing cfg seams make this a compiler-guided port.

## 7. Open questions / follow-ups for implementation

- Config-dir choice: (a) XDG-everywhere vs (b) `%APPDATA%` on Windows (§3.3) —
  needs a product taste call; (a) is zero-code.
- Player table contents: initial set proposal = mpv, VLC, foobar2000 (+ PATH
  `musikcube`?). Verify fb2k/MusicBee CLI syntaxes against their docs when
  writing the table (same "syntax verified against man pages/docs" discipline
  as the Linux table in §13 of the external-player design doc).
- Whether to add the `longPathAware` manifest in v1 of Windows support or defer
  (§3.4); also whether to statically link the CRT for the release zip.
- Shipping a Windows CI job implies this repo gets its first `ci.yml` —
  opportunity to also run `cargo test` on the existing unix targets.

## 8. Decisions log (2026-08-01, owner)

All resolved from §7, implemented on `experimental-windows-support`:

1. **Config dir** — (a) keep `~/.config\kyoku` on Windows (zero code).
2. **Player table v1** — mpv + VLC (PATH), foobar2000 + VLC (%ProgramFiles%
   probes), `explorer.exe` fallback. MusicBee/AIMP/musikcube deferred
   until requested.
3. **`longPathAware` manifest** — yes, in v1 (`build.rs` + `embed-resource`,
   no-op on non-Windows hosts).
4. **Static CRT** for the release exe — yes (`+crt-static` in release.yml).
5. **arm64 build** — skipped for v1 (Prism emulation covers ARM PCs).
6. **Distribution** — GH Release zip + README/SmartScreen note only;
   crates.io/scoop/winget deferred until the Windows build has survived a
   release.
7. **CI** — new `ci.yml`: `cargo test` on ubuntu/macos/windows +
   clippy. `cargo fmt --check` deliberately not gated yet (tree is not
   rustfmt-clean on main — separate format commit if desired).

### Verification status

- All new logic is unit-tested host-independently (FakeProbes Windows arms,
  pure `pathext_candidates`, reserved-name sanitize; case-folding tests run
  on the windows CI job: `occupied_set_matches_case_folded_spellings`).
- Full Windows-target compile verified from Linux via zig toolchain shim
  (`cargo zigbuild --target x86_64-pc-windows-gnu` — `cfg(windows)` is ABI-
  independent for our code; MSVC CI/release runners do the authoritative
  MSVC build).
- **Still required before release**: Tier-3 interactive QA in a Windows VM
  (setup wizard, import, organize, play hand-off, file-locking error UX,
  WT-sixel and WezTerm cover previews) — none of that is exercisable CI-side.
- Tracked follow-ups: `.cmd`-based spawn e2e on Windows (shell-script e2e
  is unix-gated); registry-based player detection (`windows-sys`) if the
  two fixed install-dir probes prove insufficient; MusicBee/AIMP entries.