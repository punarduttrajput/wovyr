# Wovyr Dashboard

The Wovyr platform web UI — an Angular SPA that consumes the platform API served by
`wovyr-server`. See [`docs/10-dashboard`](../docs/10-dashboard) for the full spec and
[`overview.md`](../docs/10-dashboard/overview.md) for the architecture (note: the
NestJS BFF is deferred; the SPA currently talks to `wovyr-server` directly).

## Surfaces

**Monitoring** · **Agent Studio** · **Workflow Builder** · **Memory Explorer** ·
**Marketplace** · **Settings** · **Sign in** are all built. Each is a lazy-loaded
route under `src/app/features/`.

- **Agent Studio** — agent CRUD (`/api/v1/agents`) + live `agents:stream` SSE test console.
- **Workflow Builder** — validate/submit/inspect workflow executions
  (`/api/v1/workflows*`) — the server has shipped these routes since `workflow_runner.rs`
  landed; this surface is not a placeholder.
- **Monitoring** — polls `/metrics`, `/healthz`, `/api/v1/workflows` every 5s; rows link
  to an **execution detail** view (`/executions/:id`).
- **Memory Explorer** — namespaces, browse, and hybrid search with explainable
  `score_breakdown` (`/api/v1/memory/*`).
- **Marketplace** — installed plugin catalog with enable/disable (`/api/v1/plugins*`).
- **Settings** — orgs / projects / members / quotas / webhooks (tenancy + webhooks API).
- **Sign in** (RM-GA-P4 OBS-805) — sets the tenant/principal the dashboard acts as,
  and (optionally) a real API key/JWT — see [Authentication](#authentication) below.

A ⌘K command palette (top-bar or `Ctrl/⌘+K`) jumps between surfaces.

## Authentication

There is no username/password login endpoint anywhere in the platform (`wovyr-server`
only *verifies* a pre-existing JWT/API key — see
[`auth.rs`](../crates/wovyr-server/src/auth.rs) — it never mints one from a password).
The **Sign in** page (`/login`) reflects that: it collects a tenant, a principal, and
optionally an already-minted credential, persisted in `localStorage` via
[`core/session.ts`](src/app/core/session.ts) — no more hardcoded, rebuild-to-change
`TENANT`/`PRINCIPAL` constants. [`tenant.interceptor.ts`](src/app/core/tenant.interceptor.ts)
always sends `X-Wovyr-Tenant`/`X-Wovyr-Principal`, and additionally sends
`Authorization: Bearer <value>` once a credential is set.

Which of the server's three `WOVYR_AUTH_MODE`s you run against changes what's required:

- **`disabled-loopback` (the default)** — the server trusts the two headers verbatim,
  but *still* refuses every request unless `WOVYR_ALLOW_ANONYMOUS=1` is set (SEC-101) —
  a real, easy-to-hit gotcha: `cargo run -p wovyr-cli -- dev` with no env vars 401s
  every dashboard call. Run it as:

  ```bash
  WOVYR_ALLOW_ANONYMOUS=1 WOVYR_PLATFORM_ADMINS=admin@wovyr.local \
    cargo run -p wovyr-cli -- dev        # binds 127.0.0.1:8080
  ```

  (`WOVYR_PLATFORM_ADMINS` authorizes the Settings surface's tenancy/webhook calls;
  `WOVYR_ALLOW_ANONYMOUS=1` is the dev-only opt-in `refuse_anonymous_on_non_loopback`
  enforces can never reach a non-loopback bind.) Leave the Sign-in page's API key
  field empty in this mode.
- **`apikey`** — real verification. Mint a key once:

  ```bash
  cargo run -p wovyr-cli -- auth create-key admin@wovyr.local
  # minted a new API key ... : <the-raw-key>
  WOVYR_AUTH_MODE=apikey cargo run -p wovyr-cli -- dev
  ```

  then paste `<the-raw-key>` into the Sign-in page. **Verified live**: `authenticate`
  accepts the bearer credential (a `GET /api/v1/tools` call — auth-only, no RBAC —
  returns `200`), and a route requiring tenancy membership the key's principal
  doesn't hold (e.g. `GET /api/v1/agents`) correctly still `403`s rather than
  `401`ing — proof the credential itself verified, and RBAC is a separate, later gate.
- **`jwt`** — same idea with a pre-issued bearer JWT instead of a minted API key.

## Run it locally

1. Start the platform server (mock provider when no `OPENAI_API_KEY`) — see
   [Authentication](#authentication) above for the env vars a given `WOVYR_AUTH_MODE`
   needs; the simplest local loop is `disabled-loopback` + `WOVYR_ALLOW_ANONYMOUS=1`.

2. Start the dashboard dev server (proxies `/api` → `127.0.0.1:8080` via
   `proxy.conf.json`):

   ```bash
   # from dashboard/
   npm install
   npm start                           # ng serve, http://localhost:4200
   ```

   `npm start`/`build`/`test`/`watch` all first run
   `scripts/ensure-ui-react-built.js` (a `pre*` npm hook), which builds
   [`../sdks/ui-react`](../sdks/ui-react) if its `dist/` is missing or stale.
   The dashboard depends on that package via `file:../sdks/ui-react` and
   resolves its compiled `dist/` output (the web component + styles), so this
   step is required, not optional — a clean checkout without it fails with
   `TS2307`/unresolved-import errors. Equivalently, from the repo root:
   `make dashboard-dev` / `dashboard-build` / `dashboard-test`.

3. Open the app, visit **Sign in** once to set your tenant/principal (and, for
   `apikey`/`jwt` mode, paste a credential), then go to **Agent Studio** and click
   **Run ▸** to stream a live agent run.

## Cross-origin deployment (RM-GA-P4 OBS-805)

When the dashboard is served from a different origin than `wovyr-server` (e.g. the
static build below, hosted separately), the server's CORS layer (Phase-1 SEC-204,
already fully implemented — `crates/wovyr-server/src/config.rs`'s `cors_layer`) needs
the dashboard's real origin in its allow-list:

```bash
WOVYR_CORS_ALLOWED_ORIGINS=https://dashboard.example.com cargo run -p wovyr-cli -- dev
```

No server-side code changes are needed — `cors_layer` already allows the
`X-Wovyr-Tenant`/`X-Wovyr-Principal`/`Authorization`/`Idempotency-Key`/`If-Match`
headers this dashboard sends and exposes `X-Request-Id`/`ETag`; an unconfigured
`WOVYR_CORS_ALLOWED_ORIGINS` means no CORS headers at all (same-origin only), never a
wildcard.

## Build

```bash
npm run build        # ng build (production) → dist/dashboard/browser/
```

A Docker build stage producing a static image of this output is at
[`deployment/docker/dashboard.Dockerfile`](../deployment/docker/dashboard.Dockerfile)
(nginx serving the SPA, with client-side routing fallback to `index.html`):

```bash
docker build -f deployment/docker/dashboard.Dockerfile -t wovyr-dashboard:dev .
docker run --rm -p 8081:80 wovyr-dashboard:dev
```

It is a separate image from `deployment/docker/Dockerfile` (the Rust `wovyr` binary)
and is **not** wired into `deployment/docker-compose.yml` as a running service yet —
see that Dockerfile's own header comment for what's proven vs. not.

## Layout

```
src/app/
├── app.{ts,html,scss}        # shell: nav rail + topbar + router-outlet + theme toggle
├── app.routes.ts             # lazy feature routes
├── core/                     # session (auth), tenant interceptor, theme, API types
├── shared/                   # command palette
└── features/
    ├── agent-studio/         # designer form + live SSE test console + agent CRUD
    ├── workflow-builder/     # validate/submit/inspect workflow executions
    ├── monitoring/           # golden-signal polling + execution list
    ├── execution-detail/     # one execution's status + event timeline
    ├── memory-explorer/      # namespaces, browse, hybrid search
    ├── marketplace/          # installed plugin catalog
    ├── settings/             # orgs / projects / members / quotas / webhooks
    ├── login/                # Sign in — sets tenant/principal/credential
    └── placeholder/          # stand-in for any future not-yet-built surface
```

The design system (tokens + shared primitives) lives in `src/styles.scss`, ported
from the approved design review (cobalt accent, mono-forward type, cool neutrals,
light/dark).
