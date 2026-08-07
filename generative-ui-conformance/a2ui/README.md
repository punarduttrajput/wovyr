# A2UI port: what carries over, and what the protocol cannot express

Port of the [protocol-neutral conformance vectors](../vectors.json) to **A2UI v0.9.1**
(basic catalog). Vectors are in [`vectors-a2ui.json`](vectors-a2ui.json).

**Six of nine port cleanly. Three are inexpressible** — not unenforced, *unrepresentable* —
and all three fail for the same reason.

> **A2UI interactive components declare a visual variant but no semantic action class.**

`Button.variant` is `"default" | "primary" | "borderless"`. `TextField.variant` is
`"longText" | "number" | "shortText" | "obscured"`. Per `catalog.json`, no component in the
basic catalog carries a destructive, role, or importance attribute — in v0.9.1 or in the
v1.0 draft, which adds `actionResponse` / `actionId` / `callFunction` but no action semantics.

---

## The three that don't port

| Vector | Why it cannot be expressed |
|---|---|
| `affirmative_action_wearing_a_cancel_label` | Needs a control declaring affirmative semantics while wearing a negative label, so the two can contradict. A2UI has a visual variant and an opaque event name. Nothing to contradict. |
| `destructive_reading_label_under_a_neutral_class` | Same cause, inverse direction. Detecting a destructive label *hiding under* a benign class requires a class to hide under. |
| `destructive_action_without_opt_in` | Deny-by-default for destructive actions requires the protocol to state destructiveness. A deployer cannot deny what the wire format never says — and cannot route high-risk actions to an approval gate, because the gate has nothing to key on. |

This is the concrete, actionable finding. A `class` (or `role`, or `intent`) enum on
interactive components — `confirm` / `approve` / `submit` / `cancel` / `reject` /
`destructive` / `neutral` — is a small, additive, backward-compatible change that makes
all three checkable. Without it, the deceptive-control family is invisible to any conforming
validator, and human-in-the-loop approval gates have no signal to trigger on.

Worth noting this is exactly the threat class the [published MAESTRO threat model for
AG-UI/A2UI](https://kenhuangus.substack.com/p/ag-ui-and-a2ui-protocols-explained)
names as Layer 7 *"UI Confusion / Deceptive Interfaces"*, with the example of a
`"View Details"` button that triggers deletion.

---

## The structural observation

The basic catalog's own example gallery ships
[`09_login-form.json`](https://github.com/google/A2UI/blob/main/specification/v0_9_1/catalogs/basic/examples/09_login-form.json)
— email field, password field with `variant: "obscured"`, *"Welcome back"*,
*"Sign in to your account"*, a **Sign in** button — and `22_credit-card.json`.

These are canonical, spec-endorsed surfaces. They are also, structurally, a credential
harvesting form.

An agent under prompt injection that emits a surface byte-identical to `09_login-form.json`
produces a working phishing page. **No conforming validator can tell the attack from the
endorsed example, because at the protocol level they are the same document.** The
distinguishing fact — whether this application is legitimately in an authentication flow
right now — is not present anywhere in the wire format.

That is not a criticism of the catalog model, which correctly eliminates code execution and
markup injection and is the right foundation. It is about the layer above it. Two possible
directions:

1. Credential-collecting components are **out of scope** for agent-generated surfaces, and
   authentication is a host-supplied flow agents cannot address; or
2. The protocol carries surface **purpose/provenance**, so policy can require that an
   authentication surface was host-initiated rather than agent-initiated.

---

## Three A2UI-specific vectors

Attack surface A2UI has that the source protocol does not. These are **proposed rules** —
there is no existing implementation of them, and they are flagged as such in the JSON.

- **`a2ui_markdown_link_in_text_is_uncontrolled`** — `Text.text` supports Markdown, which
  admits inline links. An origin allow-list covering `Image.url` but not Markdown link
  targets leaves a phishing channel open inside ordinary body copy.
- **`a2ui_openurl_functioncall_has_no_origin_allowlist`** — an action may be a local
  `functionCall`, and the documented `openUrl` takes a `url` argument with no origin
  constraint. That is a direct client-side navigation primitive, and the more dangerous of
  the two channels.
- **`a2ui_obscured_variant_is_not_a_policy_signal`** — pre-empts a natural but wrong
  inference. `variant: "obscured"` is a *rendering* hint. Omitting it must not reduce
  enforcement; setting it must not grant permission. The frame collecting a password
  *without* `obscured` is if anything the more hostile of the two.

---

## Porting notes for implementers

Three structural differences that cost real implementation effort:

**Labels are child references, not props.** `Button` has no `text`. Its label is a separate
`Text` component referenced by `child` (required per `catalog.json`; confirmed in
`00_interactive-button.json` and `09_login-form.json`). Every label-reading rule therefore
needs a resolution pass over the flat component array, and must handle a dangling reference.
A validator that reads properties only cannot implement label-based rules at all.

**Input names are data paths.** `TextField` has no `name`. The nearest analogue is
`value: { "path": "/..." }`. Sensitive-input matching should read **both** the `label` and
the binding path — the path often carries the more honest signal (`/payment/cardNumber`).

**Components are flat.** Parent-child is `children: [ids]` on `Column`/`Row` and `child: id`
on `Card`/`Button`. Traversal is a graph walk over a lookup table, not a tree descent, and
needs cycle handling.

---

## Status and caveats

**These vectors have not been run against a live A2UI renderer or schema validator.** They
were constructed against `catalog.json` and the shipped example gallery's structure, and
schema conformance is therefore *claimed, not proven*. Anyone with an A2UI implementation
to hand who runs them and finds a frame that does not validate — please say so, that is a
bug in this file, not in the argument.

Frames use version string `"v0.9"` and the `v0_9` `catalogId` to match the shipped examples
verbatim.

Apache-2.0, same as A2UI. Lift freely.
