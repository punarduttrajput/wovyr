# Design System — Wovyr Landing

**Status:** Adopted 2026-07-31, shipped at `/` (`website/src/pages/index.astro`)
**Supersedes:** the cobalt / mono-forward / "Trust Gate" system this file described
until 2026-07-31. That direction is retired — see §8 for what still runs on it.

---

## 1. Design thesis

Wovyr's name is the design. *Wovyr weaves* — an interface generated at runtime is
cloth, produced thread by thread for one person, and Wovyr is the loom: it checks each
thread against policy, cuts the ones that fail before they reach the web, marks the
selvedge, and holds the work on the beam until a human decides.

The page is therefore a **plate from a weaving atlas**. A warp grid runs behind every
section; content is organised into numbered plates because an atlas genuinely is
ordered; and the palette is the two classical dyes — **indigo** for a sound thread,
**madder** for one that was cut.

Boldness is spent in exactly one place: the hero figure, a **wolf's head woven from
weft threads of light** that assembles row by row, the way cloth comes off a loom.
Everything else stays quiet, ruled, and technical.

This is deliberately none of the current AI-landing defaults: no warm cream with a
serif display and terracotta accent, no purple-to-blue gradient hero, no acid-green
pop, no Inter or Space Grotesk, no emoji section markers, no everything-centered, and
no particle mesh. The weaving vocabulary is derived from the product's own name, which
makes it the one thing a competitor cannot lift without also taking the name.

## 2. Colour

Cool paper, not cream — the neutral is chosen with a slight blue-grey bias so it never
reads as the generic warm-cream template. Light is the primary theme; dark gets equal
care rather than a naive inversion.

| Token | Light (primary) | Dark | Role |
|---|---|---|---|
| `--paper` | `#DEE3E7` | `#0C1115` | page ground |
| `--paper-2` | `#EBEEF0` | `#131920` | raised panels, callouts |
| `--sunk` | `#D2D9DE` | `#080B0E` | terminal / inset wells |
| `--ink` | `#0E141A` | `#E7EBEF` | primary text |
| `--ink-2` | `#53616D` | `#8B98A3` | secondary text, annotations |
| `--warp` | `#C3CCD3` | `#1D252C` | the warp grid, hairline rules |
| `--warp-2` | `#AEB9C2` | `#2C3641` | stronger rules, borders |
| `--indigo` | `#27386B` | `#8AA2E8` | **a sound thread** — links, CTAs, the figure |
| `--indigo-soft` | `#8FA0CE` | `#3C4C7E` | secondary warp threads in the figure |
| `--madder` | `#A63A26` | `#E5735C` | **a thread policy cut** — semantic only |

**Madder is never decorative.** It appears only where something was denied: the cut
thread in the hero, the cut row in the Plate 04 band, the counter under Fig. 1, and the
left rule on the candour callout. It must not be used as a second accent, a hover
state, or a highlight. The four segments of the Plate 03 shuttle pass are all indigo
for exactly this reason — that sequence is the sound path.

Source of truth: the `:root` blocks in `website/src/pages/index.astro`. The OG card
generator (`website/scripts/generate-og-image.mjs`) duplicates the dark values by hand,
because it runs standalone under `sharp` before Astro's pipeline exists — **if the
palette changes, change it in both places.**

## 3. Typography

Two families, three roles.

- **Display and body — Archivo Variable** (`@fontsource-variable/archivo`). A real
  variable font, so headings at 700 and running text at 400 come from one download.
  Headings are tight: `letter-spacing: -.021em`, `line-height: 1.06`, `text-wrap:
  balance`. A technical grotesque set at poster weight, not a neutral delivery vehicle.
- **Annotations — IBM Plex Mono 400/500** (`@fontsource/ibm-plex-mono`, latin subset).
  Plate numbers, figure captions, eyebrows, buttons, the terminal, the footer base.
  Always uppercase with `letter-spacing: .13em` at 11px. These are *plate annotations*,
  the marks a draftsman leaves on a technical drawing.
- **Data — the same mono with `tabular-nums`** on every count, version and figure
  number, so digits line up in a column.

Both faces are self-hosted via `website/src/styles/landing-fonts.css`, which is
imported by the landing page **only**. It is deliberately not merged into
`src/styles/fonts.css`, which Starlight loads on every docs page and the dashboard
reads directly from `node_modules` — see §8.

## 4. The mark

A forward-facing **wolf's head** cut from flat planes: 22 straight segments on a
100 × 100 grid, symmetrical about x = 50, ears swept back, the ruff wider than the
ears. One path, one ink, no curves — so it prints in a single colour and holds at
16 px.

The same geometry is rendered in different **materials** depending on surface:

| Material | Where |
|---|---|
| **Solid** — one filled path | nav, footer, favicon, OG card wordmark |
| **Woven** — weft threads clipped to the silhouette | the hero figure, the OG card figure |

Canonical sources: `website/public/favicon.svg` (solid, on a dark tile — a bare
silhouette loses its edge against light browser chrome at 16 px),
`website/landing/assets/wovyr-logo.svg` (the wordmark lockup), the inline path in
`index.astro`, and `website/src/assets/brand/logo-{light,dark}.svg` (the docs-site
header lockup). All five carry the identical path data and are kept in sync by hand.

