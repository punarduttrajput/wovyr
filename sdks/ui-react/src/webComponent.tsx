import { createRoot, type Root } from "react-dom/client";
import { UiFrameView } from "./UiFrameView.js";
import type { UiDecision, UiFrame } from "./types.js";

/** `detail` of the `decide` event {@link ApexUiFrameElement} dispatches. The
 * element has no opinion on how a decision reaches the server — a listener
 * attaches the async outcome to `result`; if that promise rejects, the
 * element re-enables for retry exactly like {@link UiFrameView}'s `onDecide`
 * contract does for a React host:
 *
 * ```js
 * el.addEventListener("decide", (e) => {
 *   e.detail.result = apexClient.ui.decide(frameId, e.detail.decision);
 * });
 * ```
 *
 * Leaving `result` unset is treated as an immediate success. */
export interface ApexUiFrameDecideDetail {
  decision: UiDecision;
  result?: Promise<void>;
}

/** `<apex-ui-frame>` (RDR-402) — a framework-agnostic wrapper around
 * {@link UiFrameView} for hosts that aren't React themselves (the dashboard's
 * Angular shell, a plain HTML page, a CMS-embedded widget). Internally still
 * React, mounted into the element via `react-dom/client`'s `createRoot` — the
 * host never needs to know that.
 *
 * `frame`/`expectedHash`/`disabled` are set as **JS properties**, not HTML
 * attributes (a `UiFrame` doesn't serialize sensibly into an attribute
 * string) — `disabled` also mirrors the boolean `disabled` *attribute* for
 * hosts that prefer declarative markup (`<apex-ui-frame disabled>`).
 *
 * Registers itself under `customElements` on import — see
 * `registerApexUiFrameElement` if a host wants to defer that (e.g. to pick a
 * different tag name via a subclass) or needs the idempotency guard exposed
 * directly. Rendering requires the caller to also load this package's
 * `styles.css` (unchanged from the plain-React usage — this wrapper renders
 * into light DOM, not a shadow root, so host-level styling still applies). */
export class ApexUiFrameElement extends HTMLElement {
  static readonly tagName = "apex-ui-frame";
  static readonly observedAttributes = ["disabled"];

  #root: Root | null = null;
  #frame: UiFrame | null = null;
  #expectedHash: string | undefined;
  #disabled = false;

  get frame(): UiFrame | null {
    return this.#frame;
  }
  set frame(value: UiFrame | null) {
    this.#frame = value;
    this.#render();
  }

  get expectedHash(): string | undefined {
    return this.#expectedHash;
  }
  set expectedHash(value: string | undefined) {
    this.#expectedHash = value;
    this.#render();
  }

  get disabled(): boolean {
    return this.#disabled;
  }
  set disabled(value: boolean) {
    this.#disabled = Boolean(value);
    this.#render();
  }

  connectedCallback(): void {
    this.#root ??= createRoot(this);
    this.#render();
  }

  disconnectedCallback(): void {
    // Unmount synchronously on detach — a lingering root would keep
    // `onDecide` closures (and their captured `frame`) alive after the host
    // has removed the element from the document.
    this.#root?.unmount();
    this.#root = null;
  }

  attributeChangedCallback(name: string, _oldValue: string | null, newValue: string | null): void {
    if (name === "disabled") {
      this.#disabled = newValue !== null;
      this.#render();
    }
  }

  #render(): void {
    if (!this.#root || !this.#frame) return;
    this.#root.render(
      <UiFrameView
        frame={this.#frame}
        expectedHash={this.#expectedHash}
        disabled={this.#disabled}
        onDecide={(decision) => this.#emitDecide(decision)}
      />,
    );
  }

  async #emitDecide(decision: UiDecision): Promise<void> {
    const detail: ApexUiFrameDecideDetail = { decision };
    this.dispatchEvent(
      new CustomEvent<ApexUiFrameDecideDetail>("decide", {
        detail,
        bubbles: true,
        composed: true,
      }),
    );
    // Rethrowing a rejection here is what makes `UiFrameView` re-enable the
    // form for retry — matches the plain-React `onDecide` contract exactly.
    await detail.result;
  }
}

/** Registers `<apex-ui-frame>` under `customElements`, idempotently (safe to
 * call more than once — a second `define` for the same tag throws). Imported
 * modules already call this once at load time; exported separately so a host
 * can call it explicitly (e.g. after a subclass swaps in a different tag
 * name) without re-importing. */
export function registerApexUiFrameElement(): void {
  if (!customElements.get(ApexUiFrameElement.tagName)) {
    customElements.define(ApexUiFrameElement.tagName, ApexUiFrameElement);
  }
}

registerApexUiFrameElement();

declare global {
  interface HTMLElementTagNameMap {
    "apex-ui-frame": ApexUiFrameElement;
  }
}
