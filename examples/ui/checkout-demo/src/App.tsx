import { UiFrameView, createUiClient, usePendingFrames } from "@wovyr/ui-react";
import { useMemo, useState } from "react";

// Mirrors examples/workflows/ui-checkout-approve.yaml / ui-checkout-block.yaml
// verbatim — inlined so this static demo needs no server-side file serving.
const APPROVE_MANIFEST = `apiVersion: workflow.wovyr.io/v1
kind: Workflow
metadata:
  name: ui-checkout-approve
  version: 1.0.0
spec:
  activities:
    - id: confirm
      type: ui
      inputs:
        frame:
          schema_version: 1.0.0
          title: Confirm order
          root:
            type: column
            children:
              - type: text
                text: Reorder 3 boxes of pipette tips from LabSupply Co?
              - type: key_value
                entries:
                  - { key: Vendor, value: "LabSupply Co" }
                  - { key: Total, value: "$412.80" }
              - type: text_input
                name: po_number
                label: PO number
                required: true
              - type: row
                children:
                  - { type: button, action: approve, label: Approve, class: approve }
                  - { type: button, action: cancel, label: Cancel, class: cancel }
`;

const BLOCK_MANIFEST = `apiVersion: workflow.wovyr.io/v1
kind: Workflow
metadata:
  name: ui-checkout-block
  version: 1.0.0
spec:
  activities:
    - id: confirm
      type: ui
      inputs:
        frame:
          schema_version: 1.0.0
          title: Confirm payment
          root:
            type: column
            children:
              - type: text
                text: Enter payment details to finish your order
              - type: text_input
                name: card_number
                label: Card number
              - { type: button, action: pay, label: Continue, class: submit }
`;

interface LogEntry {
  ts: string;
  message: string;
}

export function App() {
  const [baseUrl, setBaseUrl] = useState("http://127.0.0.1:8080");
  const [principal, setPrincipal] = useState("sdk-test-admin");
  const [log, setLog] = useState<LogEntry[]>([]);
  const [busy, setBusy] = useState(false);

  const client = useMemo(() => createUiClient({ baseUrl, principal }), [baseUrl, principal]);
  const { frames, decide } = usePendingFrames(client, { intervalMs: 1500 });

  function appendLog(message: string) {
    setLog((prev) => [{ ts: new Date().toLocaleTimeString(), message }, ...prev].slice(0, 20));
  }

  async function submit(kind: "approve" | "block") {
    setBusy(true);
    const manifest = kind === "approve" ? APPROVE_MANIFEST : BLOCK_MANIFEST;
    const executionId = `${kind}-${Date.now()}`;
    try {
      const res = await fetch(`${baseUrl}/api/v1/workflows`, {
        method: "POST",
        headers: { "content-type": "application/json", "X-Wovyr-Principal": principal },
        body: JSON.stringify({ manifest, execution_id: executionId }),
      });
      const body = await res.json();
      if (!res.ok) {
        appendLog(`submit(${kind}) failed: ${res.status} ${JSON.stringify(body)}`);
        return;
      }
      appendLog(`submitted "${executionId}" — watching for either a rendered frame or a block…`);
      pollOutcome(executionId, kind);
    } finally {
      setBusy(false);
    }
  }

  function pollOutcome(executionId: string, kind: "approve" | "block") {
    let attempts = 0;
    const interval = setInterval(async () => {
      attempts++;
      const res = await fetch(`${baseUrl}/api/v1/workflows/${executionId}`, {
        headers: { "X-Wovyr-Principal": principal },
      });
      if (res.ok) {
        const body = await res.json();
        const status = body.execution?.status;
        if (status === "failed" && kind === "block") {
          appendLog(`"${executionId}" failed as expected — the trust layer blocked the frame`);
          clearInterval(interval);
        } else if (status === "completed") {
          appendLog(`"${executionId}" completed`);
          clearInterval(interval);
        }
      }
      if (attempts > 40) clearInterval(interval);
    }, 500);
  }

  return (
    <main className="demo-shell">
      <h1>Wovyr — Generative UI Trust Runtime</h1>
      <p className="demo-subtitle">
        The killer demo (PRD-005 §9): submit a safe checkout or a poisoned one, and watch the
        trust layer render, block, and resume — for real, against a running <code>wovyr-server</code>.
      </p>

      <section className="demo-config">
        <label>
          Server base URL
          <input value={baseUrl} onChange={(e) => setBaseUrl(e.target.value)} />
        </label>
        <label>
          Principal
          <input value={principal} onChange={(e) => setPrincipal(e.target.value)} />
        </label>
      </section>

      <section className="demo-actions">
        <button disabled={busy} onClick={() => submit("approve")}>
          Submit safe checkout (renders, you approve)
        </button>
        <button disabled={busy} onClick={() => submit("block")}>
          Submit poisoned checkout (asks for a card number — blocked)
        </button>
      </section>

      <section className="demo-frames">
        <h2>Pending frames</h2>
        {frames.length === 0 && <p className="demo-empty">No pending frames right now.</p>}
        {frames.map((f) => (
          <div className="demo-frame-card" key={f.frame_id}>
            <p className="demo-frame-meta">
              frame <code>{f.frame_id}</code> · policy <code>{f.policy_ref}</code>
            </p>
            <UiFrameView
              frame={f.frame}
              expectedHash={f.frame_hash}
              theme="dark"
              onDecide={async (decision) => {
                await decide(f.frame_id, decision);
                appendLog(`decided "${decision.action}" on ${f.frame_id}`);
              }}
            />
          </div>
        ))}
      </section>

      <section className="demo-log">
        <h2>Activity log</h2>
        <ul>
          {log.map((entry, i) => (
            <li key={i}>
              <span className="demo-log-ts">{entry.ts}</span> {entry.message}
            </li>
          ))}
        </ul>
      </section>
    </main>
  );
}
