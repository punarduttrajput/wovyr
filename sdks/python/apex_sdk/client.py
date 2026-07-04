"""A client for one Apex `apex-server` instance, scoped to one tenant/
principal (construct a new client per tenant to act as a different one).
Mirrors the server's actual routes (see `docs/09-api/openapi.yaml`) — not the
aspirational conventions in `overview.md` (no opaque `agt_...` ids, no
OAuth2; resources are addressed by their natural key and auth is the
`X-Apex-Tenant`/`X-Apex-Principal` headers).

Synchronous by design: built on `urllib` (see `http.py`), so every method
blocks. An `asyncio` variant is a documented gap (see the package README),
not an oversight."""

from __future__ import annotations

import base64
import json
from typing import Any, Iterator, List, Optional
from urllib.parse import quote

from .http import HttpClient, Opener, RetryOptions
from .sse import parse_sse
from .types import (
    Attestation,
    MarketplaceSearchParams,
    Page,
    PublishResult,
    PutMemoryRequest,
    QueryMemoryRequest,
    Role,
    RunRequest,
    RunResult,
    SecretMetadata,
    SubmitWorkflowRequest,
    WorkflowValidation,
)


def _b64encode(data: bytes) -> str:
    return base64.b64encode(data).decode("ascii")


def _b64decode(data: str) -> bytes:
    return base64.b64decode(data)


def _quote(value: str) -> str:
    return quote(value, safe="")


class ApexClient:
    def __init__(
        self,
        base_url: str,
        *,
        tenant: Optional[str] = None,
        principal: Optional[str] = None,
        retry: Optional[RetryOptions] = None,
        opener: Optional[Opener] = None,
    ) -> None:
        self._http = HttpClient(base_url, tenant=tenant, principal=principal, retry=retry, opener=opener)
        self.agents = AgentsResource(self._http)
        self.workflows = WorkflowsResource(self._http)
        self.memory = MemoryResource(self._http)
        self.plugins = PluginsResource(self._http)
        self.marketplace = MarketplaceResource(self._http)
        self.secrets = SecretsResource(self._http)
        self.organizations = OrganizationsResource(self._http)
        self.projects = ProjectsResource(self._http)
        self.webhooks = WebhooksResource(self._http)
        self.audit = AuditResource(self._http)
        self.tools = ToolsResource(self._http)

    def health(self) -> dict[str, Any]:
        """`GET /healthz`."""
        return self._http.request("GET", "/healthz")


class AgentsResource:
    def __init__(self, http: HttpClient) -> None:
        self._http = http

    def run(
        self,
        req: RunRequest,
        *,
        idempotency_key: Optional[str] = None,
        project: Optional[str] = None,
    ) -> RunResult:
        """`POST /api/v1/agents:run` — run an inline manifest, no persistence."""
        headers: dict[str, str] = {}
        if idempotency_key:
            headers["Idempotency-Key"] = idempotency_key
        if project:
            headers["X-Apex-Project"] = project
        return self._http.request("POST", "/api/v1/agents:run", req, headers=headers)

    def stream(self, req: RunRequest, *, project: Optional[str] = None) -> Iterator[dict[str, Any]]:
        """`POST /api/v1/agents:stream` — run an inline manifest, yielding
        progress frames as they arrive. The final yielded event is always
        `result` or `error`."""
        headers: dict[str, str] = {}
        if project:
            headers["X-Apex-Project"] = project
        response = self._http.raw("POST", "/api/v1/agents:stream", req, headers=headers)
        if not (200 <= response.status < 300):
            text = response.read().decode("utf-8")
            raise RuntimeError(f"agents:stream failed with status {response.status}: {text}")
        for frame in parse_sse(response.iter_lines()):
            if frame.event == "result":
                yield {"type": "result", **json.loads(frame.data)}
            elif frame.event == "error":
                yield {"type": "error", "message": frame.data}
            else:
                yield json.loads(frame.data)

    def list(self, *, limit: Optional[int] = None, cursor: Optional[str] = None) -> Page:
        """`GET /api/v1/agents` — list stored agent ids for the caller's tenant."""
        return self._http.request("GET", "/api/v1/agents", query={"limit": limit, "cursor": cursor})

    def create(self, manifest: str) -> dict[str, Any]:
        """`POST /api/v1/agents` — store an agent manifest."""
        return self._http.request("POST", "/api/v1/agents", {"manifest": manifest})

    def get(self, agent_id: str) -> dict[str, Any]:
        """`GET /api/v1/agents/{id}`."""
        return self._http.request("GET", f"/api/v1/agents/{_quote(agent_id)}")

    def delete(self, agent_id: str) -> None:
        """`DELETE /api/v1/agents/{id}`."""
        return self._http.request("DELETE", f"/api/v1/agents/{_quote(agent_id)}")

    def run_stored(
        self,
        agent_id: str,
        *,
        input: Any = None,
        max_steps: Optional[int] = None,
        project: Optional[str] = None,
    ) -> RunResult:
        """`POST /api/v1/agents/{id}/run` — run a stored agent by id."""
        headers: dict[str, str] = {}
        if project:
            headers["X-Apex-Project"] = project
        body: dict[str, Any] = {}
        if input is not None:
            body["input"] = input
        if max_steps is not None:
            body["max_steps"] = max_steps
        return self._http.request("POST", f"/api/v1/agents/{_quote(agent_id)}/run", body, headers=headers)


