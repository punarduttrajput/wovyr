<!--
File: docs/13-security/secret-management.md
Document ID: SEC-005
-->

# Security: Secret Management

**Document ID:** SEC-005  
**File Path:** `docs/13-security/secret-management.md`  
**Version:** 1.0.0  
**Status:** Draft  
**Owner:** Security Team  
**Last Updated:** 2026-06-27

---

# 1. Purpose

This document defines how the Apex AI Platform stores, references, injects, and rotates **secrets** — provider API keys, database credentials, signing keys, and integration tokens — without ever exposing them in code, config, logs, or responses.

---

# 2. Principles

1. **Reference, never embed** — components hold secret *references*, not values.
2. **Centralized vault** — secrets live in one managed store.
3. **Least privilege** — each workload reads only its own secrets.
4. **Short-lived where possible** — prefer minted, scoped, expiring credentials.
5. **Never logged** — secrets are masked everywhere, including audit.
6. **Rotatable** — rotation without redeploying consumers.

---

# 3. Secret Vault

A managed vault (cloud secrets manager or HashiCorp Vault) is the single source of
truth. Secrets are addressed by reference:

```text
secret://acme/github-token
secret://platform/llm/openai-key
```

References appear in manifests, plugin
[permissions](../08-plugin-sdk/permissions.md), and configs; the value is resolved
at runtime by an authorized workload identity.

---

# 4. Workload Access

```text
Workload (with IAM/workload identity)
   │  authenticates to vault
   ▼
Vault checks the workload may read the referenced secret
   │
   ▼
Returns value (in-memory) → used → never persisted
```

Access is scoped via IAM (e.g. IRSA/workload identity from
[Terraform](../12-deployment/terraform.md#7-secrets--iam)); a service can read only
the secrets it is bound to.

---

# 5. Injection into Tools & Plugins

Tools and plugins never receive raw long-lived credentials directly:

- The Tool Runtime / Plugin host resolves the secret reference and injects it into
  the sandbox **in memory** (env/tmpfs), zeroed on teardown
  ([Tool Runtime secrets](../07-tool-runtime/security-isolation.md#7-secret-management)).
- Where the provider supports it, **short-lived scoped credentials** are minted per
  execution rather than handing over the master secret.
- A plugin must hold a `secret:read:<ref>` [grant](../08-plugin-sdk/permissions.md)
  to access a secret.

---

# 6. Provider Keys

LLM provider keys are stored as references and consumed only by the
[LLM Gateway](../05-llm-gateway/overview.md#11-security); callers never see them.
This centralizes provider credentials behind one governed service.

---

# 7. Rotation

| Secret | Rotation |
|--------|----------|
| Provider/API keys | Scheduled; dual-key window for zero-downtime |
| DB credentials | Rotated via vault dynamic secrets where supported |
| Signing keys | Rotated; old keys retained for verification window |
| Service identities | Short-lived, continuously rotated |

Consumers reading by reference pick up rotated values automatically.

---

# 8. Revocation & Incident Response

- Any secret can be revoked immediately, disabling dependent capabilities.
- On suspected leak: rotate the secret, audit `secrets_used`
  ([tool audit](../07-tool-runtime/security-isolation.md#11-audit)), and revoke
  affected grants ([runbook](../07-tool-runtime/observability-ops.md#9-runbooks)).

---

# 9. Masking

Secrets are masked in logs, traces, error messages, and audit records. Audit
references the secret by id, never its value
([Tool audit example](../07-tool-runtime/security-isolation.md#11-audit)).

---

# 10. Audit

Secret reads, rotations, and revocations are audited per [audit.md](audit.md) with
the workload identity and reference (not value).

---

# 11. Dependencies

- [`13-security/encryption.md`](encryption.md)
- [`07-tool-runtime/security-isolation.md`](../07-tool-runtime/security-isolation.md)
- [`08-plugin-sdk/permissions.md`](../08-plugin-sdk/permissions.md)
- [`12-deployment/terraform.md`](../12-deployment/terraform.md)

---

# 12. Related Documents

- [`13-security/authentication.md`](authentication.md)
- [`13-security/audit.md`](audit.md)

---

# 13. Revision History

| Version | Date | Description |
|---------|------|-------------|
| 1.0.0 | 2026-06-27 | Initial Secret Management specification |
