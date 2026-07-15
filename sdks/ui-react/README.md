# @apex/ui-react

The React renderer for [Apex](../../README.md)'s generative-UI trust runtime
(PRD-005 RDR-4xx): it consumes trust-layer-**validated** `UiFrame`s and turns
a human's click into a typed, boundary-validated decision. It never renders
raw HTML/script and has no credential-input component — the vocabulary it
understands is exactly the constrained set `apex-ui-guard` polices
server-side (see [ADR-0011](../../docs/17-adr/ADR-0011-generative-ui-repositioning.md)).

## Install

Not yet published; consume from the repo directly (`file:` dependency) until
it ships to npm:

```bash
npm install --save "file:../path/to/Apex/sdks/ui-react"
```

## Quickstart (< 30 minutes, frame to pixel)

1. Start a local `apex-server` with a UI policy configured — without one,
   the hosted floor (GRD-207) denies every interactive frame:

   ```bash
   APEX_UI_POLICY=examples/policies/default-ui-policy.yaml \
     cargo run -p apex-cli -- dev
   ```

2. Submit a workflow with a `ui` activity (see
   [`examples/workflows/ui-checkout-approve.yaml`](../../examples/workflows/ui-checkout-approve.yaml)):

   ```bash
   curl -X POST http://127.0.0.1:8080/api/v1/workflows \
     -H 'content-type: application/json' \
     -d '{"manifest": "'"$(sed 's/"/\\"/g' examples/workflows/ui-checkout-approve.yaml | tr '\n' ' ')"'"}'
   ```

3. Render it:

   ```tsx
   import { createUiClient, usePendingFrames, UiFrameView } from "@apex/ui-react";
   import "@apex/ui-react/styles.css";

   const client = createUiClient({ baseUrl: "http://127.0.0.1:8080" });

   function App() {
     const { frames, decide } = usePendingFrames(client);
     return (
       <>
         {frames.map((f) => (
           <UiFrameView
             key={f.frame_id}
             frame={f.frame}
             expectedHash={f.frame_hash}
             onDecide={(decision) => decide(f.frame_id, decision)}
           />
         ))}
       </>
     );
   }
   ```

That's the whole surface: `usePendingFrames` polls
`GET /api/v1/ui/frames`; `UiFrameView` renders the vocabulary and collects
input values; `onDecide` posts to `POST /api/v1/ui/decisions/{id}`, which the
server validates fail-closed against the frame before anything reaches the
workflow (HIL-302) — a rejected decision throws, and the view stays
interactive so the human can correct and retry.

## What this package will not render

- **Raw HTML or script.** The vocabulary (`UiNode` in `types.ts`) is a closed
  union; a `type` it doesn't recognize renders as a visible, inert
  placeholder — never skipped, never interpreted loosely (RDR-403).
- **Credential inputs.** There is no password/card/OTP component in the
  protocol at all — a frame that needs one cannot be expressed.

## Frame integrity (RDR-403)

Pass a pending frame's `frame_hash` as `expectedHash` and `UiFrameView`
recomputes the content hash client-side (`hash.ts`'s `verifyFrame`,
SHA-256 over a canonical, alphabetically-key-sorted JSON form — matching
`apex_ui::UiFrame::content_hash()` exactly) before rendering anything. A
mismatch renders a warning instead of the frame. This is defense-in-depth
against transport/render tampering, confirming the pixels match what the
audit chain recorded — it is not a substitute for TLS.

## Consuming an agent's SSE stream instead of polling

If the host already holds an `agents:stream` connection, extract `ui_frame`
events directly instead of polling:

```ts
import { extractUiFrames } from "@apex/ui-react";

const response = await fetch(`${baseUrl}/api/v1/agents:stream`, { method: "POST", ... });
for await (const { frame_id, frame } of extractUiFrames(response.body!)) {
  // render with <UiFrameView frame={frame} onDecide={...} />
}
```

## Theming

Every visual value is a CSS custom property under the `.apex-ui` wrapper
class (`styles.css`) — override them in a parent selector, or set
`data-theme="dark"`/`"light"` on `<UiFrameView className>`'s container to
force a scheme regardless of `prefers-color-scheme`.

## What's not here yet

- A framework-agnostic web-component build (`RDR-402`) — deferred to a later
  milestone; React is the only supported host today.
- Signed/marketplace-distributed component templates (`CMP-5xx`).

See [the v1.2 roadmap](../../docs/18-roadmap/v1.2-generative-ui.md) for the
full phase plan.
