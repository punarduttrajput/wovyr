<!--
File: docs/14-observability/logging.md
Document ID: OBS-001
-->

# Observability: Logging

**Document ID:** OBS-001  
**File Path:** `docs/14-observability/logging.md`  
**Version:** 1.0.0  
**Status:** Draft  
**Owner:** Platform Operations Team  
**Last Updated:** 2026-06-27

---

# 1. Purpose

This document defines **structured logging** standards for all Wovyr AI Platform services — format, levels, correlation, and privacy.

---

# 2. Format

Logs are **structured JSON**, one event per line:

```json
{
  "ts": "2026-06-27T10:00:00.123Z",
  "level": "info",
  "service": "tool-runtime",
  "msg": "execution completed",
  "request_id": "req_01H...",
  "trace_id": "trace_01H...",
  "tenant": "acme",
  "execution_id": "exec_01H...",
  "duration_ms": 84
}
```

Structured logs are queryable and correlate to traces/metrics via `request_id` /
`trace_id`.

---

# 3. Levels

| Level | Use |
|-------|-----|
| `error` | Action failed; needs attention |
| `warn` | Degraded/unexpected, handled |
| `info` | Lifecycle and key transitions |
| `debug` | Detailed diagnostics (non-prod default off) |
| `trace` | Very verbose (opt-in) |

Level is configurable per service via `WOVYR_LOG`
([deployment config](../12-deployment/docker.md#5-configuration)), falling back to
`RUST_LOG` and defaulting to `warn`. It accepts the full `EnvFilter` directive syntax,
so a per-target level works too — `WOVYR_LOG=info,hyper_util=off` keeps the platform's
own lifecycle lines without the HTTP connection-pool noise. Beware that a bare word
`EnvFilter` doesn't recognize as a level parses as a *target* directive at `trace`
(`WOVYR_LOG=Warning` means `Warning=trace`, a firehose — not an error).

The filter applies to **every** sink, OTLP export included. Since the instrumented
hot-path spans (`agent.run`, `gateway.chat`, `workflow.activity`, `api.*`) are
`info`-level, an OTLP deployment must set `WOVYR_LOG=info` or nothing is exported at
the `warn` default.

---

# 4. Required Fields

Every log line includes: `ts`, `level`, `service`, `msg`, and — where applicable —
`request_id`, `trace_id`, `tenant`, `principal`, and the relevant resource id.
Consistent fields make cross-service queries possible.

---

# 5. Correlation

The `request_id` is generated at the API Gateway and propagated through every
downstream call ([API observability](../09-api/overview.md#14-observability)); the
`trace_id` ties logs to [traces](tracing.md). One ID reconstructs an entire request
across services.

---

# 6. Privacy & Security

- **No secrets** in logs (masked; see [secret management](../13-security/secret-management.md#9-masking)).
- **PII masked** per classification ([encryption](../13-security/encryption.md#7-pii-handling)).
- Tool inputs are hashed, not logged raw, unless policy requires retention
  ([tool audit](../07-tool-runtime/security-isolation.md#11-audit)).

Operational logs are distinct from security [audit](../13-security/audit.md).

---

# 7. Pipeline

```text
Service (stdout JSON) ─► collector ─► log store (indexed) ─► query/UI
```

In Kubernetes, a node agent ships stdout to the aggregator
([deployment](../12-deployment/kubernetes.md#10-observability)). Logs are indexed
for search and linked from dashboards/traces.

---

# 8. Retention & Sampling

- Retention is tiered (recent hot, older archived/cold).
- High-volume `debug`/`trace` may be sampled; `error`/`warn` are never dropped.
- Retention is shorter than security audit by design.

---

# 9. Standards for Developers

- Log **events**, not prose; put variables in fields, not the message.
- One log per significant state transition; avoid log spam in hot loops.
- Never log credentials, tokens, or raw PII.

(Cross-references the planned
[Coding Standards](../19-implementation-guide/coding-standards.md).)

---

# 10. Dependencies

- [`14-observability/tracing.md`](tracing.md)
- [`13-security/audit.md`](../13-security/audit.md)
- [`09-api/overview.md`](../09-api/overview.md)

---

# 11. Related Documents

- [`14-observability/metrics.md`](metrics.md)
- [`14-observability/index.md`](index.md)

---

# 12. Revision History

| Version | Date | Description |
|---------|------|-------------|
| 1.0.0 | 2026-06-27 | Initial Logging specification |
