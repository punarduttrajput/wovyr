import { useEffect, useState } from "react";
import { renderNode } from "./components/index.js";
import { verifyFrame } from "./hash.js";
import type { ActionClass, UiDecision, UiFrame } from "./types.js";

export interface UiFrameViewProps {
  /** The validated frame to render (a {@link PendingUiFrame}'s `frame` field,
   * or one consumed directly off an `agents:stream` `ui_frame` SSE event). */
  frame: UiFrame;
  /** When supplied (typically the pending frame's `frame_hash`), the view
   * recomputes the content hash client-side and renders an
   * {@link IntegrityWarning} instead of the frame if it doesn't match
   * (RDR-403) — a defense-in-depth check, not a substitute for TLS; see
   * `hash.ts`'s doc comment for its known limitations. */
  expectedHash?: string;
  /** Called once with the assembled decision when the human picks an action.
   * The view disables itself while this is pending and re-enables on
   * rejection (so a failed decide — e.g. a 400 from a stale required field —
   * is retryable) but stays disabled on success (the frame is presumed
   * consumed). */
  onDecide: (decision: UiDecision) => void | Promise<void>;
  /** Externally force the disabled state (e.g. the host already knows this
   * frame is stale). Independent of the view's own in-flight tracking. */
  disabled?: boolean;
  className?: string;
}

/** Renders a trust-layer-validated {@link UiFrame} and turns a human's click
 * into a typed {@link UiDecision} (PRD-005 RDR-401). Collects input values as
 * the human fills them in (lifted state, see `components/index.tsx`'s
 * `RenderCtx`) and assembles `{ action, values }` on submit — the server
 * still validates fail-closed (HIL-302); this is ergonomics, not the
 * enforcement point. */
export function UiFrameView({
  frame,
  expectedHash,
  onDecide,
  disabled,
  className,
}: UiFrameViewProps) {
  const [values, setValues] = useState<Record<string, unknown>>({});
  const [submitting, setSubmitting] = useState(false);
  const [integrityOk, setIntegrityOk] = useState<boolean | null>(expectedHash ? null : true);

  useEffect(() => {
    if (!expectedHash) {
      setIntegrityOk(true);
      return;
    }
    let cancelled = false;
    setIntegrityOk(null);
    void verifyFrame(frame, expectedHash).then((ok) => {
      if (!cancelled) setIntegrityOk(ok);
    });
    return () => {
      cancelled = true;
    };
    // `frame`/`expectedHash` are the only meaningful inputs; a new pending
    // frame is always a new object reference from the caller.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [frame, expectedHash]);

  async function handleAction(action: string, _actionClass: ActionClass) {
    setSubmitting(true);
    try {
      await onDecide({ action, values });
    } catch (err) {
      // A rejected decision (e.g. a 400 the server caught) must stay
      // interactive so the human can correct and retry.
      setSubmitting(false);
      throw err;
    }
  }

  if (integrityOk === null) {
    return (
      <div className={`apex-ui${className ? ` ${className}` : ""}`}>
        <p className="apex-ui-text-caption">Verifying frame integrity…</p>
      </div>
    );
  }

  if (integrityOk === false) {
    return (
      <div className={`apex-ui${className ? ` ${className}` : ""}`}>
        <div className="apex-ui-integrity-warning" role="alert">
          This interface's content doesn't match what was recorded when it was presented and has
          not been rendered. Do not act on it — refresh and try again.
        </div>
      </div>
    );
  }

  return (
    <div className={`apex-ui${className ? ` ${className}` : ""}`}>
      {frame.title && (
        <p className="apex-ui-text-heading" role="heading" aria-level={1}>
          {frame.title}
        </p>
      )}
      {renderNode(frame.root, "root", {
        values,
        setValue: (name, value) => setValues((prev) => ({ ...prev, [name]: value })),
        disabled: Boolean(disabled) || submitting,
        onAction: (action, actionClass) => void handleAction(action, actionClass),
      })}
    </div>
  );
}
