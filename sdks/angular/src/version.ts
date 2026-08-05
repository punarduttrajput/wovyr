/** DX-303: SDK ↔ server version awareness.
 *
 * The SDK's version tracks the platform release it was written against.
 * `@wovyr/angular` is a new client package (its own 0.1.x lineage) but is
 * written against the platform release named by `TARGET_SERVER_VERSION`
 * below — so the version handshake compares
 * the *server's* reported version against `TARGET_SERVER_VERSION`, not against
 * this package's own `SDK_VERSION` (which would be 0.1.x and would always
 * falsely warn). `SDK_VERSION` is kept in lockstep with `package.json` by the
 * release process; the unit suite asserts the two match so drift can't ship. */
export const SDK_VERSION = "0.1.0";

/** The platform (`wovyr-server`) release this SDK targets. Same major.minor as
 * the server means the same API surface (DX-303). */
export const TARGET_SERVER_VERSION = "0.4.1";

/** Human-readable warning when the SDK and server disagree on major.minor —
 * `null` when they agree (patch-level differences are compatible by policy)
 * or when either version is unparseable (a dev build like `"dry-run"` should
 * not spam warnings). */
export function versionSkew(serverVersion: string): string | null {
  const server = parse(serverVersion);
  const target = parse(TARGET_SERVER_VERSION);
  if (!server || !target) return null;
  if (server.major === target.major && server.minor === target.minor) return null;
  return (
    `@wovyr/angular ${SDK_VERSION} was written against wovyr-server ${target.major}.${target.minor}.x, ` +
    `but the server reports ${serverVersion} — routes and shapes may differ. ` +
    `Upgrade the ${server.major * 1000 + server.minor < target.major * 1000 + target.minor ? "SDK" : "server"} ` +
    `to matching major.minor.`
  );
}

function parse(v: string): { major: number; minor: number } | null {
  const m = /^(\d+)\.(\d+)\./.exec(v.trim());
  return m ? { major: Number(m[1]), minor: Number(m[2]) } : null;
}
