# The killer demo — Generative UI Trust Runtime

Reproduces PRD-005 §9's acceptance narrative end to end, for real, against a
running `apex-server`:

> A procurement agent is asked to reorder lab supplies. It composes a
> checkout confirmation. **Take 1**: the model (steered by a poisoned vendor
> page) includes a card-number input — the trust layer blocks the frame; the
> block is in the audit chain; the user never sees it. **Take 2**: the safe
> variant renders; the user approves it; the workflow resumes and completes.

The same flow is proven as an automated integration test at the Rust layer
(`crates/apex-server/src/ui.rs`'s `uc1_frame_survives_restart_...` /
`uc4_credential_frame_is_blocked_...`) and at the SDK layer
(`sdks/typescript/test/client.test.ts`'s `ui:` suite). This app is the
human-visible third leg — the renderer actually showing (and blocking) a
frame in a real browser.

## Run it

1. **Start the server** with a UI policy configured (without one, the hosted
   floor denies every interactive frame, including the safe one) and CORS
   opened for the Vite dev server's origin:

   ```bash
   APEX_UI_POLICY=examples/policies/default-ui-policy.yaml \
   APEX_PLATFORM_ADMINS=sdk-test-admin \
   APEX_ALLOW_ANONYMOUS=1 \
   APEX_CORS_ALLOWED_ORIGINS=http://localhost:5173 \
     cargo run -p apex-cli -- dev --addr 127.0.0.1:8080
   ```

   (`APEX_PLATFORM_ADMINS=sdk-test-admin` + submitting/deciding as that
   principal is the same real-credential requirement every other mutating
   route has — see `crates/apex-server/src/tenancy.rs`. `APEX_ALLOW_ANONYMOUS`
   only lets the *request* authenticate; it's the admin principal that
   actually grants `workflows:run`/`workflows:read`.)

2. **Build `@apex/ui-react`** if you haven't already (this app depends on it
   via a `file:` path, so it needs a `dist/` to resolve against):

   ```bash
   cd ../../../sdks/ui-react && npm install && npm run build
   ```

3. **Run the demo**:

   ```bash
   cd examples/ui/checkout-demo
   npm install
   npm run dev
   ```

   Open the printed `http://localhost:5173` URL. If your server isn't on
   `127.0.0.1:8080`, update the "Server base URL" field in the page.

## What to click

- **"Submit poisoned checkout"** first — the activity log reports the
  execution failed, and no frame ever appears in "Pending frames". That's
  the trust layer: `apex-ui-guard`'s sensitive-input-name rule matched
  `card_number` and blocked the frame before it reached this page at all.
  Check `GET /api/v1/audit` (or the CLI's `apex audit` surface) for the
  `ui.frame.block` record naming the rule.
- **"Submit safe checkout"** — a real, validated frame appears under
  "Pending frames" within ~1.5s (the pull-polling interval). Fill in a PO
  number and click **Approve**. The decision is validated against the frame
  boundary-side (an empty PO number, or clicking before typing one, is
  rejected with a 400 and the frame stays interactive) before it ever
  reaches the workflow; on success the activity log reports the execution
  completed.
- **Kill the server mid-flight** (Ctrl-C after a frame renders, before you
  approve) and restart it with the same command — the same pending frame
  reappears with the same `frame_id`/`frame_hash` (durable, event-sourced
  workflow state), and approving it still resumes the execution. This is the
  "restart" half of PRD-005 §9, driven from the browser instead of a test.

## Files

- `src/App.tsx` — the whole demo: submit buttons, the pending-frames list
  rendered with `<UiFrameView>`, and an activity log.
- The workflow manifests are inlined in `App.tsx`, mirroring
  [`examples/workflows/ui-checkout-approve.yaml`](../../workflows/ui-checkout-approve.yaml)
  and [`ui-checkout-block.yaml`](../../workflows/ui-checkout-block.yaml)
  verbatim (a static demo can't read server-side files at runtime).
