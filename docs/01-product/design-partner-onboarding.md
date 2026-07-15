<!--
File: docs/01-product/design-partner-onboarding.md
Document ID: GDE-001
-->

# Design Partner Onboarding: The Generative UI Trust Runtime

**Document ID:** GDE-001
**File Path:** `docs/01-product/design-partner-onboarding.md`
**Version:** 1.0.0
**Status:** Ready — describes what's actually shipped (RM-GUI-P1–P3), not a
target-state design. Every command below has been run against a real
`apex-server`; where something is aspirational or cut, it's called out
explicitly rather than glossed over.
**Owner:** Product / Founder
**Last Updated:** 2026-07-15

---

## 1. Who this is for

You're a team shipping an agent product — a workflow engine, a chat app, an
internal tool — and your agent occasionally needs a human to look at
something and decide: approve this refund, confirm this reorder, pick from
these three vendor quotes. You don't want to hand your model a raw HTML/JS
sandbox (that's an injection surface), and you don't want to hand-roll a
constrained UI vocabulary and an audit trail yourself.

This guide gets you from a fresh clone to **your own agent rendering its
first trust-layer-governed frame and recording a human's decision**, end to
end, in one sitting. PRD-005 §8 sets a 30-minute quickstart-to-first-frame
target for exactly this path — timings below are what actually elapsed
running it fresh.

**You do not need to adopt our workflow engine, agent framework, or durable
execution model.** RM-GUI-P3's "standalone middleware mode" (EMB-701) is
built for exactly this: three HTTP calls (`present`, `decide`,
`getDecision`), no workflow/agent adoption required at all. If you *are*
already running Apex workflows, the same trust layer is also wired into the
`ui` workflow activity — see
[`prd-generative-ui-runtime.md`](prd-generative-ui-runtime.md) §5 (UIP/HIL
workstreams) for that path instead.

---

## 2. What you're actually adopting

```
your agent  →  UiFrame (JSON, constrained vocabulary)
                  │
                  ▼
         apex-ui-guard trust layer   (deny-by-default: no raw HTML, no
                  │                   credential inputs, destructive actions
                  │                   blocked, media origins allow-listed)
        ┌─────────┴─────────┐
     Allow/Redact         Block
        │                    │
        ▼                    ▼
  rendered to a human   never reaches the human;
  (web component or      recorded in the audit chain
  @apex/ui-react)
        │
        ▼
  human decision  →  POST /api/v1/ui/decisions/{frame_id}
        │
        ▼
  your backend reads it back via GET .../decisions/{frame_id}
```

The vocabulary (`UiNode`: column/row/card/text/badge/key-value/image/button/
text-input/number-input/select/checkbox) deliberately has **no raw-HTML or
script node and no credential-input component** — `apex-ui-guard` polices
what remains expressible (sensitive-input names, destructive actions,
deceptive labels, unapproved media origins). See ADR-0011 §2.4 for why this
is a floor the vocabulary enforces structurally, not a policy someone could
misconfigure away.

---

## 3. Quickstart (~25 minutes, timed)

### 3.1 Run a server (≈5 min)

```bash
git clone https://github.com/punarduttrajput/Apex.git
cd Apex
cargo build -p apex-cli
```

Start it with a UI policy configured — **without one, the hosted floor
(GRD-207) denies every interactive frame by default**, which is the safe
failure mode but not what you want for a first try:

```bash
APEX_UI_POLICY=examples/policies/default-ui-policy.yaml \
APEX_PLATFORM_ADMINS=design-partner \
APEX_ALLOW_ANONYMOUS=1 \
  cargo run -p apex-cli -- dev --addr 127.0.0.1:8080
```

`examples/policies/default-ui-policy.yaml` is the minimal real policy —
every `rules` field is optional and defaults to the strict/safe setting
(`PolicyRules::default()`); this file's only job is to *exist* so the hosted
floor steps aside. `APEX_PLATFORM_ADMINS=design-partner` +
`APEX_ALLOW_ANONYMOUS=1` is a **development convenience**, not how you'd run
this in production — see §7 before you ship.

### 3.2 Present a frame — no workflow, no agent (≈5 min)

```bash
curl -s -X POST http://127.0.0.1:8080/api/v1/ui/present \
  -H 'Content-Type: application/json' \
  -H 'X-Apex-Tenant: default' \
  -H 'X-Apex-Principal: design-partner' \
  -d '{
    "frame": {
      "schema_version": "1.0.0",
      "title": "Confirm refund",
      "root": {
        "type": "column",
        "children": [
          { "type": "text", "text": "Refund $42.00 to order #A1017?" },
          { "type": "button", "action": "approve", "label": "Approve", "class": "approve" },
          { "type": "button", "action": "cancel", "label": "Cancel", "class": "cancel" }
        ]
      }
    }
  }' | tee /tmp/pending.json
```

You get back a `PendingUiFrame`: `frame_id`, `frame_hash` (SHA-256 over the
frame's canonical JSON — hash it yourself client-side before rendering and
compare, RDR-403), and `policy_ref` naming which policy judged it
(`default@v1`). Try mutating the frame to add a `text_input` named
`"card_number"` — you'll get a `403` instead, with the block recorded in the
audit chain (`GET /api/v1/audit`), never reaching this far silently.

### 3.3 Render it (≈10 min — pick one)

**Option A — you're a React app.** Add `@apex/ui-react` (currently consumed
via git checkout + a `file:` dependency — see the known gap in §8) and use
`<UiFrameView>` directly:

