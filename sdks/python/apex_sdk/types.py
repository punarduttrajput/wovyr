"""Structural types for the Apex API. These are `TypedDict`s, not validated
dataclasses — every value on the wire is already a plain JSON dict/list, so a
`TypedDict` documents the shape for editors/`mypy` without adding a runtime
(de)serialization layer. Mirrors `sdks/typescript/src/types.ts` 1:1; see
`docs/09-api/openapi.yaml` for the authoritative contract."""

from __future__ import annotations

from typing import Any, List, Literal, Optional, TypedDict


class Page(TypedDict):
    """The cursor-pagination envelope every list endpoint returns."""

    data: List[Any]
    has_more: bool
    next_cursor: Optional[str]
    total_estimate: int


class AuditPage(TypedDict):
    """`GET /api/v1/audit`'s envelope (SEC-301): identical to `Page` except
    `total_estimate` is always `None` — computing an exact count would
    require the full-log scan this route's bounded, time-ranged paging
    exists to avoid."""

    data: List[Any]
    has_more: bool
    next_cursor: Optional[str]
    total_estimate: None


class PageParams(TypedDict, total=False):
    limit: int
    cursor: str


class Usage(TypedDict):
    total_tokens: int
    cost_usd: float


class _RunRequestRequired(TypedDict):
    manifest: str


class RunRequest(_RunRequestRequired, total=False):
    input: Any
    max_steps: int


class RunResult(TypedDict):
    run_id: str
    status: Literal["succeeded"]
    output: dict[str, Any]
    steps: int
    usage: Usage


class AgentSummary(TypedDict):
    id: str
    manifest: str


class WorkflowActivitySummary(TypedDict, total=False):
    id: str
    type: str
    name: str


class WorkflowEdgeSummary(TypedDict, total=False):
    from_: str
    to: str
    when: Optional[str]


class WorkflowValidation(TypedDict):
    valid: bool
    name: str
    version: str
    activities: List[WorkflowActivitySummary]
    edges: List[dict[str, Any]]
    activity_count: int


WorkflowStatus = Literal[
    "created",
    "validated",
    "scheduled",
    "running",
    "waiting",
    "resumed",
    "compensating",
    "completed",
    "failed",
    "cancelled",
]


class WorkflowListParams(PageParams, total=False):
    workflow: str
    status: WorkflowStatus


class _SubmitWorkflowRequestRequired(TypedDict):
    manifest: str


class SubmitWorkflowRequest(_SubmitWorkflowRequestRequired, total=False):
    input: Any
    execution_id: str


MemoryType = Literal["semantic", "conversation", "workflow", "episodic"]


class _PutMemoryRequestRequired(TypedDict):
    namespace: str
    content: str


class PutMemoryRequest(_PutMemoryRequestRequired, total=False):
    type: MemoryType
    importance: float
    tags: List[str]
    required_scopes: List[str]


class _QueryMemoryRequestRequired(TypedDict):
    text: str


class QueryMemoryRequest(_QueryMemoryRequestRequired, total=False):
    namespace: str
    strategy: Literal["vector", "keyword", "hybrid"]
    limit: int
    diversity: float
    min_importance: float
    tags: List[str]
    grants: List[str]
    relevance: float
    recency: float
    importance: float


class MemoryScoreBreakdown(TypedDict):
    relevance: float
    recency: float
    importance: float
    total: float


class MemoryQueryResult(TypedDict):
    id: str
    namespace: str
    content: str
    type: str
    importance: float
    tags: List[str]
    score: float
    breakdown: MemoryScoreBreakdown


CapabilityKind = Literal["tool", "provider", "memory_backend", "policy", "workflow_activity"]


class CapabilityDescriptor(TypedDict):
    kind: CapabilityKind
    id: str


class PluginSummary(TypedDict):
    id: str
    name: str
    version: str
    publisher: str
    description: str
    state: Literal["enabled", "disabled"]
    permissions: List[str]
    granted: List[str]
    capabilities: List[CapabilityDescriptor]
    platform_api: str


class ScanFinding(TypedDict):
    code: str
    severity: Literal["info", "warning", "critical"]
    message: str


class ScanReport(TypedDict):
    findings: List[ScanFinding]


class MarketplaceSearchParams(TypedDict, total=False):
    q: str
    category: str
    capability: CapabilityKind


class PublishResult(TypedDict):
    listing: str
    reference: str
    channel: str
    status: Literal["published"]
    scan: ScanReport


class Attestation(TypedDict):
    id: str
    version: str
    publisher: str
    verified: bool
    signature_verified: bool
    risk: Literal["low", "medium", "high"]
    permissions: List[str]
    package_digest: str
    sbom: Optional[List[Any]]
    provenance: Optional[Any]
    scan: ScanReport


class SecretMetadata(TypedDict):
    reference: str
    name: str
    version: int


Role = Literal["viewer", "editor", "project_admin", "org_admin", "platform_admin"]


class AuditActor(TypedDict, total=False):
    principal: str
    type: str
    tenant: str


class AuditResource_(TypedDict, total=False):
    type: str
    id: str


class AuditEntry(TypedDict, total=False):
    actor: AuditActor
    action: str
    resource: AuditResource_
    outcome: str
    reason: str
    request_id: str
    timestamp_ms: int
    prev_hash: str
    hash: str


class ToolSummary(TypedDict):
    id: str
    description: str
    category: str
    permissions: Optional[List[str]]
