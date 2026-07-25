<!--
File: docs/01-product/personas.md
Document ID: PRD-007
-->

# User Personas

**Document ID:** PRD-007  
**File Path:** `docs/01-product/personas.md`  
**Version:** 1.0.1  
**Status:** Draft  
**Owner:** Product Team  
**Last Updated:** 2026-07-07

---

# 1. Purpose

This document defines the **user personas** for the Wovyr AI Platform — the archetypal users whose goals and pain points drive product decisions. It expands the target-user list in the [PRD §8](prd.md#8-target-users).

---

# 2. Persona Summary

| Persona | Primary need | Key surfaces |
|---------|--------------|--------------|
| App Developer | Ship an AI feature fast | [CLI](../11-cli/index.md), [SDK](../19-implementation-guide/build-system.md#6-the-rust-sdk), [Agent Studio](../10-dashboard/agent-studio.md) |
| AI Engineer | Build reliable agents/workflows | [Agent Framework](../04-agent-framework/index.md), [Workflow Builder](../10-dashboard/workflow-builder.md) |
| Platform/Ops Engineer | Operate it at scale, safely | [Deployment](../12-deployment/index.md), [Observability](../14-observability/index.md) |
| Security/Compliance | Govern access and data | [Security](../13-security/index.md), [Audit](../13-security/audit.md) |
| Plugin Developer | Extend & distribute capabilities | [Plugin SDK](../08-plugin-sdk/index.md), [Marketplace](../08-plugin-sdk/marketplace.md) |
| Engineering Leader | Adopt for the org | [Vision](../00-executive/vision.md), [Roadmap](../18-roadmap/index.md) |
| Researcher | Experiment with agent designs | [Agent Studio](../10-dashboard/agent-studio.md), [Examples](../16-examples/index.md) |

---

# 3. Personas

## 3.1 App Developer — "Dana"

- **Context:** Full-stack developer adding AI to a product; not an ML specialist.
- **Goals:** Add a working agent/feature in days; avoid wiring many services.
- **Pains:** Fragmented tooling, vendor lock-in, prompt-only "frameworks".
- **How Wovyr helps:** One platform; [hello agent](../16-examples/hello-agent.md) to
  production via [CLI](../11-cli/index.md)/[SDK](../19-implementation-guide/build-system.md#6-the-rust-sdk);
  provider independence via the [LLM Gateway](../05-llm-gateway/index.md).
- **Success:** First agent shipped quickly; swap models without code changes.

## 3.2 AI Engineer — "Amir"

- **Context:** Builds non-trivial agents and multi-step workflows.
- **Goals:** Grounded, reliable, debuggable agent behavior.
- **Pains:** Non-determinism, opaque failures, brittle RAG, no durability.
- **How Wovyr helps:** [Memory Engine](../06-memory-engine/index.md) for RAG,
  durable [workflows](../03-workflow-engine/overview.md), trace/step inspection in
  [Agent Studio](../10-dashboard/agent-studio.md#4-trace--step-inspector).
- **Success:** Agents that are testable ([workflow tests](../15-testing/workflow-tests.md))
  and observable.

## 3.3 Platform / Ops Engineer — "Priya"

- **Context:** Runs the platform for many teams.
- **Goals:** Scale, reliability, cost control, safe operations.
- **Pains:** Weak observability, hard scaling, runaway cost.
- **How Wovyr helps:** [Kubernetes/Helm](../12-deployment/index.md), autoscaling
  [tool workers](../07-tool-runtime/worker-pool.md), SLOs/alerts
  ([observability](../14-observability/index.md)), [quotas](../09-api/projects.md#5-quotas).
- **Success:** Meets SLOs; cost visible and bounded per tenant.

## 3.4 Security / Compliance — "Sam"

- **Context:** Owns security posture and audits.
- **Goals:** Least privilege, isolation, auditability, compliance.
- **Pains:** Untrusted AI tools, secret sprawl, missing audit trails.
- **How Wovyr helps:** [Sandboxed tools/plugins](../07-tool-runtime/security-isolation.md),
  [RBAC/ABAC](../13-security/rbac.md), [secret vault](../13-security/secret-management.md),
  tamper-evident [audit](../13-security/audit.md).
- **Success:** Passes [security testing](../15-testing/security-testing.md); zero
  cross-tenant leakage.

## 3.5 Plugin Developer — "Leo"

- **Context:** Builds integrations/tools, possibly to sell.
- **Goals:** Package capabilities once; distribute safely.
- **Pains:** No standard extension model; unsafe third-party code.
- **How Wovyr helps:** [Plugin SDK](../08-plugin-sdk/plugin-api.md), signing +
  [distribution](../08-plugin-sdk/distribution.md), [marketplace](../08-plugin-sdk/marketplace.md).
- **Success:** A signed plugin published and installed by others (see
  [VPN example](../16-examples/vpn-agent.md)).

## 3.6 Engineering Leader — "Morgan"

- **Context:** Decides whether the org adopts Wovyr.
- **Goals:** Productivity, control, no lock-in, sustainable roadmap.
- **Pains:** Build-vs-buy risk, fragmented stack, governance gaps.
- **How Wovyr helps:** [Vision](../00-executive/vision.md)/[business goals](../00-executive/business-goals.md),
  open-source + provider-neutral, clear [roadmap](../18-roadmap/index.md).
- **Success:** Teams self-serve under central governance.

## 3.7 Researcher — "Riya"

- **Context:** Explores agent/coordination designs.
- **Goals:** Rapid experimentation and comparison.
- **Pains:** Rebuilding infra per experiment; no eval harness.
- **How Wovyr helps:** Reusable runtime, [Agent Studio](../10-dashboard/agent-studio.md)
  + planned [evaluation](../15-testing/workflow-tests.md#4-evaluation-quality-testing),
  [multi-agent coordination](../04-agent-framework/multi-agent-coordination.md).
- **Success:** Compare agent versions on quality/cost/latency easily.

---

# 4. Anti-Personas (Not Targeted)

Per [PRD scope](prd.md#9-product-scope): consumers seeking a chat app, teams wanting
a low-code website builder, or users needing foundation-model *training* are not the
target.

---

# 5. Related

- [`01-product/prd.md`](prd.md) · [`01-product/user-stories.md`](user-stories.md)
- [`00-executive/vision.md`](../00-executive/vision.md)

---

# 6. Revision History

| Version | Date | Description |
|---------|------|-------------|
| 1.0.1 | 2026-07-07 | Renumbered from PRD-002 to PRD-007 — that ID collided with [`prd-future.md`](prd-future.md), which was independently assigned PRD-002 later without checking the sequence. Found during a project-wide doc review; no content changed |
| 1.0.0 | 2026-06-27 | Initial User Personas |