class WorkflowsResource:
    def __init__(self, http: HttpClient) -> None:
        self._http = http

    def validate(self, manifest: str) -> WorkflowValidation:
        """`POST /api/v1/workflows/validate` — parse-only, no side effects."""
        return self._http.request("POST", "/api/v1/workflows/validate", {"manifest": manifest})

    def list(self, **params: Any) -> Page:
        """`GET /api/v1/workflows` — list executions visible to the caller's
        tenant. Accepts `workflow`, `status`, `limit`, `cursor`."""
        return self._http.request("GET", "/api/v1/workflows", query=params)

    def submit(self, req: SubmitWorkflowRequest) -> dict[str, Any]:
        """`POST /api/v1/workflows` — durably start + async-drive an
        execution. Returns immediately; poll `get` for completion."""
        return self._http.request("POST", "/api/v1/workflows", req)

    def get(self, execution_id: str) -> dict[str, Any]:
        """`GET /api/v1/workflows/{id}` — live status + event history."""
        return self._http.request("GET", f"/api/v1/workflows/{_quote(execution_id)}")

    def cancel(self, execution_id: str) -> None:
        """`DELETE /api/v1/workflows/{id}` — advisory cancel."""
        return self._http.request("DELETE", f"/api/v1/workflows/{_quote(execution_id)}")

    def signal(self, execution_id: str, *, manifest: str, event: str, payload: Any = None) -> dict[str, Any]:
        """`POST /api/v1/workflows/{id}/signal`."""
        body: dict[str, Any] = {"manifest": manifest, "event": event}
        if payload is not None:
            body["payload"] = payload
        return self._http.request("POST", f"/api/v1/workflows/{_quote(execution_id)}/signal", body)

    def approve(
        self, execution_id: str, *, manifest: str, activity_id: str, decision: Any = None
    ) -> dict[str, Any]:
        """`POST /api/v1/workflows/{id}/approve`."""
        body: dict[str, Any] = {"manifest": manifest, "activity_id": activity_id}
        if decision is not None:
            body["decision"] = decision
        return self._http.request("POST", f"/api/v1/workflows/{_quote(execution_id)}/approve", body)


class MemoryResource:
    def __init__(self, http: HttpClient) -> None:
        self._http = http

    def namespaces(self) -> dict[str, Any]:
        """`GET /api/v1/memory/namespaces`."""
        return self._http.request("GET", "/api/v1/memory/namespaces")

    def list_records(
        self, *, namespace: Optional[str] = None, limit: Optional[int] = None, cursor: Optional[str] = None
    ) -> Page:
        """`GET /api/v1/memory/records`."""
        return self._http.request(
            "GET", "/api/v1/memory/records", query={"namespace": namespace, "limit": limit, "cursor": cursor}
        )

    def put(self, req: PutMemoryRequest) -> dict[str, Any]:
        """`POST /api/v1/memory/records` — store a memory record."""
        return self._http.request("POST", "/api/v1/memory/records", req)

    def query(self, req: QueryMemoryRequest) -> dict[str, Any]:
        """`POST /api/v1/memory:query` — hybrid vector+keyword retrieval.
        Returns `{"results": [...], "count": int}`."""
        return self._http.request("POST", "/api/v1/memory:query", req)


