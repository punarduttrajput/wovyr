# apex Helm chart

Deploys the **real, single-binary v0.1 topology** — one `apex` server +
Postgres + Qdrant, the same shape as
[`deployment/docker-compose.yml`](../../docker-compose.yml) — to Kubernetes.

This is **not** the multi-service split described in
[`docs/12-deployment/kubernetes.md`](../../../docs/12-deployment/kubernetes.md)
and [`helm.md`](../../../docs/12-deployment/helm.md) (separate api-gateway/
agent-runtime/workflow-engine/… Deployments with HPAs, a tool-worker
DaemonSet, Redis, NATS, MinIO). That's the long-term aspirational
architecture; today there is one binary (`apex-cli`), and this chart deploys
exactly that, honestly.

## What this chart does not prove

- **Never deployed against a real cluster.** Authored and validated offline
  in a sandbox with no live Kubernetes cluster — `helm lint`/`helm
  template`/`kubectl apply --dry-run=client` all passed (client-side schema
  validation only, no apiserver involved), but nothing here has been applied
  to a real cluster, chaos-tested, or load-tested. Treat it the same way the
  project already treats its egress-lockdown feature: "authored/tested
  offline, never verified against a real [cluster] — watch this on first
  real use."
- **`/healthz` is a pure liveness check.** `crates/apex-server/src/lib.rs`'s
  handler returns `{"status":"ok","version":...}` unconditionally — it does
  not check Postgres/Qdrant connectivity. Both `livenessProbe` and
  `readinessProbe` on the apex StatefulSet point at it, so `readinessProbe`
  here only proves the process started, not that its dependencies are
  reachable. There is no `/readyz` endpoint in `apex-server` today. The
  `wait-for-postgres` init container is what actually gates the pod's first
  start on Postgres being reachable (the K8s equivalent of compose's
  `depends_on: condition: service_healthy`) — it does not re-check on every
  liveness/readiness poll afterward.
- **Qdrant is CLI-only, same as in compose.** The embedded `apex-server`
  never routes memory reads/writes through Postgres or Qdrant — it always
  uses its local file store under `/home/apex/.apex/memory`. Qdrant here
  backs `APEX_MEMORY_QDRANT_URL` for `apex memory put/query` CLI commands run
  against this cluster (`kubectl exec` into the apex pod, or a separate Job).
  Set `qdrant.enabled: false` if nothing runs those commands.
- **No horizontal scaling.** `apex` is a `StatefulSet` with `replicaCount`
  fixed at 1 in `values.yaml` — its durable state
  (`tenancy`/`kms`/`secrets`/`webhooks`/`workflows`/`memory`/`audit`) is local
  files on a PVC, not shared or replicated. Scaling this out needs the
  platform to support sharding/multi-region first (not built yet).
- **No image registry wired up.** `values.yaml`'s `apex.image.repository`/
  `tag` are placeholders (`apex:dev`) — push
  [`deployment/docker/Dockerfile`](../docker/Dockerfile)'s image (built with
  `--build-arg FEATURES=tiered-memory,postgres` to match this chart's
  Postgres/Qdrant wiring) to a real registry and set these before deploying.
- **No TLS termination or ingress of its own (RM-GA-P1 SEC-202).** The pod
  binds `0.0.0.0`, so `apex_server::serve()` refuses to start without either
  real TLS (not templated by this chart) or `apex.env.tlsTerminatedUpstream`
  acknowledging a proxy/mesh sidecar handles it — defaulted to `"1"` here,
  since this chart has no Ingress/Gateway resource. Put a real TLS-terminating
  proxy in front before exposing this outside the cluster. Real auth
  (RM-GA-P1 SEC-101) is also required now — `apex.env.authMode` defaults to
  `apikey`; mint a key post-deploy with `kubectl exec -it <pod> -- apex auth
  create-key <principal>` (persisted to the same PVC the server reads).

## Installing

```bash
# Generate a real KMS root key first — the default is empty, which makes
# apex-server fall back to a fully ephemeral in-process key (logged loudly),
# fine for a smoke test but not for anything you want to survive a restart.
KMS_KEY=$(openssl rand -hex 32)

helm install my-apex ./deployment/helm/apex \
  --set apex.image.repository=<your-registry>/apex \
  --set apex.image.tag=<your-tag> \
  --set apex.secrets.kmsRootKey="$KMS_KEY" \
  --set postgres.password=<a-real-password> \
  --set apex.secrets.openaiApiKey=<your-key-or-leave-empty-for-the-mock-provider>
```

In production, prefer pointing an external-secrets/sealed-secrets controller
at the `<release>-apex-secrets` Secret's keys instead of passing plaintext
values on the command line.

## Value → compose mapping

| `values.yaml` | `docker-compose.yml` equivalent |
|---|---|
| `apex.image.*` | `apex.build` (this chart doesn't build the image — push it yourself) |
| `apex.port` (8080) | `apex.ports: ["8080:8080"]` |
| `postgres.user/password/database` (`apex`/`apex`/`apex`) | `postgres.environment.POSTGRES_*` |
| `qdrant.enabled` | `qdrant` service (always present in compose; toggleable here) |
| `apex.secrets.kmsRootKey` | `APEX_KMS_ROOT_KEY` env var (unset in compose too — same ephemeral-key gap there) |
| `apex.secrets.openaiApiKey` | `OPENAI_API_KEY` env var (unset in compose too — mock provider) |

## Validating without a cluster

```bash
helm lint deployment/helm/apex
helm template deployment/helm/apex \
  --set postgres.password=x --set apex.secrets.kmsRootKey=y \
  | kubectl apply --dry-run=client -f -
```

Both commands are client-side only — no live apiserver required.
