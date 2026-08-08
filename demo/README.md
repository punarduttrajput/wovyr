# The 90-second demo

A narrated, recordable run of [PRD-005](../docs/01-product/prd-generative-ui-runtime.md)
§9's acceptance narrative, driven over HTTP against a real `wovyr` server.

The same five beats are already proven as assertions at two layers — at the Rust
layer in [`crates/wovyr-server/src/ui.rs`](../crates/wovyr-server/src/ui.rs)
(`uc4_credential_frame_is_blocked_…` / `uc1_frame_survives_restart_…`) and at the
SDK layer in the TypeScript suite's `ui:` block. What did not exist until now is a
version a person can *watch*.

## Run it

```bash
cargo build -p wovyr-cli
node demo/killer-demo.mjs
```

No API key, no Docker, no cloud account. The driver starts its own server on a
scratch `HOME`, so it cannot read or write your real `~/.wovyr`.

| Flag / env | Effect |
|---|---|
| `--fast` | Drops every dramatic pause — ~4s instead of ~90s. Use in CI. |
| `WOVYR_BIN` | Use a specific binary instead of `target/{debug,release}/wovyr`. |
| `WOVYR_DEMO_PORT` | Bind port (default `8099`, deliberately not `8080`). |
| `WOVYR_DEMO_PACE_MS` | Per-line cadence (default `55`). |
| `WOVYR_DEMO_PAUSE_SCALE` | Multiplier on the between-beat pauses (default `3.9`). |

**The driver is also a test.** Every beat is asserted, not narrated over: if the
credential frame ever becomes pullable, if the frame id or hash drifts across the
restart, if an undeclared action stops returning `400`, if the decision's
`decided_by`/`frame_hash` don't match, or if any `prev_hash` fails to match its
predecessor, it exits non-zero and says which claim broke. A green run is
therefore evidence, and `--fast` makes it cheap enough to gate on.

## The five beats

| | Beat | Outcome |
|---|---|---|
| 01 | A poisoned agent composes a checkout frame asking for a card number | **cut** |
| 02 | The safe variant presents, content-hashed, and pends on a human | sound |
| 03 | `kill -9` the server mid-flight, then restart it | sound |
| 04 | An action the frame never declared is refused; the real approval lands | **cut** |
| 05 | The whole session is re-read from the audit chain and verified link by link | sound |

"Cut" means *policy cut something* — the vocabulary
[the landing page's design system](../website/landing/DESIGN-system.md) reserves
madder for. Beat 03 kills a process and beat 05 reports an earlier block; neither
is itself a cut, which is why the driver declares each beat's outcome explicitly
instead of letting a presenter infer it from a `✗` glyph.

## Outputs

Written to `demo/out/` on every run:

| File | What it is |
|---|---|
| `demo.cast` | [asciinema v2](https://docs.asciinema.org/manual/asciicast/v2/) capture. Timings are **measured**, never synthesized — it replays at the speed it ran. |
| `transcript.json` | The same lines plus the declared act index, for anything that wants structure rather than a terminal stream. |
| `player.html` | Self-contained replay page — see below. |

## The player

```bash
node demo/build-player.mjs      # → demo/out/player.html
```

One file, ~117 KB, no network at all: fonts are inlined as data URIs and the
capture is inlined as JSON, so it works from disk, behind a strict CSP, or
embedded on the site. It has a transport (play/pause, scrub, 1×/2×/4×), a
click-to-seek act index, and both themes. `prefers-reduced-motion` gets the
finished transcript instead of an animation to sit through.

It reads its palette and type from
[`website/landing/DESIGN-system.md`](../website/landing/DESIGN-system.md) rather
than inventing any: indigo for a sound thread, madder only where policy cut
something, Archivo and IBM Plex Mono, the 22-segment wolf mark. **The mark's path
data is duplicated by hand** in `build-player.mjs` — DESIGN-system §4 lists the
copies that must stay in sync, and this is now one of them.

## The video — `cast2video`

[`cast2video/`](cast2video/) is a small Rust tool that rasterises the cast into a
real video file. It is its own workspace root (an empty `[workspace]` table in its
`Cargo.toml`), so it stays out of `cargo build --workspace`, out of CI, and out of
the product's dependency graph.

```bash
cargo build --release --manifest-path demo/cast2video/Cargo.toml

# MP4 (h264 via ffmpeg), the shareable one
demo/cast2video/target/release/cast2video demo/out/demo.cast --out demo/out/demo.mp4

# GIF — pure Rust, no ffmpeg, idle-capped so 89s of reading pauses becomes ~23s
demo/cast2video/target/release/cast2video demo/out/demo.cast --out demo/out/demo.gif \
  --font-size 12 --fps 12 --idle-cap 0.9

# PNG sequence — also ffmpeg-free, for frame-by-frame inspection
demo/cast2video/target/release/cast2video demo/out/demo.cast --out demo/out/frames/
```

`--help` lists the rest (`--theme light`, `--speed`, `--font`, `--crf`, …).

### What it does itself, and what it delegates

The text pipeline is entirely self-contained — **a from-scratch TrueType reader
and scanline rasteriser**, a small terminal model, and a painter. That is not
gold-plating: this host pins `[net] offline = true` and no font crate
(`fontdue`, `ab_glyph`, `ttf-parser`, `swash`) is in its registry cache, so there
was nothing to depend on. `src/font.rs` reads `glyf`/`loca` outlines including
composite glyphs, `cmap` formats 4/6/12, and rasterises with nonzero winding,
4× vertical supersampling and analytic horizontal coverage.

GIF and PNG output are pure Rust, so the tool never *requires* ffmpeg. MP4 does
pipe raw frames to `ffmpeg` — encoding H.264 in-process would mean vendoring an
encoder, and shelling out to a platform tool is the same pattern `wovyr-tools`
already uses for `iptables`/`nsenter` and `icacls`.

Deliberately **not** implemented: hinting, kerning, `CFF `/OpenType outlines,
variable-font axes, and cursor addressing / alternate screens / scroll regions in
the terminal model. A `CFF `-outline font is rejected with a clear message rather
than rendering blank glyphs, and unhandled escape sequences are skipped rather
than printed.

### Two things it caught

Both were real defects, found because the tool reports rather than guesses:

- **`CascadiaMono` has no `✗` (U+2717)** — the glyph that marks every denial, and
  the most semantically important mark in the video. It would have rendered as a
  blank cell. Fixed with real font fallback (`--fallback`, defaulting to the
  platform symbol font); `--probe` reports coverage without rendering:

  ```bash
  demo/cast2video/target/release/cast2video demo/out/demo.cast --probe
  # → 89 distinct characters, 88 primary, 1 fallback, 0 missing
  ```

- **Six lines overflowed the 92-column grid** and folded back to column 0,
  wrecking the indentation. Invisible in the wide terminal the capture was taken
  in; obvious at 30fps. Fixed in the driver (which now wraps with a hanging
  indent) and guarded — every run reports any line that would wrap on replay.

### Frame reuse

A narrated recording is mostly deliberate pauses, so the renderer repaints only
when the grid actually changes: the 90-second MP4 is 2,725 frames but only ~107
paintings, which is why it encodes in about 30 seconds.