class PluginsResource:
    def __init__(self, http: HttpClient) -> None:
        self._http = http

    def list(self) -> dict[str, Any]:
        """`GET /api/v1/plugins`."""
        return self._http.request("GET", "/api/v1/plugins")

    def install(self, apexpkg: bytes, grants: Optional[List[str]] = None) -> Any:
        """`POST /api/v1/plugins:install` — `apexpkg` is the raw `.apexpkg` bytes."""
        return self._http.request(
            "POST", "/api/v1/plugins:install", {"apexpkg": _b64encode(apexpkg), "grants": grants or []}
        )

    def enable(self, plugin_id: str) -> Any:
        """`POST /api/v1/plugins:enable`."""
        return self._http.request("POST", "/api/v1/plugins:enable", {"id": plugin_id})

    def disable(self, plugin_id: str) -> Any:
        """`POST /api/v1/plugins:disable`."""
        return self._http.request("POST", "/api/v1/plugins:disable", {"id": plugin_id})

    def upgrade(self, apexpkg: bytes, grants: Optional[List[str]] = None) -> Any:
        """`POST /api/v1/plugins:upgrade`."""
        return self._http.request(
            "POST", "/api/v1/plugins:upgrade", {"apexpkg": _b64encode(apexpkg), "grants": grants or []}
        )

    def rollback(self, plugin_id: str) -> Any:
        """`POST /api/v1/plugins:rollback`."""
        return self._http.request("POST", "/api/v1/plugins:rollback", {"id": plugin_id})

    def trust(self, publisher: str, public_key_hex: str) -> Any:
        """`POST /api/v1/plugins:trust`."""
        return self._http.request(
            "POST", "/api/v1/plugins:trust", {"publisher": publisher, "public_key_hex": public_key_hex}
        )

    def uninstall(self, plugin_id: str) -> Any:
        """`DELETE /api/v1/plugins/{id}` — `id` is `publisher/name`."""
        return self._http.request("DELETE", f"/api/v1/plugins/{_quote(plugin_id)}")


class MarketplaceResource:
    def __init__(self, http: HttpClient) -> None:
        self._http = http

    def search(self, params: Optional[MarketplaceSearchParams] = None) -> dict[str, Any]:
        """`GET /api/v1/marketplace/listings`."""
        return self._http.request("GET", "/api/v1/marketplace/listings", query=params or {})

    def publish(
        self, apexpkg: bytes, *, categories: Optional[List[str]] = None, channel: Optional[str] = None
    ) -> PublishResult:
        """`POST /api/v1/marketplace:publish`."""
        body: dict[str, Any] = {"apexpkg": _b64encode(apexpkg), "categories": categories or []}
        if channel is not None:
            body["channel"] = channel
        return self._http.request("POST", "/api/v1/marketplace:publish", body)

    def get(self, listing_id: str) -> Any:
        """`GET /api/v1/marketplace/listings/{id}` — `id` is `publisher/name`."""
        return self._http.request("GET", f"/api/v1/marketplace/listings/{_quote(listing_id)}")

    def download(self, listing_id: str, *, version: Optional[str] = None) -> bytes:
        """`GET /api/v1/marketplace/listings/{id}/download` — returns the raw
        `.apexpkg` bytes (already base64-decoded)."""
        res = self._http.request(
            "GET",
            f"/api/v1/marketplace/listings/{_quote(listing_id)}/download",
            query={"version": version},
        )
        return _b64decode(res["apexpkg"])

    def attestation(self, listing_id: str, *, version: Optional[str] = None) -> Attestation:
        """`GET /api/v1/marketplace/listings/{id}/attestation`."""
        return self._http.request(
            "GET",
            f"/api/v1/marketplace/listings/{_quote(listing_id)}/attestation",
            query={"version": version},
        )

    def review(self, listing_id: str, *, author: str, rating: int, body: Optional[str] = None) -> Any:
        """`POST /api/v1/marketplace/listings/{id}/reviews`."""
        payload: dict[str, Any] = {"author": author, "rating": rating}
        if body is not None:
            payload["body"] = body
        return self._http.request(
            "POST", f"/api/v1/marketplace/listings/{_quote(listing_id)}/reviews", payload
        )

    def set_verified(self, listing_id: str, verified: bool = True) -> dict[str, Any]:
        """`POST /api/v1/marketplace/listings/{id}/verify` — operator
        override, bypasses the request/approve/reject workflow below."""
        return self._http.request(
            "POST", f"/api/v1/marketplace/listings/{_quote(listing_id)}/verify", {"verified": verified}
        )

    def request_review(self, listing_id: str) -> dict[str, Any]:
        """`POST /api/v1/marketplace/listings/{id}/request-review`."""
        return self._http.request(
            "POST", f"/api/v1/marketplace/listings/{_quote(listing_id)}/request-review"
        )

    def approve_review(self, listing_id: str, *, reviewer: Optional[str] = None) -> dict[str, Any]:
        """`POST /api/v1/marketplace/listings/{id}/approve`."""
        return self._http.request(
            "POST", f"/api/v1/marketplace/listings/{_quote(listing_id)}/approve", {"reviewer": reviewer}
        )

    def reject_review(self, listing_id: str, *, reason: str, reviewer: Optional[str] = None) -> dict[str, Any]:
        """`POST /api/v1/marketplace/listings/{id}/reject`."""
        return self._http.request(
            "POST",
            f"/api/v1/marketplace/listings/{_quote(listing_id)}/reject",
            {"reason": reason, "reviewer": reviewer},
        )

    def install(
        self, listing_id: str, *, version: Optional[str] = None, grants: Optional[List[str]] = None
    ) -> Any:
        """`POST /api/v1/marketplace/listings/{id}/install`."""
        return self._http.request(
            "POST",
            f"/api/v1/marketplace/listings/{_quote(listing_id)}/install",
            {"version": version, "grants": grants or []},
        )


