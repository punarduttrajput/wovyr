<!--
File: docs/07-tool-runtime/http.md
Document ID: TRT-107
-->

# Built-in Tool: HTTP

**Document ID:** TRT-107  
**File Path:** `docs/07-tool-runtime/http.md`  
**Version:** 1.0.0  
**Status:** Draft  
**Owner:** AI Platform Team  
**Last Updated:** 2026-06-27

---

# 1. Purpose

The `http.request` built-in tool lets agents call external REST/HTTP APIs through a
**controlled egress proxy**, with destinations restricted to explicitly granted
hosts. It is the most common integration tool.

---

# 2. Operations

| Tool | Description |
|------|-------------|
| `http.request` | Perform an HTTP request and return the response |

---

# 3. Schema

```json
// input
{ "method": "GET", "url": "https://api.example.com/orders/123",
  "headers": { "Accept": "application/json" }, "timeout_ms": 30000 }
// output
{ "status_code": 200, "headers": { "...": "..." }, "body": "{...}" }
```

Secrets (e.g. API tokens) are referenced, not inlined:
`headers.Authorization: "secret://acme/example-token"` is resolved at run time.

---

# 4. Permissions

```text
net:egress:api.example.com        (per host)
secret:read:<token-ref>           (if auth needed)
```

Egress is **default-deny**; each destination host must be granted
([network isolation](security-isolation.md#5-network-isolation)). Wildcard egress is
flagged as broad.

---

# 5. Sandbox & Safety

- Requests route through an egress proxy enforcing the allowlist and (for untrusted
  tools) inspection.
- DNS is restricted to allowed domains to prevent exfiltration.
- Response size and timeout are bounded.
- Auth secrets injected in-memory, never logged
  ([secrets](security-isolation.md#7-secret-management)).

---

# 6. Determinism & Caching

`GET`/`HEAD` may be cached briefly when the tool declares the call idempotent;
non-idempotent methods are never cached
([caching rules](worker-pool.md#11-caching)).

---

# 7. Example

```bash
apex tools invoke http.request --input '{"method":"GET","url":"https://api.example.com/health"}'
```

The basis for most plugin integrations (e.g. [VPN agent](../16-examples/vpn-agent.md)).

---

# 8. Related

- [`07-tool-runtime/security-isolation.md`](security-isolation.md)
- [`07-tool-runtime/execution-api.md`](execution-api.md)

---

# 9. Revision History

| Version | Date | Description |
|---------|------|-------------|
| 1.0.0 | 2026-06-27 | Initial HTTP tool spec |
