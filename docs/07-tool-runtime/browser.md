<!--
File: docs/07-tool-runtime/browser.md
Document ID: TRT-108
-->

# Built-in Tool: Browser

**Document ID:** TRT-108  
**File Path:** `docs/07-tool-runtime/browser.md`  
**Version:** 1.0.0  
**Status:** Draft  
**Owner:** AI Platform Team  
**Last Updated:** 2026-06-27

---

# 1. Purpose

The `browser.*` built-in tools give agents a **headless browser** for tasks that
need rendered pages — scraping JS-heavy sites, filling forms, capturing
screenshots. High-resource and high-risk; sandboxed strongly.

---

# 2. Operations

| Tool | Description |
|------|-------------|
| `browser.navigate` | Open a URL, return rendered text/DOM |
| `browser.click` | Click an element |
| `browser.fill` | Fill form fields |
| `browser.screenshot` | Capture a screenshot |
| `browser.extract` | Extract structured content via selectors |

---

# 3. Schema (example: `browser.navigate`)

```json
// input
{ "url": "https://example.com/docs", "wait_for": "networkidle", "timeout_ms": 30000 }
// output
{ "url": "...", "title": "...", "text": "...", "status": 200 }
```

Screenshots return an artifact reference stored in object storage, not inline bytes.

---

# 4. Permissions

```text
net:egress:<domain>     (per allowed site)
browser:use
```

Egress is restricted to granted domains; broad browsing requires explicit, broad
grants flagged as high-risk.

---

# 5. Sandbox & Safety

- The browser runs in a **strong sandbox** (gVisor/microVM) on the untrusted worker
  pool due to its attack surface ([backends](sandbox-runtime.md#2-isolation-backends)).
- Egress allowlist + DNS restriction prevent navigation to disallowed hosts.
- CPU/memory/time limits are higher than other tools but still enforced; sessions
  are ephemeral.
- No credentials are auto-filled unless provided as secret references.

---

# 6. Determinism & Caching

Browsing is non-deterministic (live web) and **not cached**. For reproducible tests,
point at fixtures or recorded pages.

---

# 7. Example

```bash
apex tools enable browser.navigate --project research
apex tools invoke browser.navigate --input '{"url":"https://example.com"}'
```

---

# 8. Related

- [`07-tool-runtime/sandbox-runtime.md`](sandbox-runtime.md)
- [`07-tool-runtime/security-isolation.md`](security-isolation.md)

---

# 9. Revision History

| Version | Date | Description |
|---------|------|-------------|
| 1.0.0 | 2026-06-27 | Initial Browser tool spec |
