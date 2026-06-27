<!--
File: docs/19-implementation-guide/release-process.md
Document ID: IMPL-004
-->

# Release Process

**Document ID:** IMPL-004  
**File Path:** `docs/19-implementation-guide/release-process.md`  
**Version:** 1.0.0  
**Status:** Draft  
**Owner:** Engineering Team  
**Last Updated:** 2026-06-27

---

# 1. Purpose

This document defines how the Apex AI Platform is versioned and released — from a
merged change to signed, deployable artifacts.

---

# 2. Versioning

- The platform follows **semantic versioning** (`MAJOR.MINOR.PATCH`).
- Public contracts have explicit compatibility rules:
  [API](../09-api/overview.md#3-base-url--versioning),
  [Workflow DSL](../03-workflow-engine/workflow-dsl.md#23-workflow-versioning),
  [Plugin API](../08-plugin-sdk/versioning.md).
- Releases align to the [roadmap](../18-roadmap/index.md) milestones.

---

# 3. Branching

```text
main (always releasable)
  ├─ feature branches → PR → merge to main
  └─ release/x.y → stabilization for a minor release
```

`main` stays green and releasable; release branches stabilize a version.

---

# 4. Release Steps

```text
1. Cut release/x.y from main
2. Update CHANGELOG + version numbers
3. CI: build → test → package images + SDK
4. Sign artifacts (images, CLI, SDK) + generate provenance/SBOM
5. Publish to registries
6. Tag the release; publish release notes
7. Deploy via Helm/Terraform (staged: canary → full)
```

Artifact signing mirrors [plugin signing](../08-plugin-sdk/distribution.md#3-signing)
([build system](build-system.md#5-artifacts)).

---

# 5. Quality Gates

A release must pass:
- Unit + integration + contract tests ([testing](../15-testing/index.md)).
- Performance regression check vs. baseline
  ([performance](../15-testing/performance-tests.md#9-regression-gating)).
- Security scans + review for sensitive changes
  ([security testing](../15-testing/security-testing.md)).

---

# 6. Deployment & Rollback

- Staged rollout (canary → progressive) via
  [Helm](../12-deployment/helm.md#6-upgrades) with health-gated steps.
- DB [migrations](../12-deployment/docker-compose.md#5-initialization) run as
  pre-deploy jobs; forward-compatible.
- `helm rollback` reverts to a prior release if SLOs/alerts regress
  ([alerting](../14-observability/alerting.md)).

---

# 7. Changelog & Notes

- Conventional commits feed an automated CHANGELOG.
- Release notes call out features, fixes, deprecations, and migration steps.
- Deprecations announce a window before removal (compatibility commitments).

---

# 8. Hotfixes

Critical fixes branch from the affected release tag, follow the same gates
(expedited), and are signed/published like any release; revocation is available for
compromised artifacts ([distribution](../08-plugin-sdk/distribution.md#8-revocation)).

---

# 9. Related

- [`19-implementation-guide/build-system.md`](build-system.md)
- [`18-roadmap/index.md`](../18-roadmap/index.md)
- [`12-deployment/helm.md`](../12-deployment/helm.md)

---

# 10. Revision History

| Version | Date | Description |
|---------|------|-------------|
| 1.0.0 | 2026-06-27 | Initial Release Process |
