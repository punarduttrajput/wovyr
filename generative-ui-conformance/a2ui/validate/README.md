# Reproducing the validation

Validates every surface in [`../vectors-a2ui.json`](../vectors-a2ui.json) against A2UI's
**official JSON Schemas**, using `ajv` — the same validator A2UI's own conformance harness
(`specification/v0_9_1/test/run_tests.py`) drives via `ajv-cli`.

```bash
node fetch-schemas.js   # downloads the 11 schemas + basic catalog from a2ui-project/a2ui
npm install
npm test                # validate.js && negative-control.js
```

Requires Node 18+ (uses built-in `fetch`). No Python needed — A2UI's own runner wraps
`ajv-cli` in Python, which these scripts skip by calling `ajv` directly.

## What each script does

**`fetch-schemas.js`** — downloads `specification/v0_9_1/json/*.json` plus
`specification/v0_9_1/catalogs/basic/catalog.json` into `schemas/` (gitignored; always
fetched fresh from upstream so this can't silently drift from the real spec).

**`validate.js`** — validates all 21 messages across 9 surfaces against
`server_to_client.json`. Expected: **9/9 pass**.

**`negative-control.js`** — feeds 11 deliberately malformed surfaces through the same
validator. Expected: **11/11 rejected, 0 leaked**.

Run the negative control. A validator that accepts everything would report a perfect score
on the real vectors while proving nothing; this is what makes the positive result mean
something.

## One implementation note

`server_to_client.json` refs `catalog.json#/$defs/anyComponent`, which resolves against its
own `$id` base to `https://a2ui.org/specification/v0_9/catalog.json`. The basic catalog's
actual `$id` is `.../v0_9/catalogs/basic/catalog.json`, so the scripts register it under
both ids. That substitution is what makes component validation actually engage — without
it, `ajv` fails to resolve the reference at compile time rather than silently passing.

## Result as of 2026-08-07

| | |
|---|---|
| Surfaces | 9/9 pass |
| Messages | 21/21 pass |
| Negative control | 11/11 rejected, 0 leaked |
| Validator | `ajv` 8.20.0, JSON Schema draft 2020-12 |

This proves **schema conformance**, not visual rendering. The surfaces have not been
displayed in a renderer — a separate and weaker claim, since the schema is the normative
artifact.