```tsx
import { UiFrameView } from "@apex/ui-react";
import "@apex/ui-react/styles.css";

<UiFrameView
  frame={pending.frame}
  expectedHash={pending.frame_hash}
  onDecide={(decision) => apexClient.ui.decide(pending.frame_id, decision)}
/>;
```

**Option B — you're not a React app** (Angular, Vue, plain HTML, a CMS-embedded
widget). Use the `<apex-ui-frame>` web component instead (RDR-402) — it's
still React under the hood, but your host never needs to know:

```html
<script type="module">
  import "@apex/ui-react/web-component";
  import "@apex/ui-react/styles.css";
</script>
<apex-ui-frame id="frame"></apex-ui-frame>
<script type="module">
  const el = document.getElementById("frame");
  el.frame = pending.frame; // set as a JS property, not an HTML attribute
  el.expectedHash = pending.frame_hash;
  el.addEventListener("decide", (e) => {
    e.detail.result = apexClient.ui.decide(pending.frame_id, e.detail.decision);
  });
</script>
```

See `examples/ui/checkout-demo/web-component.html` for a complete, running
example that never imports React on the host page at all.

**Option C — you're not on JS/TS at all.** The three routes are plain HTTP +
JSON; hash-verify with any SHA-256 implementation over the frame's
alphabetical-key-sorted JSON (see `sdks/ui-react/src/hash.ts` for the exact
canonicalization if you're porting it) and render the vocabulary in your own
UI toolkit. You lose the pre-built renderer, not the trust layer.

### 3.4 Record the decision, read it back (≈5 min)

```bash
FRAME_ID=$(node -e "console.log(JSON.parse(require('fs').readFileSync('/tmp/pending.json')).frame_id)")

curl -s -X POST "http://127.0.0.1:8080/api/v1/ui/decisions/$FRAME_ID" \
  -H 'Content-Type: application/json' \
  -H 'X-Apex-Tenant: default' \
  -H 'X-Apex-Principal: design-partner' \
  -d '{ "action": "approve", "values": {} }'

curl -s "http://127.0.0.1:8080/api/v1/ui/decisions/$FRAME_ID" \
  -H 'X-Apex-Tenant: default' -H 'X-Apex-Principal: design-partner'
```

The second call works **even after the pending record is gone** — standalone
decisions are retrievable by `frame_id` independent of any workflow state
(there is no workflow here at all). That's the whole loop: present → render →
decide → retrieve, with the trust layer governing every frame the same way
regardless of which of these three paths produced it.

---

## 4. Configuring your own policy

Start from `examples/policies/default-ui-policy.yaml` and tighten from
there — every field in `PolicyRules` defaults to the strict setting, so
*loosening* is the only direction that needs an explicit opt-in:

```yaml
name: acme-prod
version: 1
rules:
  allow_destructive_actions: false      # default; opt in per-tenant if you truly need it
  allowed_media_origins: [".acme-cdn.com"]  # empty = no images at all, the default
  redact_text_patterns: ["ACME-INTERNAL"]   # scrubbed from display text, not blocked
```

`version` is stamped into every verdict's audit record — treat a published
`(name, version)` as immutable (SAF-202); bump the version when you change
rules.

---

## 5. Verify your policy before you ship (EMB-704)

`apex-ui-guard::conformance` is a public, reusable set of must-allow /
must-block / must-redact vectors — the same claims this repo's own CI gates
on, exposed so *you* can gate on them too:

```rust
use apex_ui_guard::conformance::conformance_report;

let report = conformance_report(&my_policy);
assert!(report.all_passed(), "{report}");
```

Today this means depending on the `apex-ui-guard` crate from a checkout of
this repo (it isn't published to crates.io yet — see the gap list below);
wire the assertion above into your own CI so a policy change that silently
stops blocking sensitive inputs fails your build, not your first incident.

---

## 6. Production readiness checklist

- [ ] Move off `APEX_ALLOW_ANONYMOUS` + `APEX_PLATFORM_ADMINS` — configure
      `APEX_AUTH_MODE=jwt` or `apikey` (`crates/apex-server/src/auth.rs`;
      [`09-api/authentication.md`](../09-api/authentication.md) has the
      up-to-date detail on what's actually implemented vs. target-state).
- [ ] Grant the presenting/deciding principal a role with `ui:read`/
      `ui:write` — any built-in role at `Editor` or above already has both
      (RBAC scopes follow a generic `<domain>:read`/`:write` pattern, so no
      code change is needed for a new resource like `ui`).
- [ ] Set `APEX_CORS_ALLOWED_ORIGINS` to your actual frontend origin(s), not
      `*`.
- [ ] Run the conformance suite (§5) against your **actual** configured
      policy in CI, not just the shipped defaults.
- [ ] Decide your integrity-verification story: `expectedHash` client-side
      checking (RDR-403) catches transport/render tampering but is not a
      substitute for TLS — see `sdks/ui-react/src/hash.ts`'s doc comment for
      its documented edge cases (large/small `f64` formatting).
- [ ] Point whoever runs incident response at `GET /api/v1/audit` — every
      present/block/decide is a tamper-evident, hash-chained record
      (`verify()` clean is part of this repo's own test suite).

---

## 7. Known gaps — read this before you assume something exists

Matching this project's own house rule of documenting cut lines rather than
letting them surface as a surprise later:

- **No public package registry yet.** `@apex/ui-react`, `@apex-ai/sdk`, and
  `apex-ui-guard` are consumed via a git checkout (`file:` path deps, or a
  Rust path/git dependency) — not published to npm or crates.io. Budget for
  vendoring a checkout, not `npm install @apex/ui-react`.
- **No MCP server subsystem.** If you were expecting a Model Context
  Protocol server exposing these routes as MCP tools (an earlier roadmap
  item, EMB-702), it does not exist in this codebase at all yet.
- **No OAuth2/mTLS flow.** Real auth today is JWT (HS256/RS256) or a
  SHA-256-hashed API key looked up server-side — see
  [`09-api/authentication.md`](../09-api/authentication.md)'s own top-of-file
  disclaimer for the full target-vs-actual gap.
- **No saveable/reusable "surfaces."** Every frame is presented fresh; there
  is no library of named, versioned UI templates yet (ITS-604 in the
  roadmap, not built).
- **The judge-style semantic policy checks (GRD-203's LLM variant)** are not
  wired in — only the structural, deterministic rules (`apex-ui-guard`'s
  `evaluate`/`hosted_floor`) run today. Nothing here calls a model.

---

## 8. Getting help

File issues against
[github.com/punarduttrajput/Apex](https://github.com/punarduttrajput/Apex) —
include the `policy_ref` and `frame_id` from the relevant `PendingUiFrame` or
`DecidedOutcome`, and (if it's a block you didn't expect) the audit entry
from `GET /api/v1/audit` naming the rule that fired.

---

## 9. Related documents

- [`prd-generative-ui-runtime.md`](prd-generative-ui-runtime.md) — the full
  product spec this guide implements a slice of.
- [`ADR-0011-generative-ui-repositioning.md`](../17-adr/ADR-0011-generative-ui-repositioning.md)
  — why the vocabulary has no raw-HTML/script node.
- [`v1.2-generative-ui.md`](../18-roadmap/v1.2-generative-ui.md) — phase
  status and what's cut per phase.
- `examples/ui/checkout-demo/README.md` — a full workflow-backed example (not
  standalone mode) if you *are* adopting the workflow engine.

---

## 10. Revision History

| Version | Date | Description |
|---------|------|--------------|
| 1.0.0 | 2026-07-15 | Initial design-partner onboarding guide (RM-GUI-P3, PRD-005 §8) |