The docs header lockup keeps its **JetBrains Mono** wordmark rather than Archivo: the
docs site self-hosts that face and has not been migrated (§8). The mark is unified
across every surface so the site never shows two different logos; the lockup's typeface
follows whichever system its own surface is on until the migration lands.

**No outline and no eyes.** The hero figure is not stroked and carries no eye shapes:
the weft rows terminate exactly where the silhouette terminates, so the head is read
from *where the threads end*. This is a deliberate constraint, not an omission — it is
what makes the figure feel woven rather than drawn.

The wordmark is always lowercase — Wovyr is a runtime you type, not an institution you
address. Clear space equals the height of the ears.

**Open question:** the eyes are currently removed from the small solid mark as well as
the figure. Those two notches are what let the shape read as a wolf rather than a crest
below about 24 px. Reinstating them *only* at small sizes is unresolved.

## 5. Layout & components

- **Warp grid.** `repeating-linear-gradient(90deg, var(--warp) 0 1px, transparent 1px
  46px)` on `body::before` at 50% opacity, behind everything. It is the structural
  device the whole system hangs on.
- **Plates.** Each section is a numbered plate (`Plate 03 — The pass`) with a rule that
  runs to the right edge. Numbering is used **only** where order is real: the plate
  sequence itself, and the four steps of the shuttle pass. Nowhere else.
- **Selvedge.** The hero copy column carries a dashed left rule (`.sel`) — the finished
  edge of the cloth.
- **Rules, not cards.** Content is separated by 1 px hairlines and generous space, not
  boxed in rounded cards. The only filled surfaces are the terminal well, the Plate 04
  band, and the candour callout.
- **Registers.** The eight engines are a ruled register (`44px | .62fr | 2fr`), not a
  grid of tiles.
- **Figure captions.** `Fig. 1 — 42 weft · 1 thread cut`. The counts are **real**: the
  weft count is the number of rows the scanner actually produced, and the cut count
  increments each time the animation severs a thread. Structure that encodes true
  content, not decoration.
- **Colophon** (`.colo`, Plate 07). A two-column ruled spec sheet — label left in mono
  caps, value right in mono — listing where the project actually lives: licence, repo,
  npm and PyPI package names, contribution terms, version. The value column is mono
  because every entry is a literal you can paste. Vanity metrics are deliberately
  absent: there is no star or download count, because a zero helps nobody and a number
  that only moves up is not information.

## 6. Motion

One orchestrated moment, then restraint.

- **The hero weave.** Weft rows converge from scatter and settle top to bottom on a
  0.035 s per-row stagger, then breathe on a slow sine. Every 3.6–6.8 s policy **cuts**
  one thread: it turns madder, opens a gap, and re-weaves. Only rows that cross the head
  in one unbroken run are eligible — on a row already split by the cheek notches, the
  break reads as dashes rather than as one thread being severed.
- **Scroll reveal.** Sections fade and rise 12 px once on enter, via
  `IntersectionObserver`.
- **Hover.** Border and colour transitions at .18 s. No lifts, no scale.
- **`prefers-reduced-motion`.** Reveals resolve immediately and the figure renders one
  settled frame with no assembly, no breathing and no cuts.

**Performance note:** the figure applies `shadowBlur` to the thread *stroke* only. An
earlier revision also shadowed every dot — roughly 1,000 shadowed fills per frame — and
that was the single dominant cost in the render with nothing visible to show for it.
Do not reintroduce it.

## 7. Voice

Plain, concrete, and candid. Name things from the reader's side of the screen. Prefer
the specific claim to the clever one.

Candour is a stated pillar, not a tone: the page says outright that Wovyr ships as a
single-node appliance and that the distributed scheduler is tested library code not yet
in the binary. For a security product that is a credibility move, and it must survive
future copy edits.

**Contact:** `contact@wovyr.com`, linked from the footer, the footer's Project column,
and the closing CTA.

## 8. Surfaces still on the retired system

The landing page is the only surface migrated. Flagged plainly rather than quietly
left inconsistent:

- **`packages/tokens/wovyr-tokens.css`** still defines the cobalt palette and is the
  canonical token source consumed by the Starlight docs config *and* the dashboard's
  Angular build. The landing page therefore scopes its palette locally and does **not**
  import it.
- **The docs site** (every route other than `/`) renders in cobalt with JetBrains Mono.
  A visitor clicking "Documentation" crosses a visible style boundary — the wolf mark
  carries across, the palette and type do not.
- **The dashboard** (`dashboard/`) is likewise unmigrated.
- **`website/src/styles/fonts.css`** still self-hosts JetBrains Mono for those two
  surfaces and is intentionally untouched.

Migrating the tokens file would restyle the docs and the dashboard in one commit, which
is a separate, larger piece of work with its own review. Until then, treat this document
as authoritative for `/` and the token file as authoritative for everything else.

## 9. Provenance

Four directions were prototyped and reviewed before this one was adopted: **Sentinel**
(heraldic seal, gunmetal and amber, Bodoni Moda), **Nocturne** (cinematic night, a wolf
of light), **Loom** (this system, woven figure), and **Woven Light** (Loom's system with
Nocturne's assembling hero — adopted).

Rejected in review, recorded so they are not re-proposed: a **particle** hero (tried
twice), **binary 0/1** as the figure's material (generic, and carries no information
about what the product actually checks), and the **outline and eyes** on the hero figure.
