# Apex Dashboard

The Apex platform web UI — an Angular SPA that consumes the platform API served by
`apex-server`. See [`docs/10-dashboard`](../docs/10-dashboard) for the full spec and
[`overview.md`](../docs/10-dashboard/overview.md) for the architecture (note: the
NestJS BFF is deferred; the SPA currently talks to `apex-server` directly).

## Surfaces

**Monitoring** (built) · **Agent Studio** (built) · Workflow Builder · Memory Explorer ·
Marketplace · **Settings** (built). Each is a lazy-loaded route under `src/app/features/`.

- **Agent Studio** — agent CRUD (`/api/v1/agents`) + live `agents:stream` SSE test console.
- **Monitoring** — polls `/metrics`, `/healthz`, `/api/v1/workflows` every 5s.
- **Settings** — orgs / projects / members / quotas / webhooks (tenancy + webhooks API).

Workflow Builder, Memory Explorer and Marketplace remain placeholders — the platform
server does not expose workflow-authoring, memory, or plugin HTTP routes yet (those are
CLI-only today), so those surfaces are blocked on backend endpoints.

## Run it locally

1. Start the platform server (mock provider when no `OPENAI_API_KEY`):

   ```bash
   # from the repo root
   cargo run -p apex-cli -- dev        # binds 127.0.0.1:8080

   # For the Settings surface, authorize the dashboard's principal as a platform
   # admin so tenancy/webhook calls aren't denied (see src/app/core/tenant.config.ts):
   APEX_PLATFORM_ADMINS=admin@apex.local cargo run -p apex-cli -- dev
   ```

2. Start the dashboard dev server (proxies `/api` → `127.0.0.1:8080` via
   `proxy.conf.json`):

   ```bash
   # from dashboard/
   npm start                           # ng serve, http://localhost:4200
   ```

Open the app, go to **Agent Studio**, and click **Run ▸** to stream a live agent run.

## Build

```bash
npm run build        # ng build (production)
```

## Layout

```
src/app/
├── app.{ts,html,scss}        # shell: nav rail + topbar + router-outlet + theme toggle
├── app.routes.ts             # lazy feature routes
├── core/                     # theme service, API types, safe-svg pipe
└── features/
    ├── agent-studio/         # designer form + live SSE test console + agent CRUD
    └── placeholder/          # stand-in for not-yet-built surfaces
```

The design system (tokens + shared primitives) lives in `src/styles.scss`, ported
from the approved design review (cobalt accent, mono-forward type, cool neutrals,
light/dark).
