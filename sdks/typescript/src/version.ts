/** DX-303: SDK ↔ server version awareness.
 *
 * The SDK's version tracks the platform release it was written against
 * (`0.3.x` ↔ wovyr-server 0.3.x) — same major.minor means same API surface.
 * `SDK_VERSION` is kept in lockstep with `package.json` by the release
 * process, and the unit suite asserts the two match so drift can't ship. */
export const SDK_VERSION = "0.4.1";

/** Human-readable warning when the SDK and server disagree on major.minor —
 * `null` when they agree (patch-level differences are compatible by policy)
 * or when either version is unparseable (a dev build like `"dry-run"` should
 * not spam warnings). */
export function versionSkew(sdkVersion: string, serverVersion: string): string | null {
  const sdk = parse(sdkVersion);
  const server = parse(serverVersion);
  if (!sdk || !server) return null;
  if (sdk.major === server.major && sdk.minor === server.minor) return null;
  return (
    `@wovyr/sdk ${sdkVersion} was written against wovyr-server ${sdk.major}.${sdk.minor}.x, ` +
    `but the server reports ${serverVersion} — routes and shapes may differ. ` +
    `Upgrade the ${sdk.major * 1000 + sdk.minor < server.major * 1000 + server.minor ? "SDK" : "server"} ` +
    `to matching major.minor.`
  );
}

function parse(v: string): { major: number; minor: number } | null {
  const m = /^(\d+)\.(\d+)\./.exec(v.trim());
  return m ? { major: Number(m[1]), minor: Number(m[2]) } : null;
}
