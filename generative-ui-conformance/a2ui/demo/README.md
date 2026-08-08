# What a missing action class costs

A live demo for [a2ui-project/a2ui#2197](https://github.com/a2ui-project/a2ui/issues/2197):
the wovyr generative-UI trust layer, compiled to WebAssembly, evaluating **real A2UI
surfaces** in the browser. No backend, no framework, no install.

Every surface here is schema-valid and catalog-conformant — [verified against A2UI's own
JSON Schemas](../validate). The demo is about what policy can and cannot *say* about them.

## The result

| Surface | Verdict | |
|---|---|---|
| Button labelled "Cancel" that confirms a purchase | **allow** | deception invisible |
| Irreversible action, no approval gate | **allow** | nothing declared destructive |
| "View Details" that deletes | **allow** | honest gap — see below |
| **Honest "Delete address" button** | **block** `intent_mismatch` | **false positive** |
| Credential-shaped field | block `sensitive_input` | caught |
| Image from unapproved origin | block `media_origin` | caught |
| Password field without `obscured` | block `sensitive_input` | caught |
| Interactive surface, no policy configured | block `hosted_floor` | caught |

The first four rows are the argument. Because A2UI declares no semantic action class, the
adapter must force every `Button` to `neutral` — and from there the same rule set produces
**both** failure modes at once:

- **False negatives.** A control's declared intent is the only thing a label can be checked
  *against*. With no class, a "Cancel" button that confirms a purchase has nothing to
  contradict, so the deception passes untouched.
- **False positives.** The one rule that still fires — destructive-reading label under a
  non-destructive class — now fires on *every* honestly-labelled destructive button,
  because none of them can declare themselves destructive either.

There is no setting that fixes both. That is the cost the proposal is about.

The `"View Details"` row is included deliberately even though wovyr also misses it: the lie
lives in the action's server-side effect, outside the frame entirely. It's documented as a
known gap in [`../vectors-a2ui.json`](../vectors-a2ui.json), not papered over.

## Run it

```bash
cargo build --release --target wasm32-unknown-unknown
cp target/wasm32-unknown-unknown/release/a2ui_trust_demo.wasm .
node serve.js          # then open http://127.0.0.1:4173/a2ui/demo/index.html
```

A static server is needed only because `fetch()` of a `.wasm` doesn't work over `file://`.
On GitHub Pages, `index.html` + the `.wasm` + the two JSON fixtures are the whole deployment.

Headless, no browser:

```bash
node run-inexpressible.js   # the four cases above
node run-vectors.js         # all 9 ported conformance vectors
cargo test                  # adapter unit tests
```

## How it's built

**No `wasm-bindgen`.** The module has **zero imports** — it's a pure function reached
through a ~20-line shim: `alloc(n)` → write UTF-8 JSON → `evaluate_a2ui(ptr, len)` → read a
packed `u64` (`ptr << 32 | len`). 139 KB stripped. This removes a toolchain dependency
rather than adding one.

**[`src/a2ui.rs`](src/a2ui.rs)** is the adapter, and its notes are the interesting output.
Three mappings cost real information:

- `Button` has no `text`; its label is a **child `Text` node referenced by id**, so reading
  a label requires resolving against the flat component array. A validator that only reads
  properties cannot implement label rules at all.
- `TextField` has no `name`; the nearest analogue is the **data-binding path**
  (`/payment/cardNumber`), which often carries the more honest signal than the label.
- A label may be **data-bound** (`{"path": "/labels/x"}`) with its text living in
  `updateDataModel`. The adapter resolves it — a policy that inspected components only
  would be evaded by moving the credential label into the data model. There's a unit test
  for exactly that.

## Scope

This demo evaluates **policy over a static surface**. There is no agent, no model, no
workflow engine, no server — deliberately. The trust layer is a pure function from a frame
to a verdict, which is why it fits in 139 KB of wasm with no host imports.

Apache-2.0.
