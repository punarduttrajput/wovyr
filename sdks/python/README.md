# wovyr-sdk

A Python client for the [Wovyr AI Platform](../../README.md) HTTP API,
hand-written from the actual `wovyr-server` routes (see
[`docs/09-api/openapi.yaml`](../../docs/09-api/openapi.yaml) for the full
contract) — the same source [`sdks/typescript`](../typescript) was generated
from, so the two clients cover identical ground.

Zero runtime dependencies: the HTTP layer is built on `urllib` from the
standard library rather than `requests`/`httpx`, so there's nothing to
install to use it.

## Install

Published as [`wovyr-sdk`](https://pypi.org/project/wovyr-sdk/) on PyPI:

```bash
pip install wovyr-sdk
```

## Usage

```python
from wovyr_sdk import WovyrClient

client = WovyrClient(
    "http://127.0.0.1:8080",
    tenant="acme",       # optional, defaults to the server's "default" tenant
    principal="alice",   # optional
)

result = client.agents.run({"manifest": my_agent_yaml, "input": {"message": "Hi"}})
print(result["output"]["message"])

# Streaming:
for frame in client.agents.stream({"manifest": my_agent_yaml, "input": {}}):
    if frame["type"] == "delta":
        print(frame["text"], end="")
    if frame["type"] == "result":
        print("\ndone:", frame["output"]["message"])
```

Every resource is namespaced on the client: `client.agents`, `client.workflows`,
`client.memory`, `client.plugins`, `client.marketplace`, `client.secrets`,
`client.organizations`, `client.projects`, `client.webhooks`, `client.audit`,
`client.tools`.

Errors are raised as `WovyrApiError` (`.status`, `.code`, `.request_id`, `.body`)
mapping the server's `{error: {...}}` envelope.

Pagination: every list method returns a `Page` dict (`data`/`has_more`/
`next_cursor`/`total_estimate`); `paginate_all` drains every page:

```python
from wovyr_sdk import paginate_all

for agent_id in paginate_all(client.agents.list, limit=25):
    ...
```

## Development

No package manager is required to run this SDK — it's stdlib-only. To run
the tests:

```bash
cargo run -p wovyr-cli -- dev --addr 127.0.0.1:8080 &
python3 -m unittest discover -s tests -v
```

Tests are integration tests against a real, locally running server (skip
cleanly, not failing, if `WOVYR_TEST_BASE_URL` — default
`http://127.0.0.1:8080` — is unreachable), plus a handful of unit tests for
the retry/backoff logic against a fake opener (no socket).

One test (`test_projects_create_with_stale_if_match_is_rejected`) exercises
organization/project creation, which — unlike agents/workflows/memory — has no
anonymous-default-tenant back-compat bypass; it needs a real `org.admin` role.
Start the server with `WOVYR_PLATFORM_ADMINS=sdk-test-admin` for that test to
run instead of skip.

## Asyncio

For asyncio programs, `wovyr_sdk.aio.AsyncWovyrClient` exposes the same
resource surface with every method awaitable (the sync transport runs in a
worker thread, so the event loop never blocks; streaming yields frames as
they arrive):

```python
from wovyr_sdk.aio import AsyncWovyrClient

client = AsyncWovyrClient("http://127.0.0.1:8080")
result = await client.agents.run({"manifest": my_agent_yaml, "input": {"message": "Hi"}})
final = await client.workflows.wait_for_completion(execution_id)
async for frame in client.agents.stream({"manifest": my_agent_yaml}):
    ...
```

## Retries, polling, versions

- **GET requests** retry transient failures (429/502/503/504, network errors)
  with exponential backoff — configurable via `retry=RetryOptions(...)`.
- **Mutations retry only when keyed**: pass `idempotency_key=` and the same
  retry policy applies — the server's replay middleware makes it safe. A
  keyless mutation never auto-retries (it could double-execute).
- **`client.workflows.wait_for_completion(execution_id)`** polls to a
  terminal status and returns the final snapshot (`WovyrTimeoutError` on
  deadline) — no more hand-rolled poll loops.
- **Versioning**: the SDK version tracks the platform release it targets
  (same major.minor = same API surface; see `CHANGELOG.md`). `health()`
  emits an `WovyrVersionSkewWarning` once per client if the server's
  major.minor differs.

## Known gaps

- No `redocly`-style contract test wired against `openapi.yaml` (the
  TypeScript SDK's `npm test` runs one; this package has no npm-equivalent
  tool available in this environment to wire the same check to).
