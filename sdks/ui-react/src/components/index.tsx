import { useId } from "react";
import type { ActionClass, UiNode } from "../types.js";
import { isAffirmative } from "../types.js";

/** Form state threaded through the recursive render (lifted to
 * {@link UiFrameView} so a submit button can assemble every field's current
 * value) plus the callbacks a leaf node needs. Not a React context: `renderNode`
 * is a plain function called during another component's render, so ordinary
 * parameter-passing is simpler and just as correct here. */
export interface RenderCtx {
  values: Record<string, unknown>;
  setValue: (name: string, value: unknown) => void;
  /** True while a decision is in flight — every input/button disables. */
  disabled: boolean;
  onAction: (action: string, actionClass: ActionClass) => void;
}

/** Render one {@link UiNode}, recursing into containers. A `type` this
 * function doesn't recognize — including anything the protocol itself
 * doesn't define, since the vocabulary is closed by construction — renders as
 * a visible, inert {@link UnknownNode} rather than being silently skipped
 * (RDR-403): a host must never present a generated element it didn't
 * actually understand as if it had. */
export function renderNode(node: UiNode, key: string, ctx: RenderCtx): React.ReactNode {
  switch (node.type) {
    case "column":
      return (
        <div className="apex-ui-column" key={key} role="group">
          {node.children.map((child, i) => renderNode(child, `${key}.${i}`, ctx))}
        </div>
      );
    case "row":
      return (
        <div className="apex-ui-row" key={key} role="group">
          {node.children.map((child, i) => renderNode(child, `${key}.${i}`, ctx))}
        </div>
      );
    case "card":
      return (
        <section className="apex-ui-card" key={key} aria-label={node.title}>
          {node.title && (
            <p className="apex-ui-card-title" role="heading" aria-level={2}>
              {node.title}
            </p>
          )}
          {node.children.map((child, i) => renderNode(child, `${key}.${i}`, ctx))}
        </section>
      );
    case "divider":
      return <hr className="apex-ui-divider" key={key} />;
    case "text":
      return (
        <p
          className={`apex-ui-text${node.style ? ` apex-ui-text-${node.style}` : ""}`}
          key={key}
        >
          {node.text}
        </p>
      );
    case "badge":
      return (
        <span
          className={`apex-ui-badge${node.tone && node.tone !== "neutral" ? ` apex-ui-badge-${node.tone}` : ""}`}
          key={key}
        >
          {node.text}
        </span>
      );
    case "key_value":
      return (
        <dl className="apex-ui-keyvalue" key={key}>
          {node.entries.map((entry, i) => (
            <div key={`${key}.${i}`} style={{ display: "contents" }}>
              <dt>{entry.key}</dt>
              <dd>{entry.value}</dd>
            </div>
          ))}
        </dl>
      );
    case "image":
      return <img className="apex-ui-image" key={key} src={node.url} alt={node.alt} />;
    case "text_input":
      return (
        <TextInputNode key={key} node={node} ctx={ctx} />
      );
    case "number_input":
      return <NumberInputNode key={key} node={node} ctx={ctx} />;
    case "select":
      return <SelectNode key={key} node={node} ctx={ctx} />;
    case "checkbox":
      return <CheckboxNode key={key} node={node} ctx={ctx} />;
    case "button":
      return <ButtonNode key={key} node={node} ctx={ctx} />;
    default:
      return <UnknownNode key={key} node={node} />;
  }
}

function useFieldId(name: string): string {
  // useId gives a stable, SSR-safe unique suffix; the field `name` stays the
  // human-readable/debuggable part of the id.
  const reactId = useId();
  return `apex-ui-field-${name}-${reactId}`;
}

