# Generative UI: conformance vectors for the gap above the catalog

A closed component catalog is the right foundation for generative UI. It is not
sufficient on its own, and this is a small, concrete demonstration of why.

**Every blocked vector in [`vectors.json`](vectors.json) is catalog-valid.** Each one uses
only standard catalog components, with individually well-typed props, and would pass any
pure schema or catalog conformance check. Several of them are credential-harvesting or
intent-deceptive interfaces.

The vectors are Apache-2.0 and free to lift. Reuse the *vector* — threat, frame shape,
expected verdict — not necessarily the dialect the frames happen to be written in.

---

## The argument in one example

```json
{
  "type": "column",
  "children": [
    { "type": "text_input", "name": "card_number", "label": "Card number" },
    { "type": "button", "action": "pay", "label": "Continue", "class": "submit" }
  ]
}
```

`column`, `text_input`, `button` — three ordinary catalog components. Every prop is a
legal value of its declared type. There is no script, no raw HTML, no injected markup,
no protocol violation. A catalog check, a JSON Schema check, and a prop sanitizer all
pass this frame.

It is a payment-details phishing form.

The same holds for deception, in both directions:

```json
{ "type": "button", "action": "sneaky",  "label": "Cancel",         "class": "confirm" }
{ "type": "button", "action": "cleanup", "label": "Delete account", "class": "neutral" }
```

Two valid props each. The defect exists only in the **relationship between them** — a
label that reads as backing out on a control that affirms, and a destructive-reading
label on a class that would evade a destructive-action approval gate.

This is the load-bearing point: the failure is not weak prop validation. Strengthening
per-prop validation does not reach it. **The unit of validation has to be the assembled
frame, evaluated against policy — not the individual component, validated against schema.**
Catalogs give you conformance. They do not give you intent.

---

## The vectors

| Vector | Catalog-valid | Expected | Rule |
|---|---|---|---|
| `benign_confirmation_is_allowed` | yes | allow | — |
| `credential_named_input_is_blocked` | **yes** | block | `sensitive_input` |
| `credential_lookalike_word_is_not_blocked` | yes | allow | — |
| `destructive_action_without_opt_in_is_blocked` | **yes** | block | `destructive_action` |
| `affirmative_action_wearing_a_cancel_label_is_blocked` | **yes** | block | `intent_mismatch` |
| `destructive_reading_label_under_a_neutral_class_is_blocked` | **yes** | block | `intent_mismatch` |
| `image_with_no_allowed_origins_is_blocked` | **yes** | block | `media_origin` |
| `hosted_floor_denies_an_interactive_frame` | yes | block | `hosted_floor` |
| `hosted_floor_allows_a_display_only_frame` | yes | allow | — |

Two of the nine are deliberately **must-allow** false-positive guards, because a rule set
that only ever blocks is untestable and undeployable:

- `credential_lookalike_word_is_not_blocked` — `discard_reason` contains `card` as a
  substring. Token matching passes it; substring matching fails it. This vector is how you
  tell the two implementations apart.
- `hosted_floor_allows_a_display_only_frame` — a frame with no actions carries no decision
  to hijack, and renders freely even with no policy configured. The floor is deny-**interactive**,
  not deny-all.

---

## What this does not catch

Listed in full under `known_gaps` in the JSON. The most important one, stated plainly:

> **A benign label with a dishonest server-side effect is not detected.**
>
> A button labelled `"View Details"`, declared `class: "neutral"`, whose action id resolves
> server-side to account deletion, passes every rule here. Every axis the policy can observe
> is honest — the label is benign, the class is consistent with the label, the props are
> valid. The lie lives in the binding between the action id and its effect, which is outside
> the frame entirely.

A declarative frame policy structurally cannot see that. Frame-level policy bounds what a
user can be **shown** and asked to decide; it cannot attest that a declared action class
matches the effect the backend will execute. Closing it needs either an effect registry
mapping action ids to declared effect classes and checked at dispatch, or a signed
action→effect binding established when the tool is registered.

Open question, and the reason this file exists rather than a blog post: **does that belong
in the UI protocol at all, or in the agent/tool-calling layer beneath it?**

Two smaller gaps, also documented: the label token lists are English-only, and there is no
semantic judge — every rule here is pure, deterministic and dependency-free so that a
verdict is reproducible and auditable, which is a deliberate trade against coverage.

---

## Running them

The vectors are plain JSON and carry their own expectations, so a harness in any language
is a loop. Structure per vector:

```
name, threat, catalog_valid, expected ∈ {allow, redact, block}, rule, frame
```

Against the reference implementation (Rust, Apache-2.0):

```rust
use wovyr_ui_guard::{UiPolicy, PolicyRules};
use wovyr_ui_guard::conformance::conformance_report;

let policy = UiPolicy { name: "prod".into(), version: 1, rules: PolicyRules::default() };
let report = conformance_report(&policy);
assert!(report.all_passed(), "{report}");
```

A deployer who deliberately loosens a rule is expected to diverge on exactly the
corresponding vector and no other — e.g. setting `allow_destructive_actions: true` fails
`destructive_action_without_opt_in_is_blocked` and nothing else. That property is itself
tested. Read the report alongside knowing which of your own rules you loosened; it is not
meant as an unconditional pass/fail.

---

## Porting to another protocol

The frames use a declarative node tree over a constrained component vocabulary: node
`type` + typed props + a declared semantic **action class** on every interactive control.

The action class is the part that matters and the part most likely to be missing elsewhere.
Both `intent_mismatch` directions depend on a control declaring its semantics
(`confirm` / `approve` / `submit` / `cancel` / `reject` / `destructive` / `neutral`)
separately from its label, so the two can be checked against each other. Without a
declared class there is nothing to compare the label to, and the deception vectors become
undetectable rather than merely unenforced.

Porting the frame JSON to another catalog schema is mechanical. Porting the vectors
requires that target protocol to carry declared action semantics.

---

Apache-2.0. Extracted from `wovyr-ui-guard`; these vectors run in that project's CI, where
the deny-by-default reference policy passes all nine.