class SecretsResource:
    def __init__(self, http: HttpClient) -> None:
        self._http = http

    def list(self) -> dict[str, Any]:
        """`GET /api/v1/secrets`."""
        return self._http.request("GET", "/api/v1/secrets")

    def create(self, name: str, value: str) -> SecretMetadata:
        """`POST /api/v1/secrets` — value is never returned."""
        return self._http.request("POST", "/api/v1/secrets", {"name": name, "value": value})

    def get(self, name: str) -> SecretMetadata:
        """`GET /api/v1/secrets/{name}`."""
        return self._http.request("GET", f"/api/v1/secrets/{_quote(name)}")

    def delete(self, name: str) -> None:
        """`DELETE /api/v1/secrets/{name}`."""
        return self._http.request("DELETE", f"/api/v1/secrets/{_quote(name)}")

    def rotate(self, name: str, value: str) -> SecretMetadata:
        """`POST /api/v1/secrets/{name}/rotate`."""
        return self._http.request("POST", f"/api/v1/secrets/{_quote(name)}/rotate", {"value": value})


class OrganizationsResource:
    def __init__(self, http: HttpClient) -> None:
        self._http = http

    def list(self, *, limit: Optional[int] = None, cursor: Optional[str] = None) -> Page:
        """`GET /api/v1/organizations`."""
        return self._http.request("GET", "/api/v1/organizations", query={"limit": limit, "cursor": cursor})

    def create(self, name: str) -> dict[str, Any]:
        """`POST /api/v1/organizations`."""
        return self._http.request("POST", "/api/v1/organizations", {"name": name})