function TextInputNode({
  node,
  ctx,
}: {
  node: Extract<UiNode, { type: "text_input" }>;
  ctx: RenderCtx;
}) {
  const id = useFieldId(node.name);
  const value = (ctx.values[node.name] as string | undefined) ?? "";
  const Field = node.multiline ? "textarea" : "input";
  return (
    <div className="apex-ui-field">
      <label className="apex-ui-label" htmlFor={id}>
        {node.label}
        {node.required && (
          <span className="apex-ui-required-marker" aria-hidden="true">
            *
          </span>
        )}
      </label>
      <Field
        id={id}
        className={node.multiline ? "apex-ui-textarea" : "apex-ui-input"}
        type={node.multiline ? undefined : "text"}
        rows={node.multiline ? 3 : undefined}
        value={value}
        placeholder={node.placeholder}
        required={node.required}
        aria-required={node.required || undefined}
        disabled={ctx.disabled}
        onChange={(e) => ctx.setValue(node.name, e.target.value)}
      />
    </div>
  );
}

function NumberInputNode({
  node,
  ctx,
}: {
  node: Extract<UiNode, { type: "number_input" }>;
  ctx: RenderCtx;
}) {
  const id = useFieldId(node.name);
  const value = ctx.values[node.name];
  return (
    <div className="apex-ui-field">
      <label className="apex-ui-label" htmlFor={id}>
        {node.label}
        {node.required && (
          <span className="apex-ui-required-marker" aria-hidden="true">
            *
          </span>
        )}
      </label>
      <input
        id={id}
        className="apex-ui-input"
        type="number"
        min={node.min}
        max={node.max}
        value={typeof value === "number" ? value : ""}
        required={node.required}
        aria-required={node.required || undefined}
        disabled={ctx.disabled}
        onChange={(e) =>
          ctx.setValue(node.name, e.target.value === "" ? undefined : Number(e.target.value))
        }
      />
    </div>
  );
}

function SelectNode({ node, ctx }: { node: Extract<UiNode, { type: "select" }>; ctx: RenderCtx }) {
  const id = useFieldId(node.name);
  const value = (ctx.values[node.name] as string | undefined) ?? "";
  return (
    <div className="apex-ui-field">
      <label className="apex-ui-label" htmlFor={id}>
        {node.label}
        {node.required && (
          <span className="apex-ui-required-marker" aria-hidden="true">
            *
          </span>
        )}
      </label>
      <select
        id={id}
        className="apex-ui-select"
        value={value}
        required={node.required}
        aria-required={node.required || undefined}
        disabled={ctx.disabled}
        onChange={(e) => ctx.setValue(node.name, e.target.value)}
      >
        <option value="" disabled hidden>
          Select…
        </option>
        {node.options.map((opt) => (
          <option key={opt.value} value={opt.value}>
            {opt.label}
          </option>
        ))}
      </select>
    </div>
  );
}

function CheckboxNode({
  node,
  ctx,
}: {
  node: Extract<UiNode, { type: "checkbox" }>;
  ctx: RenderCtx;
}) {
  const id = useFieldId(node.name);
  const checked = (ctx.values[node.name] as boolean | undefined) ?? node.checked ?? false;
  return (
    <div className="apex-ui-checkbox-field">
      <input
        id={id}
        className="apex-ui-checkbox-input"
        type="checkbox"
        checked={checked}
        disabled={ctx.disabled}
        onChange={(e) => ctx.setValue(node.name, e.target.checked)}
      />
      <label className="apex-ui-label" htmlFor={id}>
        {node.label}
      </label>
    </div>
  );
}

function ButtonNode({ node, ctx }: { node: Extract<UiNode, { type: "button" }>; ctx: RenderCtx }) {
  const actionClass = node.class ?? "neutral";
  const variant =
    actionClass === "destructive"
      ? " apex-ui-button-destructive"
      : isAffirmative(actionClass)
        ? " apex-ui-button-affirmative"
        : "";
  return (
    <button
      type="button"
      className={`apex-ui-button${variant}`}
      disabled={ctx.disabled}
      onClick={() => ctx.onAction(node.action, actionClass)}
    >
      {node.label}
    </button>
  );
}

/** A visible, inert placeholder for a node type this renderer doesn't
 * recognize (RDR-403) — never skipped, never guessed at. */
function UnknownNode({ node }: { node: { type: string } }) {
  return (
    <div className="apex-ui-unknown" role="note">
      Unsupported element (<code>{node.type}</code>) — this frame includes a component this
      renderer version doesn't understand yet.
    </div>
  );
}
