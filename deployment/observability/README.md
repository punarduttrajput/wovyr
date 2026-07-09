# Apex observability starter (RM-GA-P4 OBS-803)

A **starter** Prometheus alert-rule file and Grafana dashboard for the single-node
Apex appliance, built directly on the real metric series the server emits — not the
aspirational SLO/dashboard catalog in `docs/14-observability/{alerting,dashboards}.md`
(those documents describe a future multi-service fleet's full observability stack;
this directory is what actually exists to point a Prometheus/Grafana pair at today).

## Files

- **`alerts.yml`** — Prometheus alert rules: API error rate (warning/critical),
  latency SLO burn (p95 > 1s), a contract-drift detector on the `route="unmatched"`
  label (RM-GA-P4 OBS-801's fallback for a route missing from `hardening::ROUTE_LABELS`),
  target-down, an LLM daily-cost-spike heuristic, and a webhook delivery-failure-rate
  alert. Validated with `promtool check rules alerts.yml` (7 rules, all pass) — a
  portable `promtool`/`prometheus` binary was downloaded from the project's GitHub
  releases specifically to validate this offline, the same approach this repo's Helm
  chart used for `kubeconform` (see `deployment/helm/apex/README.md`).
- **`dashboard.json`** — a Grafana dashboard (schema v39): request rate/error-rate/
  latency-percentiles by route, an unmatched-route panel, LLM token/cost panels, a
  cache-savings stat, and webhook delivery outcomes. Import directly into Grafana
  with a Prometheus datasource named `Prometheus` (or edit each panel's `datasource`
  field to match an existing one). Validated as well-formed JSON; **never rendered in
  a live Grafana instance** (none exists in this dev environment) — the same
  "offline-validated, not live-verified" caveat this repo's Helm chart and
  docker-compose slices already carry, stated explicitly rather than implied.

## What this is not

Neither file is wired into `deployment/docker-compose.yml` or the Helm chart — there
is no Prometheus/Grafana *service* in either, only the `apex` binary's `/metrics`
endpoint for an operator-supplied Prometheus to scrape. Adding actual Prometheus/
Grafana services (and provisioning these files into them automatically) is future
work, matching the "reliability first slice" scoping already used for compose/Helm.

## Metric series these rules/panels reference

All confirmed live in `crates/apex-server/src/hardening.rs` (`track_metrics`,
RM-GA-P4 OBS-801) and `config.rs` (`MetricsCostObserver`)/`webhooks.rs`:

| Metric | Type | Labels |
|---|---|---|
| `apex_api_requests_total` | counter | `route`, `method`, `status` |
| `apex_api_request_duration_seconds` | histogram | `route`, `method` |
| `apex_llm_tokens_total` | counter | `model`, `type` (`prompt`/`completion`) |
| `apex_llm_cost_usd_total` | counter | `model` |
| `apex_cache_savings_usd_total` | counter | `subsystem` |
| `apex_webhook_deliveries_total` | counter | `result` (`delivered`/`failed`) |

`up{job="apex"}` (`ApexTargetDown`) is a standard Prometheus-generated series for
any scrape job named `apex` — set the job name in the caller's own scrape config.