class ProjectsResource:
    def __init__(self, http: HttpClient) -> None:
        self._http = http

    def list(self, *, limit: Optional[int] = None, cursor: Optional[str] = None) -> Page:
        """`GET /api/v1/projects`."""
        return self._http.request("GET", "/api/v1/projects", query={"limit": limit, "cursor": cursor})

    def create(self, name: str, organization: str) -> tuple[dict[str, Any], Optional[str]]:
        """`POST /api/v1/projects`. Returns `(project, etag)` — `etag` is the
        current `version`, for a subsequent `update`'s `if_match`."""
        data, headers = self._http.request_with_headers(
            "POST", "/api/v1/projects", {"name": name, "organization": organization}
        )
        return data, headers.get("etag")

    def get(self, project_id: str) -> tuple[dict[str, Any], Optional[str]]:
        """`GET /api/v1/projects/{id}`. Returns `(project, etag)`."""
        data, headers = self._http.request_with_headers("GET", f"/api/v1/projects/{_quote(project_id)}")
        return data, headers.get("etag")

    def update(
        self,
        project_id: str,
        *,
        settings: Optional[dict[str, Any]] = None,
        status: Optional[str] = None,
        if_match: Optional[str] = None,
    ) -> tuple[dict[str, Any], Optional[str]]:
        """`PATCH /api/v1/projects/{id}`. `if_match` (from a prior `get`/
        `create`'s `etag`) guards against a lost concurrent update — a stale
        value raises `ApexApiError` with `status == 409`."""
        patch: dict[str, Any] = {}
        if settings is not None:
            patch["settings"] = settings
        if status is not None:
            patch["status"] = status
        headers: dict[str, str] = {}
        if if_match:
            headers["If-Match"] = if_match
        data, response_headers = self._http.request_with_headers(
            "PATCH", f"/api/v1/projects/{_quote(project_id)}", patch, headers=headers
        )
        return data, response_headers.get("etag")

    def delete(self, project_id: str) -> None:
        """`DELETE /api/v1/projects/{id}`."""
        return self._http.request("DELETE", f"/api/v1/projects/{_quote(project_id)}")

    def list_members(self, project_id: str) -> dict[str, Any]:
        """`GET /api/v1/projects/{id}/members`."""
        return self._http.request("GET", f"/api/v1/projects/{_quote(project_id)}/members")

    def add_member(self, project_id: str, user: str, role: Role) -> Any:
        """`POST /api/v1/projects/{id}/members`."""
        return self._http.request(
            "POST", f"/api/v1/projects/{_quote(project_id)}/members", {"user": user, "role": role}
        )

    def remove_member(self, project_id: str, user_id: str) -> None:
        """`DELETE /api/v1/projects/{id}/members/{uid}`."""
        return self._http.request(
            "DELETE", f"/api/v1/projects/{_quote(project_id)}/members/{_quote(user_id)}"
        )

    def get_quota(self, project_id: str) -> dict[str, Any]:
        """`GET /api/v1/projects/{id}/quota`."""
        return self._http.request("GET", f"/api/v1/projects/{_quote(project_id)}/quota")

    def update_quota(
        self,
        project_id: str,
        *,
        concurrent_agent_runs: Optional[int] = None,
        llm_cost_per_day_usd: Optional[float] = None,
    ) -> dict[str, Any]:
        """`PATCH /api/v1/projects/{id}/quota` — org.admin only."""
        limits: dict[str, Any] = {}
        if concurrent_agent_runs is not None:
            limits["concurrent_agent_runs"] = concurrent_agent_runs
        if llm_cost_per_day_usd is not None:
            limits["llm_cost_per_day_usd"] = llm_cost_per_day_usd
        return self._http.request("PATCH", f"/api/v1/projects/{_quote(project_id)}/quota", limits)


class WebhooksResource:
    def __init__(self, http: HttpClient) -> None:
        self._http = http

    def list(self, *, limit: Optional[int] = None, cursor: Optional[str] = None) -> Page:
        """`GET /api/v1/webhooks` — secrets redacted."""
        return self._http.request("GET", "/api/v1/webhooks", query={"limit": limit, "cursor": cursor})

    def register(self, *, url: str, events: List[str], secret: str) -> Any:
        """`POST /api/v1/webhooks`."""
        return self._http.request("POST", "/api/v1/webhooks", {"url": url, "events": events, "secret": secret})

    def remove(self, webhook_id: str) -> None:
        """`DELETE /api/v1/webhooks/{id}`."""
        return self._http.request("DELETE", f"/api/v1/webhooks/{_quote(webhook_id)}")


class AuditResource:
    def __init__(self, http: HttpClient) -> None:
        self._http = http

    def query(
        self, *, principal: Optional[str] = None, action: Optional[str] = None, limit: Optional[int] = None
    ) -> dict[str, Any]:
        """`GET /api/v1/audit` — tenant-scoped, tamper-evident hash-chained log."""
        return self._http.request(
            "GET", "/api/v1/audit", query={"principal": principal, "action": action, "limit": limit}
        )


class ToolsResource:
    def __init__(self, http: HttpClient) -> None:
        self._http = http

    def list(self) -> dict[str, Any]:
        """`GET /api/v1/tools` — built-ins + enabled plugin tools. Unauthenticated."""
        return self._http.request("GET", "/api/v1/tools")
