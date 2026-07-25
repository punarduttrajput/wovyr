<!--
File: docs/16-examples/vpn-agent.md
Document ID: EX-005
-->

# Example: VPN Operations Agent

**Document ID:** EX-005  
**File Path:** `docs/16-examples/vpn-agent.md`  
**Version:** 1.0.0  
**Status:** Draft  
**Owner:** Developer Relations Team  
**Last Updated:** 2026-06-27

---

# 1. Goal

Build an **operational agent** that manages VPN access by calling an external system
through a **plugin** — showing how a custom integration (tools + permissions +
secrets) is packaged and granted, then used by an agent.

This example demonstrates the [Plugin SDK](../08-plugin-sdk/index.md) end to end:
author → sign → publish → install → grant → use.

---

# 2. The Plugin

A `vpn` plugin ships tools that call a VPN provider's API.

`plugin.yaml` (excerpt):

```yaml
apiVersion: plugin.wovyr.io/v1
kind: Plugin
metadata: { name: acme/vpn, version: 1.0.0, publisher: acme }
compatibility: { platform_api: ">=1.0.0 <2.0.0" }
permissions:
  - net:egress:api.vpnprovider.com
  - secret:read:vpn-admin-token
capabilities:
  - { kind: tool, id: vpn.grant_access,  entry: capabilities/tools/grant,  sandbox: wasm }
  - { kind: tool, id: vpn.revoke_access, entry: capabilities/tools/revoke, sandbox: wasm }
  - { kind: tool, id: vpn.list_sessions, entry: capabilities/tools/list,   sandbox: wasm }
```

The plugin declares exactly the [permissions](../08-plugin-sdk/permissions.md) it
needs — egress to one host and one secret reference.

---

# 3. Build, Sign, Publish

```bash
wovyr plugin new vpn --kind tool
# implement capabilities/tools/*
wovyr plugin build && wovyr plugin test
wovyr plugin sign --key ~/.keys/acme.key
wovyr plugin publish --registry https://registry.wovyr.example.com
```

Packaging follows [distribution](../08-plugin-sdk/distribution.md) (signed,
SBOM, provenance).

---

# 4. Install & Grant

```bash
wovyr plugins install acme/vpn@1.0.0
PID=$(wovyr plugins list -o json | jq -r '.data[]|select(.name=="acme/vpn").id')
wovyr plugins grants add "$PID" --project netops \
  --permission net:egress:api.vpnprovider.com \
  --permission secret:read:vpn-admin-token
wovyr plugins enable "$PID"
```

The admin token is stored in the [secret vault](../13-security/secret-management.md)
and injected into the tool sandbox at run time — the plugin never sees the raw value
outside execution.

---

# 5. Define the Agent

`agents/vpn-ops.yaml`:

```yaml
kind: Agent
metadata: { name: vpn-ops }
spec:
  model_selector: { capability: chat, class: balanced }
  instructions: |
    You manage VPN access. Confirm the user and scope before granting.
    Use vpn.* tools. Never grant longer than requested.
  tools: [vpn.grant_access, vpn.revoke_access, vpn.list_sessions]
  policies: [require-approval-for-grants]
```

---

# 6. Run

```bash
wovyr agents run -f agents/vpn-ops.yaml --stream \
  --input '{"message":"Grant contractor alex@x.com access to staging for 8 hours."}'
```

```text
tool_call · vpn.list_sessions()           → []
delta     · "Granting alex@x.com staging access for 8h..."
tool_call · vpn.grant_access({user, scope:"staging", ttl:"8h"})  → ok
done      · tokens: 1.4k, cost_usd: 0.02, tool_calls: 2
```

---

# 7. Safety

- The `vpn.*` tools run sandboxed with egress allowed **only** to the VPN provider
  ([network isolation](../07-tool-runtime/security-isolation.md#5-network-isolation)).
- A policy requires human approval for grants (combine with a
  [workflow approval](customer-support.md#5-human-approval) for stricter control).
- Every action is [audited](../13-security/audit.md) (who granted what, when).

---

# 8. Takeaways

This pattern — **plugin provides governed integration, agent orchestrates it** —
generalizes to any external system (ticketing, cloud, CI/CD): package as a plugin,
declare least-privilege permissions, grant per project, and let agents/workflows
use it safely.

---

# 9. Related Documents

- [`08-plugin-sdk/index.md`](../08-plugin-sdk/index.md)
- [`08-plugin-sdk/permissions.md`](../08-plugin-sdk/permissions.md)
- [`16-examples/index.md`](index.md)

---

# 10. Revision History

| Version | Date | Description |
|---------|------|-------------|
| 1.0.0 | 2026-06-27 | Initial VPN Operations Agent example |
