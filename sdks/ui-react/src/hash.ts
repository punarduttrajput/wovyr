/** Client-side frame integrity verification (RDR-403).
 *
 * `wovyr_ui::UiFrame::content_hash()` hashes the frame's **canonical**
 * (alphabetical-key) JSON form — the same shape every consumer sees via
 * `serde_json::Value` (whose map has no `preserve_order`), not the struct's
 * declaration-order `Serialize` output. To reproduce that hash here, object
 * keys must be sorted the same way before hashing: {@link canonicalStringify}
 * does that recursively, and {@link sha256Hex} hashes the result with the
 * standard Web Crypto API (available in browsers and modern Node — no extra
 * dependency, matching this package's zero-runtime-dependency stance).
 *
 * This check catches transport/render tampering and confirms the renderer is
 * showing exactly what the server's audit chain recorded (HIL-306) — it is
 * not a substitute for TLS, and known limitation: JavaScript's `number`
 * formatting doesn't always byte-match Rust's `f64` `Display` in edge cases
 * (very large/small magnitudes, trailing zeros); the vocabulary's typical
 * values (bounds like `min`/`max`, simple counts) don't hit those edges in
 * practice, but a mismatch here should not be read as proof of tampering
 * without also checking for that.
 */

/** Recursively sort object keys and re-stringify, matching the byte-wise
 * ordering `BTreeMap<String, _>` uses (which coincides with JS's default
 * string comparison for the ASCII field names this protocol uses). Arrays
 * keep their order — only object key order is canonicalized. */
export function canonicalStringify(value: unknown): string {
  return JSON.stringify(canonicalize(value));
}

function canonicalize(value: unknown): unknown {
  if (Array.isArray(value)) {
    return value.map(canonicalize);
  }
  if (value !== null && typeof value === "object") {
    const sorted: Record<string, unknown> = {};
    for (const key of Object.keys(value as Record<string, unknown>).sort()) {
      sorted[key] = canonicalize((value as Record<string, unknown>)[key]);
    }
    return sorted;
  }
  return value;
}

/** SHA-256 of `text`, lowercase hex — the same encoding
 * `wovyr_ui::UiFrame::content_hash()` produces. */
export async function sha256Hex(text: string): Promise<string> {
  const bytes = new TextEncoder().encode(text);
  const digest = await crypto.subtle.digest("SHA-256", bytes);
  return Array.from(new Uint8Array(digest))
    .map((b) => b.toString(16).padStart(2, "0"))
    .join("");
}

/** Recompute a frame's content hash the way the server did, and compare
 * against `expectedHash` (a {@link PendingUiFrame.frame_hash}). Returns
 * `true` when they match. */
export async function verifyFrame(frame: unknown, expectedHash: string): Promise<boolean> {
  const actual = await sha256Hex(canonicalStringify(frame));
  return actual === expectedHash;
}
