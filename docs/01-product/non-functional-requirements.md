<!--
File: docs/01-product/non-functional-requirements.md
Document ID: PRD-005
-->

# Non-Functional Requirements

**Document ID:** PRD-005  
**File Path:** `docs/01-product/non-functional-requirements.md`  
**Version:** 1.0.0  
**Status:** Draft  
**Owner:** Product Team  
**Last Updated:** 2026-06-27

---

# 1. Purpose

This document defines the **non-functional requirements (NFRs)** — the quality
attributes the Wovyr AI Platform must meet. It elaborates [PRD §13](prd.md#13-non-functional-requirements)
and aggregates the per-service targets specified throughout the docs.

---

# 2. Conventions

```text
NFR-<attribute>-<n>  ·  measured by <metric>  ·  target <value>
```

Targets are validated by [performance](../15-testing/performance-tests.md),
[chaos](../15-testing/chaos-testing.md), and [security](../15-testing/security-testing.md)
testing.

---

# 3. Performance

| ID | Requirement | Target |
|----|-------------|--------|
| NFR-PERF-1 | API latency | p95 < 200 ms |
| NFR-PERF-2 | LLM Gateway overhead (non-cached) | p95 < 8 ms ([gateway NFRs](../05-llm-gateway/overview.md#9-non-functional-requirements)) |
| NFR-PERF-3 | Memory warm retrieval | p95 < 30 ms ([memory NFRs](../06-memory-engine/overview.md#10-non-functional-requirements)) |
| NFR-PERF-4 | Tool warm sandbox start | p95 < 20 ms ([tool NFRs](../07-tool-runtime/overview.md#9-non-functional-requirements)) |

---

# 4. Reliability & Availability

| ID | Requirement | Target |
|----|-------------|--------|
| NFR-REL-1 | Core service availability | 99.99% |
| NFR-REL-2 | Durable workflow execution (survive restart) | No lost executions ([checkpointing](../03-workflow-engine/checkpointing-specification.md)) |
| NFR-REL-3 | Graceful degradation on dependency failure | Validated by [chaos](../15-testing/chaos-testing.md) |
| NFR-REL-4 | Provider failover success when any healthy | > 99% ([resilience](../05-llm-gateway/resilience.md)) |

---

# 5. Scalability

| ID | Requirement | Target |
|----|-------------|--------|
| NFR-SCALE-1 | Horizontal scaling of stateless services | Linear with replicas |
| NFR-SCALE-2 | Memory corpus | Billions of records ([memory](../06-memory-engine/overview.md#10-non-functional-requirements)) |
| NFR-SCALE-3 | Concurrent tool executions | Thousands ([worker pool](../07-tool-runtime/worker-pool.md)) |
| NFR-SCALE-4 | Autoscaling reaction | < 30 s to add capacity |

---

# 6. Security

| ID | Requirement | Target |
|----|-------------|--------|
| NFR-SEC-1 | Cross-tenant data leakage | 0 (hard) ([authorization](../13-security/authorization.md#5-tenant-isolation)) |
| NFR-SEC-2 | Encryption in transit & at rest | Always ([encryption](../13-security/encryption.md)) |
| NFR-SEC-3 | Untrusted code isolation | Enforced sandboxing ([tool isolation](../07-tool-runtime/security-isolation.md)) |
| NFR-SEC-4 | Auditability of sensitive actions | 100% ([audit](../13-security/audit.md)) |

---

# 7. Maintainability & Extensibility

| ID | Requirement | Target |
|----|-------------|--------|
| NFR-MAINT-1 | Clean Architecture boundaries | Enforced ([ADR-0006](../17-adr/ADR-0006-clean-architecture.md)) |
| NFR-EXT-1 | Extend without core changes | Via [plugins](../08-plugin-sdk/index.md) |
| NFR-MAINT-2 | Test coverage on critical paths | High ([testing](../15-testing/index.md)) |

---

# 8. Observability

| ID | Requirement | Target |
|----|-------------|--------|
| NFR-OBS-1 | Logs, metrics, traces on every service | Always ([observability](../14-observability/index.md)) |
| NFR-OBS-2 | Request correlation across services | `request_id`/`trace_id` everywhere |
| NFR-OBS-3 | Cost observability | Per tenant/project/model ([cost](../05-llm-gateway/token-management.md)) |

---

# 9. Portability

| ID | Requirement | Target |
|----|-------------|--------|
| NFR-PORT-1 | Cloud-neutral deployment | Docker/K8s, self-host or managed ([deployment](../12-deployment/index.md)) |
| NFR-PORT-2 | Provider independence | No hard vendor lock-in ([provider SDK](../04-agent-framework/provider-sdk.md)) |
| NFR-PORT-3 | Air-gapped operation | Supported ([distribution](../08-plugin-sdk/distribution.md#9-air-gapped-distribution)) |

---

# 10. Compliance

| ID | Requirement | Target |
|----|-------------|--------|
| NFR-COMP-1 | Support SOC 2 / ISO 27001 / GDPR controls | Designed-for ([security index](../13-security/index.md#7-compliance-posture)) |
| NFR-COMP-2 | Data residency / retention controls | Configurable (ABAC + retention) |

---

# 11. Usability

| ID | Requirement | Target |
|----|-------------|--------|
| NFR-UX-1 | Dashboard accessibility | WCAG 2.1 AA ([dashboard](../10-dashboard/overview.md#11-accessibility--i18n)) |
| NFR-UX-2 | Time-to-first-agent | Minutes ([hello agent](../16-examples/hello-agent.md)) |

---

# 12. Related

- [`01-product/functional-requirements.md`](functional-requirements.md)
- [`01-product/acceptance-criteria.md`](acceptance-criteria.md)
- [`00-executive/success-metrics.md`](../00-executive/success-metrics.md)

---

# 13. Revision History

| Version | Date | Description |
|---------|------|-------------|
| 1.0.0 | 2026-06-27 | Initial Non-Functional Requirements |
