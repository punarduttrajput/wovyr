<!--
File: docs/07-tool-runtime/database.md
Document ID: TRT-103
-->

# Built-in Tool: Database

**Document ID:** TRT-103  
**File Path:** `docs/07-tool-runtime/database.md`  
**Version:** 1.0.0  
**Status:** Draft  
**Owner:** AI Platform Team  
**Last Updated:** 2026-06-27

---

# 1. Purpose

The `db.*` built-in tools let agents query external databases through **governed,
scoped connections** — never with ambient credentials. Credentials are resolved
from the [secret vault](../13-security/secret-management.md) and injected at run
time.

---

# 2. Operations

| Tool | Description |
|------|-------------|
| `db.query` | Run a read query, return rows |
| `db.execute` | Run a write/DDL statement (guarded) |
| `db.schema` | Introspect tables/columns |

---

# 3. Schema (example: `db.query`)

```json
// input
{ "connection": "secret://acme/reporting-db", "sql": "SELECT * FROM orders WHERE id = $1", "params": ["123"], "max_rows": 1000 }
// output
{ "columns": ["id","state"], "rows": [["123","shipped"]], "row_count": 1 }
```

`connection` is a **secret reference**, not a raw DSN.

---

# 4. Permissions

```text
secret:read:<connection-ref>     net:egress:<db-host>
db:query | db:execute            (write requires db:execute)
```

Writes (`db.execute`) require an explicit, higher-privilege grant; many deployments
restrict agents to read-only.

---

# 5. Sandbox & Safety

- Parameterized queries are required; raw string interpolation is rejected to
  prevent injection.
- Egress allowed only to the granted DB host
  ([network isolation](security-isolation.md#5-network-isolation)).
- `max_rows`/timeout bound result size and duration.
- Credentials injected in-memory, zeroed on teardown
  ([secrets](security-isolation.md#7-secret-management)).

---

# 6. Determinism & Caching

`db.query` is read-only and may be cached briefly for identical
(connection+sql+params); `db.execute` is side-effecting and never cached.

---

# 7. Example

```bash
apex tools invoke db.query --input '{"connection":"secret://acme/reporting-db","sql":"SELECT count(*) FROM users","params":[]}'
```

---

# 8. Related

- [`13-security/secret-management.md`](../13-security/secret-management.md)
- [`07-tool-runtime/security-isolation.md`](security-isolation.md)

---

# 9. Revision History

| Version | Date | Description |
|---------|------|-------------|
| 1.0.0 | 2026-06-27 | Initial Database tool spec |
