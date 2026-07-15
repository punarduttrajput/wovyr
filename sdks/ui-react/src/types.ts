/** A TypeScript mirror of `apex_ui`'s Rust frame protocol (PRD-005 UIP-101),
 * kept intentionally narrow and hand-written — there is no automatic schema
 * generation pipeline for this crate yet (its OpenAPI surface types the
 * `frame` field as opaque `unknown`). Field shapes here must track
 * `crates/apex-ui/src/frame.rs` by hand; if a variant's fields drift, expect
 * `renderNode`'s exhaustiveness check in `components/index.tsx` to catch it
 * at compile time. */

/** Emphasis for a {@link TextNode}. */
export type TextStyle = "body" | "heading" | "caption";

/** Semantic tone for a {@link BadgeNode}. */
export type Tone = "neutral" | "info" | "success" | "warning" | "danger";

/** The declared semantic class of a button's intent (UIP-103) — the
 * machine-readable half of the trust layer's intent-consistency checking
 * (GRD-203/204). */
export type ActionClass =
  | "confirm"
  | "approve"
  | "submit"
  | "cancel"
  | "reject"
  | "destructive"
  | "neutral";

/** Whether a decision taking `actionClass` affirms the frame's inputs (mirrors
 * `ActionClass::is_affirmative` — required-input enforcement applies only to
 * these classes, so a Cancel is never blocked by an empty form). */
export function isAffirmative(actionClass: ActionClass): boolean {
  return (
    actionClass === "confirm" ||
    actionClass === "approve" ||
    actionClass === "submit" ||
    actionClass === "destructive"
  );
}

export interface KeyValueEntry {
  key: string;
  value: string;
}

export interface SelectOption {
  value: string;
  label: string;
}

/** Runtime-stamped origin metadata (UIP-102) — never author-trusted; present
 * only after the trust layer has processed a frame. */
export interface Provenance {
  execution_id?: string;
  activity_id?: string;
  run_id?: string;
  model_id?: string;
  prompt_ref?: string;
}

/** The constrained component vocabulary (UIP-101). There is deliberately no
 * raw-HTML/script variant and no credential-input component — `renderNode`
 * cannot render what isn't in this union, which is the point (ADR-0011 §2.4).
 * A `type` this union doesn't recognize renders as {@link UnknownPlaceholder}
 * rather than being skipped (RDR-403). */
export type UiNode =
  | { type: "column"; children: UiNode[] }
  | { type: "row"; children: UiNode[] }
  | { type: "card"; title?: string; children: UiNode[] }
  | { type: "divider" }
  | { type: "text"; text: string; style?: TextStyle }
  | { type: "badge"; text: string; tone?: Tone }
  | { type: "key_value"; entries: KeyValueEntry[] }
  | { type: "image"; url: string; alt: string }
  | {
      type: "text_input";
      name: string;
      label: string;
      placeholder?: string;
      required?: boolean;
      multiline?: boolean;
    }
  | {
      type: "number_input";
      name: string;
      label: string;
      min?: number;
      max?: number;
      required?: boolean;
    }
  | { type: "select"; name: string; label: string; options: SelectOption[]; required?: boolean }
  | { type: "checkbox"; name: string; label: string; checked?: boolean }
  | { type: "button"; action: string; label: string; class?: ActionClass };

/** A complete agent-generated interface (UIP-101/102). */
export interface UiFrame {
  schema_version: string;
  title?: string;
  provenance?: Provenance;
  root: UiNode;
}

/** The `GET /api/v1/ui/frames[/{id}]` envelope — a validated frame plus the
 * metadata needed to verify and decide on it. */
export interface PendingUiFrame {
  frame_id: string;
  execution_id: string;
  activity_id: string;
  frame: UiFrame;
  frame_hash: string;
  policy_ref: string;
  created_at_ms: number;
}

/** The `POST /api/v1/ui/decisions/{frame_id}` request body (HIL-302/303). */
export interface UiDecision {
  action: string;
  values?: Record<string, unknown>;
}
