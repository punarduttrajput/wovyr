# Design System — Wovyr Landing

**Status:** Draft (for review)
**Principle:** *Honor the product's existing identity.* The dashboard (`dashboard/src/styles.scss`)
already defines Wovyr's visual language — **"cobalt accent, mono-forward, cool neutrals."**
The landing extends that system to a marketing surface; it does not invent a new one, so
`/`, the docs, and the dashboard read as one product.

---

## 1. Design thesis

Wovyr's world is a **runtime**: terminals, monospace, hash fingerprints, policy gates,
fail-closed defaults, event-sourced audit chains. The landing's one bold move — the
place we spend all our boldness — is a living **character-art "Trust Gate"** in the hero,
built from monospace glyphs, that shows the product's actual thesis: *an AI emits an
interface → it's validated fail-closed → recorded → a human decides.* Everything around
that moment stays quiet, precise, and mono-forward.

This is deliberately **not** any of the current AI-generated landing clichés — no
cream+serif+terracotta, no purple→blue gradient hero, no acid-green pop, no Inter/Space
Grotesk, no emoji section markers, no everything-centered. The character-art gate is
specific to *this* subject and copyable by no competitor.

## 2. Color (tokens, derived from the dashboard system)

Cool neutrals with a slight blue bias (chosen, not defaulted). Cobalt is the single
accent and it carries meaning: **cobalt = verified / passed the gate.** Crit-red is
semantic only: **red = blocked / fail-closed.** Violet is a rare secondary (the human
decision), never a gradient partner.

| Token | Dark (primary) | Light | Role |
|---|---|---|---|
| `--canvas` | `#0A0E18` | `#F6F7F9` | page ground (cool ink / cool paper) |
| `--surface` | `#121826` | `#FFFFFF` | cards, nav |
| `--surface-2`| `#0E1420` | `#EEF1F6` | insets, code chips |
| `--ink` | `#EAEEF6` | `#0D1424` | primary text |
| `--ink-dim` | `#9AA6BF` | `#55617A` | secondary text, mono eyebrows |
| `--line` | `#1E2740` | `#E2E7F0` | 1px borders / hairlines |
| `--accent` (cobalt) | `#5B7BFF` | `#2D54E8` | verified state, links, focus, primary CTA |
| `--accent-2` (violet)| `#9B87FF` | `#7B61FF` | human-decision accent (sparingly) |
| `--pass` | = `--accent` | = `--accent` | frame passed the gate |
| `--block` (crit) | `#FF5C6A` | `#D23B43` | frame blocked / fail-closed |
| `--ok` | `#3FBF86` | `#18935A` | healthz / positive status only |

Contrast: verify `--accent` on `--canvas` and `--ink` on `--canvas`/light `--ink` on
`--surface` all clear WCAG AA. The dark theme is primary (a runtime lives in a terminal);
the light theme gets equal care, not a naive invert.

## 3. Typography

Mono-forward, matching the dashboard. Two roles + a display treatment:

- **Display / headlines:** the **mono** face, set large with tight leading and a hair of
  negative tracking — a runtime speaking in its own voice. Stack (prototype, system):
  `"Cascadia Code","SF Mono","JetBrains Mono",ui-monospace,Menlo,Consolas,monospace`.
  Production self-hosts **JetBrains Mono**.
- **Body / running text:** clean system sans for readability at paragraph length:
  `ui-sans-serif, system-ui, "Segoe UI", Roboto, Helvetica, Arial, sans-serif`. Column
  width ~62–66ch.
- **Utility / eyebrows / data / labels:** mono, uppercase, `letter-spacing: 0.14em`,
  `--ink-dim`. Used as structural markers (section eyebrows like `02 · HOW IT WORKS`,
  status-rail text, hash stamps) — structure that encodes real content, not decoration.
- **Type scale** (rem, 1.25 ratio): 0.75 / 0.875 / 1 / 1.25 / 1.6 / 2.1 / 2.9 / 4.0.
  Headings `text-wrap: balance`. `tabular-nums` on all data/metrics.

## 4. Layout & components

- **Grid:** 12-col, max content width 1200px, generous gutters. **Asymmetric hero**
  (left-aligned headline, gate canvas full-bleed behind a scrim) — not centered.
- **Status rail:** a thin monospace bar (top of nav or footer) reading real state —
  `v0.3.0 · Apache-2.0 · healthz ok · edition 2024`. A terminal status line as a
  structural device, carrying true info.
- **Cards:** `--surface`, 1px `--line` border, radius **10px** (brand), no drop shadow in
  dark (use border + subtle inner glow on hover → border brightens to `--accent`). No
  accent-bar-on-rounded-card motif.
- **Numbered markers:** ONLY on the How-it-works pipeline — it's a true ordered sequence
  (emit → validate → record → decide). Nowhere else.
- **Brand mark (DSY-106, corrected 2026-07-27):** the window/scanline mark — a UI
  frame (`.mk-win`, rounded rect stroked in `--ink`) with two title-bar dots
  (`.mk-dot`, `--ink-2`/`--ink-dim`), a cobalt scanline (`.mk-scan`) and a
  verified node (`.mk-node`, both `--accent`) passing through it — encodes the
  product thesis directly (a rendered interface being verified), not a generic
  shape. Used everywhere: favicon, landing nav/footer, OG image, and the
  dashboard rail. This entry previously named a plain triangle
  (`M12 3L21 19H3L12 3Z`) as canonical — that was the dashboard's mark at the
  time, before this unification; the triangle is retired, not a second valid
  option. Source: `website/landing/assets/wovyr-logo.svg` (full wordmark
  lockup) and the inline SVG in `website/src/pages/index.astro`/
  `dashboard/src/app/app.html` (icon only, `.mk-*` classes carry the same
  styling in each surface's own stylesheet). Wordmark set in mono,
  letter-spaced.
- **Code chips:** `--surface-2`, mono, copy button; the 3-command quickstart is real,
  copyable, and offline-true.

## 5. Motion

Spend motion on the hero; keep the rest restrained (over-animation reads as AI-generated).

- **Hero:** ambient looping Trust Gate (Canvas, ~30fps, deterministic seed). The one
  orchestrated moment.
- **Scroll reveal:** sections fade/rise 12px once on enter (IntersectionObserver), subtle,
  gated by `prefers-reduced-motion`.
- **Hover micro-interactions:** card border → cobalt; CTA slight lift; copy-button state.
- **Reduced motion:** all of the above collapse to static; hero renders one seeded frame.

## 6. Three hero directions (pick one at review)

The prototype ships **Direction 1** fully built; the other two are described so you can
redirect cheaply.

1. **The Trust Gate (built).** Char-art frames flow through a validating gate; safe pass
   with a cobalt hash stamp, unsafe blocked in red. Most literal to the thesis; highest
   memorability. *Recommended.*
2. **The Audit Chain.** A horizontally scrolling char-art hash-chain (`░ prev←hash ░`)
   that assembles block by block as you watch, each block a recorded human decision.
   Quieter, more "ledger," leans the tamper-evident-audit differentiator.
3. **The Frame Loom.** Character glyphs weave a UI "frame" into existence row by row,
   then a policy pass sweeps through it highlighting/removing unsafe nodes. Most
   "generative," leans the constrained-vocabulary story.

---

**Deliverable order:** PRD → TRD → this doc → interactive prototype (Direction 1) for
review → (on approval) port into `website/src/pages/index.astro`.
