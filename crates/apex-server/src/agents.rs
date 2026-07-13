//! Agent-run and workflow-visibility HTTP handlers, plus the shared
//! [`ApiError`] envelope (RM-GA-P4 HLTH-904 — split out of `lib.rs`).

use crate::state::{AppState, AsyncRunStatus};
use crate::{tenancy, webhooks};
use apex_agent::{AgentDefinition, NullSink, RunEvent, RunEventSink, RunOptions, run_agent};
use apex_common::Error;
use apex_workflow::{ExecutionFilter, WorkflowState};
use axum::{
    Json,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{
        Html, IntoResponse, Response,
        sse::{Event, Sse},
    },
};
use futures::StreamExt;
use serde::Deserialize;
use serde_json::{Value, json};
use std::convert::Infallible;
use std::sync::Arc;
use std::sync::atomic::Ordering;

/// Liveness probe.
pub(crate) async fn healthz() -> Json<Value> {
    Json(json!({ "status": "ok", "version": env!("CARGO_PKG_VERSION") }))
}

/// Body for `POST /api/v1/agents:run`.
#[derive(Debug, Deserialize)]
pub(crate) struct RunRequest {
    /// The agent manifest (YAML), supplied inline in v0.1.
    manifest: String,
    /// Run input (e.g. `{"message": "..."}`).
    #[serde(default)]
    input: Value,
    /// Override the model/tool iteration cap (default: [`apex_agent::RunOptions`]'s).
    #[serde(default)]
    max_steps: Option<usize>,
}

/// Whether the caller asked for the async submit→poll shape (RM-GA-P2 EXE-604) via
/// a standard `Prefer: respond-async` request header (RFC 7240) rather than a
/// separate `:submit` route — a comma-separated list of preferences is allowed, so
/// this checks each token rather than requiring an exact match.
fn wants_async(headers: &HeaderMap) -> bool {
    headers
        .get("prefer")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| {
            v.split(',')
                .any(|p| p.trim().eq_ignore_ascii_case("respond-async"))
        })
}

/// Run an agent, recording RED golden-signal metrics for the route. Instrumented so
/// the request runs under a trace whose id becomes the latency exemplar. Synchronous
/// by default (unchanged from before EXE-604); `Prefer: respond-async` switches to
/// the async submit→poll shape (mirroring the workflow submit route): the run is
/// admitted and started on a background task holding the quota permit, and the
/// response comes back immediately with a `run_id` to poll at `GET
/// /api/v1/agents/runs/{run_id}` instead of waiting for the model/tool loop to
/// finish. `Idempotency-Key` replay (overview §9) is handled uniformly for every
/// mutating route by `hardening::idempotency_middleware` (RM-GA-P4 API-703), which
/// wraps this route — so a repeated key replays the cached response whichever branch
/// produced it, sync or async (an async retry gets back the same `run_id` rather than
/// starting a second run).
#[tracing::instrument(name = "api.agents_run", skip_all)]
pub(crate) async fn run_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(req): Json<RunRequest>,
) -> Result<Json<Value>, ApiError> {
    let tenant = tenancy::run_tenant(&headers);
    let project = tenancy::run_project(&headers);

    if wants_async(&headers) {
        return run_async_inner(&state, tenant, project, req, headers).await;
    }

    run_inner(&state, tenant.clone(), project, req, &headers).await
}

/// Parse the inline manifest then run it ([Agents API §5](../../docs/09-api/agents.md)).
async fn run_inner(
    state: &Arc<AppState>,
    tenant: String,
    project: Option<String>,
    req: RunRequest,
    headers: &HeaderMap,
) -> Result<Json<Value>, ApiError> {
    let def = AgentDefinition::from_yaml(&req.manifest)
        .map_err(|e| ApiError::new(StatusCode::BAD_REQUEST, "validation_failed", e.to_string()))?;
    run_definition(
        state,
        def,
        req.input,
        &tenant,
        project.as_deref(),
        req.max_steps,
        headers,
    )
    .await
}

/// Parse + admit the run, then drive it on a background task that holds the quota
/// permit for its own duration (not the HTTP connection) — the async counterpart of
/// [`run_definition`]. Returns immediately once the run is admitted and the task is
/// spawned; the task records the terminal outcome into `state.runs` for
/// [`get_run_handler`] to serve.
async fn run_async_inner(
    state: &Arc<AppState>,
    tenant: String,
    project: Option<String>,
    req: RunRequest,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    let def = AgentDefinition::from_yaml(&req.manifest)
        .map_err(|e| ApiError::new(StatusCode::BAD_REQUEST, "validation_failed", e.to_string()))?;
    let input = if req.input.is_null() {
        json!({})
    } else {
        req.input
    };

    // Quota gate up front, same as the synchronous path: a rejected run never gets a
    // run id at all, rather than reporting `failed` later for a run that never
    // started.
    let permit = tenancy::admit_run(&state.tenancy, &state.quota, project.as_deref())?;

    let run_id = format!("run_{}", state.run_counter.fetch_add(1, Ordering::SeqCst));
    state.runs.insert_running(run_id.clone(), tenant.clone());

    let mut opts = RunOptions::new(input)
        .with_tenant(tenant.clone())
        .with_hosted(true);
    if let Some(n) = req.max_steps.or(def.spec.max_steps) {
        opts = opts.with_max_steps(n);
    }

    let state2 = state.clone();
    let run_id2 = run_id.clone();
    tokio::spawn(async move {
        let _permit = permit; // held for the run's duration, not the HTTP connection
        match run_agent(&def, &state2.gateway, &state2.registry, opts, &mut NullSink).await {
            Ok(out) => {
                tenancy::record_run_cost(&state2.quota, project.as_deref(), out.usage.cost_usd);
                crate::audit::audit(&state2, &headers, &tenant, "agent.run", "agent", &run_id2);
                webhooks::emit(
                    &state2,
                    "agent.run.completed",
                    &tenant,
                    json!({ "run_id": run_id2, "total_tokens": out.usage.total_tokens }),
                );
                state2.runs.finish(
                    &run_id2,
                    AsyncRunStatus::Succeeded {
                        output: json!({ "message": out.text }),
                        steps: out.steps,
                        usage: json!({
                            "total_tokens": out.usage.total_tokens,
                            "cost_usd": out.usage.cost_usd,
                        }),
                    },
                );
            }
            Err(e) => {
                webhooks::emit(
                    &state2,
                    "agent.run.failed",
                    &tenant,
                    json!({ "error": e.to_string() }),
                );
                state2.runs.finish(
                    &run_id2,
                    AsyncRunStatus::Failed {
                        error: e.to_string(),
                    },
                );
            }
        }
    });

    Ok(Json(json!({ "run_id": run_id, "status": "running" })))
}

/// `GET /api/v1/agents/runs/{run_id}` — poll a run submitted via `agents:run` with
/// `Prefer: respond-async` (RM-GA-P2 EXE-604). Tenant-scoped like every other
/// resource: a run belonging to another tenant is hidden behind the same `404` an
/// unknown run gets, rather than a `403` that would confirm it exists.
pub(crate) async fn get_run_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(run_id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let tenant = tenancy::run_tenant(&headers);
    let missing = || {
        ApiError::new(
            StatusCode::NOT_FOUND,
            "not_found",
            format!("run `{run_id}` not found"),
        )
    };
    let (owner, status) = state.runs.get(&run_id).ok_or_else(missing)?;
    if owner != tenant {
        return Err(missing());
    }
    let mut body = json!({ "run_id": run_id });
    match serde_json::to_value(&status).map_err(|e| {
        ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal_error",
            e.to_string(),
        )
    })? {
        Value::Object(fields) => {
            body.as_object_mut().unwrap().extend(fields);
        }
        _ => unreachable!("AsyncRunStatus always serializes to an object"),
    }
    Ok(Json(body))
}

/// A [`RunEventSink`] that forwards each run event to an SSE channel as a JSON frame.
struct ChannelSink {
    tx: futures::channel::mpsc::UnboundedSender<Event>,
}

impl RunEventSink for ChannelSink {
    fn emit(&mut self, event: RunEvent<'_>) {
        let data = match event {
            RunEvent::Start { model, provider } => {
                json!({ "type": "start", "model": model, "provider": provider })
            }
            RunEvent::MemoryRetrieved { source, score } => {
                json!({ "type": "memory", "source": source, "score": score })
            }
            RunEvent::Delta { text } => json!({ "type": "delta", "text": text }),
            RunEvent::ToolCallDelta {
                index,
                name,
                arguments,
            } => {
                json!({ "type": "tool_call_delta", "index": index, "name": name, "arguments": arguments })
            }
            RunEvent::ReasoningDelta { text } => json!({ "type": "reasoning", "text": text }),
            RunEvent::ToolCall { name, arguments } => {
                json!({ "type": "tool_call", "name": name, "arguments": arguments })
            }
            RunEvent::ToolResult { name, ok } => {
                json!({ "type": "tool_result", "name": name, "ok": ok })
            }
            RunEvent::Done { usage } => json!({
                "type": "done",
                "usage": { "total_tokens": usage.total_tokens, "cost_usd": usage.cost_usd }
            }),
        };
        let _ = self
            .tx
            .unbounded_send(Event::default().data(data.to_string()));
    }
}

/// `POST /api/v1/agents:stream` — run an inline-manifest agent, streaming its events
/// (start / delta / tool_call_delta / reasoning / tool_call / tool_result / done,
/// then a final `result`) as SSE.
pub(crate) async fn run_stream_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(req): Json<RunRequest>,
) -> Response {
    let def = match AgentDefinition::from_yaml(&req.manifest) {
        Ok(d) => d,
        Err(e) => {
            return ApiError::new(StatusCode::BAD_REQUEST, "validation_failed", e.to_string())
                .into_response();
        }
    };
    let input = if req.input.is_null() {
        json!({})
    } else {
        req.input
    };

    // Quota gate before streaming begins (429 if exceeded); the permit rides along in
    // the run task and releases when it ends.
    let project = tenancy::run_project(&headers);
    let tenant = tenancy::run_tenant(&headers);
    let permit = match tenancy::admit_run(&state.tenancy, &state.quota, project.as_deref()) {
        Ok(p) => p,
        Err(e) => return e.into_response(),
    };

    let mut opts = RunOptions::new(input).with_tenant(tenant).with_hosted(true);
    // An explicit per-run override wins; otherwise fall back to the agent's own default.
    if let Some(n) = req.max_steps.or(def.spec.max_steps) {
        opts = opts.with_max_steps(n);
    }

    let (tx, rx) = futures::channel::mpsc::unbounded::<Event>();
    tokio::spawn(async move {
        let _permit = permit;
        let mut sink = ChannelSink { tx: tx.clone() };
        let frame = match run_agent(&def, &state.gateway, &state.registry, opts, &mut sink).await {
            Ok(out) => {
                tenancy::record_run_cost(&state.quota, project.as_deref(), out.usage.cost_usd);
                Event::default().event("result").data(
                    json!({ "status": "succeeded", "output": { "message": out.text }, "steps": out.steps })
                        .to_string(),
                )
            }
            Err(e) => Event::default().event("error").data(e.to_string()),
        };
        let _ = tx.unbounded_send(frame);
        // `tx` (and the sink's clone) drop here, closing the SSE stream.
    });

    Sse::new(rx.map(Ok::<Event, Infallible>)).into_response()
}

/// Run a (parsed) agent definition with `input`, returning the run-response shape.
/// Shared by the inline-manifest and stored-agent run endpoints. Enforces the in-scope
/// `project`'s quota (concurrent runs + daily LLM spend) when one is set.
async fn run_definition(
    state: &Arc<AppState>,
    def: AgentDefinition,
    input: Value,
    tenant: &str,
    project: Option<&str>,
    max_steps: Option<usize>,
    headers: &HeaderMap,
) -> Result<Json<Value>, ApiError> {
    let input = if input.is_null() { json!({}) } else { input };

    // Quota gate: hold a concurrency slot for the duration of the run (released on
    // drop), then record the run's cost against the project's daily budget.
    let _permit = tenancy::admit_run(&state.tenancy, &state.quota, project)?;
    let mut opts = RunOptions::new(input).with_tenant(tenant).with_hosted(true);
    // An explicit per-run override wins; otherwise fall back to the agent's own default.
    if let Some(n) = max_steps.or(def.spec.max_steps) {
        opts = opts.with_max_steps(n);
    }
    let out = match run_agent(&def, &state.gateway, &state.registry, opts, &mut NullSink).await {
        Ok(out) => out,
        Err(e) => {
            webhooks::emit(
                state,
                "agent.run.failed",
                tenant,
                json!({ "error": e.to_string() }),
            );
            return Err(ApiError::from(e));
        }
    };
    tenancy::record_run_cost(&state.quota, project, out.usage.cost_usd);

    let run_id = format!("run_{}", state.run_counter.fetch_add(1, Ordering::SeqCst));
    crate::audit::audit(state, headers, tenant, "agent.run", "agent", &run_id);
    webhooks::emit(
        state,
        "agent.run.completed",
        tenant,
        json!({ "run_id": run_id, "total_tokens": out.usage.total_tokens }),
    );
    Ok(Json(json!({
        "run_id": run_id,
        "status": "succeeded",
        "output": { "message": out.text },
        "steps": out.steps,
        "usage": {
            "total_tokens": out.usage.total_tokens,
            "cost_usd": out.usage.cost_usd,
        }
    })))
}

/// Body for `POST /api/v1/agents` — register an agent from its YAML manifest.
#[derive(Debug, Deserialize)]
pub(crate) struct CreateAgentRequest {
    /// The agent manifest (YAML).
    manifest: String,
}

/// Body for `POST /api/v1/agents/{id}/run` — run a stored agent.
#[derive(Debug, Default, Deserialize)]
pub(crate) struct RunStoredRequest {
    /// Run input (e.g. `{"message": "..."}`).
    #[serde(default)]
    input: Value,
    /// Override the model/tool iteration cap (default: [`apex_agent::RunOptions`]'s).
    #[serde(default)]
    max_steps: Option<usize>,
}

/// `POST /api/v1/agents` — register an agent; returns its id.
pub(crate) async fn create_agent_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(req): Json<CreateAgentRequest>,
) -> Result<Json<Value>, ApiError> {
    let tenant = tenancy::tenant_authorize(&state, &headers, "agents:write")?;
    let id = state.agents.create(&tenant, req.manifest)?;
    crate::audit::audit(&state, &headers, &tenant, "agent.create", "agent", &id);
    Ok(Json(json!({ "id": id, "status": "created" })))
}

/// `GET /api/v1/agents` — list the caller's tenant's agent ids (cursor-paginated,
/// overview §6).
pub(crate) async fn list_agents_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(page): Query<crate::hardening::PageQuery>,
) -> Result<Json<Value>, ApiError> {
    let tenant = tenancy::tenant_authorize(&state, &headers, "agents:read")?;
    let items: Vec<Value> = state
        .agents
        .list(&tenant)
        .into_iter()
        .map(Value::String)
        .collect();
    Ok(Json(crate::hardening::paginate(items, &page.page())))
}

/// `GET /api/v1/agents/{id}` — fetch a stored agent's manifest (within the caller's tenant).
pub(crate) async fn get_agent_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let tenant = tenancy::tenant_authorize(&state, &headers, "agents:read")?;
    match state.agents.manifest(&tenant, &id) {
        Some(manifest) => Ok(Json(json!({ "id": id, "manifest": manifest }))),
        None => Err(ApiError::new(
            StatusCode::NOT_FOUND,
            "not_found",
            format!("agent `{id}` not found"),
        )),
    }
}

/// `DELETE /api/v1/agents/{id}` — remove a stored agent (within the caller's tenant).
pub(crate) async fn delete_agent_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    let tenant = tenancy::tenant_authorize(&state, &headers, "agents:write")?;
    if state.agents.delete(&tenant, &id) {
        crate::audit::audit(&state, &headers, &tenant, "agent.delete", "agent", &id);
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::new(
            StatusCode::NOT_FOUND,
            "not_found",
            format!("agent `{id}` not found"),
        ))
    }
}

/// `POST /api/v1/agents/{id}/run` — run a stored agent by id.
#[tracing::instrument(name = "api.agents_run_stored", skip_all)]
pub(crate) async fn run_stored_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(req): Json<RunStoredRequest>,
) -> Result<Json<Value>, ApiError> {
    let project = tenancy::run_project(&headers);
    // Authorize the run in the caller's tenant, then resolve the agent *within* that
    // tenant — a caller can only run its own tenant's stored agents.
    match tenancy::tenant_authorize(&state, &headers, "agents:run") {
        Ok(tenant) => match state.agents.definition(&tenant, &id) {
            Some(def) => {
                run_definition(
                    &state,
                    def,
                    req.input,
                    &tenant,
                    project.as_deref(),
                    req.max_steps,
                    &headers,
                )
                .await
            }
            None => Err(ApiError::new(
                StatusCode::NOT_FOUND,
                "not_found",
                format!("agent `{id}` not found"),
            )),
        },
        Err(e) => Err(e),
    }
}

/// Query params for `GET /api/v1/workflows`: filters plus cursor pagination.
#[derive(Debug, Deserialize)]
pub(crate) struct WorkflowListQuery {
    /// Filter to a workflow name.
    workflow: Option<String>,
    /// Filter to a status (e.g. `running`, `completed`, `failed`).
    status: Option<String>,
    /// `limit` + `cursor` (overview §6).
    #[serde(flatten)]
    page: crate::hardening::PageQuery,
}

/// `GET /api/v1/workflows` — list executions, optionally filtered (G4 visibility),
/// cursor-paginated (overview §6).
pub(crate) async fn list_workflows_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<WorkflowListQuery>,
) -> Result<Json<Value>, ApiError> {
    let tenant = tenancy::tenant_authorize(&state, &headers, "workflows:read")?;
    let status = match query
        .status
        .as_deref()
        .map(parse_workflow_status)
        .transpose()
    {
        Ok(status) => status,
        Err(msg) => {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "validation_failed",
                msg,
            ));
        }
    };
    // Fetch the full filtered set; pagination slices it (the cursor is the offset).
    let filter = ExecutionFilter {
        workflow_name: query.workflow,
        status,
        limit: None,
    };
    let executions = state.workflows.list(&filter).await?;
    let items: Vec<Value> = executions
        .into_iter()
        // Tenant isolation: only show executions this tenant owns.
        .filter(|e| state.workflow_visible(&e.execution_id, &tenant))
        .map(|e| serde_json::to_value(e).unwrap_or(Value::Null))
        .collect();
    Ok(Json(crate::hardening::paginate(items, &query.page.page())))
}

/// `GET /api/v1/workflows/{id}` — an execution's status plus its event timeline (G4).
pub(crate) async fn get_workflow_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let tenant = tenancy::tenant_authorize(&state, &headers, "workflows:read")?;
    // Hide cross-tenant executions behind the same 404 as a missing one.
    let missing = || {
        ApiError::new(
            StatusCode::NOT_FOUND,
            "not_found",
            format!("execution `{id}` not found"),
        )
    };
    if !state.workflow_visible(&id, &tenant) {
        return Err(missing());
    }
    let summary = state.workflows.status(&id).await?.ok_or_else(missing)?;
    let events = state.workflows.history(&id).await?;
    Ok(Json(json!({ "execution": summary, "events": events })))
}

/// Reject access to a workflow execution the caller's tenant does not own, hiding its
/// existence behind the same `404` a missing execution returns (used by the write-path
/// signal/approve/cancel routes).
pub(crate) fn require_workflow_visible(
    state: &AppState,
    id: &str,
    tenant: &str,
) -> Result<(), ApiError> {
    if state.workflow_visible(id, tenant) {
        Ok(())
    } else {
        Err(ApiError::new(
            StatusCode::NOT_FOUND,
            "not_found",
            format!("execution `{id}` not found"),
        ))
    }
}

/// `GET /workflows` — a minimal, read-only HTML UI over the visibility API (G4).
pub(crate) async fn workflows_ui_handler() -> Html<&'static str> {
    Html(WORKFLOWS_UI)
}

/// Parse a workflow status name (case-insensitive) for the list filter.
fn parse_workflow_status(s: &str) -> Result<WorkflowState, String> {
    match s.to_ascii_lowercase().as_str() {
        "created" => Ok(WorkflowState::Created),
        "validated" => Ok(WorkflowState::Validated),
        "scheduled" => Ok(WorkflowState::Scheduled),
        "running" => Ok(WorkflowState::Running),
        "waiting" => Ok(WorkflowState::Waiting),
        "resumed" => Ok(WorkflowState::Resumed),
        "compensating" => Ok(WorkflowState::Compensating),
        "completed" => Ok(WorkflowState::Completed),
        "failed" => Ok(WorkflowState::Failed),
        "cancelled" | "canceled" => Ok(WorkflowState::Cancelled),
        other => Err(format!("unknown status `{other}`")),
    }
}

/// A self-contained read-only UI that calls the visibility JSON API.
const WORKFLOWS_UI: &str = r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>Apex Workflows</title>
<style>
  body { font: 14px system-ui, sans-serif; margin: 0; padding: 1.5rem; color: #1a1a1a; }
  h1 { font-size: 1.2rem; } h2 { font-size: 1rem; margin-top: 1.2rem; }
  .row { display: flex; gap: 1.5rem; align-items: flex-start; }
  .col { flex: 1; min-width: 0; }
  table { border-collapse: collapse; width: 100%; }
  th, td { text-align: left; padding: .4rem .6rem; border-bottom: 1px solid #e3e3e3; }
  tbody tr { cursor: pointer; } tbody tr:hover { background: #f3f6ff; }
  .pill { padding: .1rem .5rem; border-radius: 1rem; font-size: .8rem; }
  .Running, .Waiting { background: #fff4d6; } .Completed { background: #d8f5dd; }
  .Failed, .Cancelled { background: #fbdcdc; }
  pre { background: #f6f8fa; padding: .8rem; border-radius: 6px; overflow-x: auto; }
  .muted { color: #888; } button { font: inherit; }
</style>
</head>
<body>
  <h1>Apex Workflows</h1>
  <div class="muted">Read-only visibility over <code>/api/v1/workflows</code>.
    <button onclick="load()">Refresh</button></div>
  <div class="row" style="margin-top:1rem">
    <div class="col">
      <h2>Executions</h2>
      <table><thead><tr><th>id</th><th>workflow</th><th>status</th></tr></thead>
      <tbody id="list"><tr><td colspan="3" class="muted">loading…</td></tr></tbody></table>
    </div>
    <div class="col"><h2>Detail</h2><div id="detail" class="muted">Select an execution.</div></div>
  </div>
<script>
async function load() {
  const tbody = document.getElementById('list');
  try {
    const res = await fetch('/api/v1/workflows');
    const data = await res.json();
    const rows = (data.data || []);
    if (!rows.length) { tbody.innerHTML = '<tr><td colspan="3" class="muted">no executions</td></tr>'; return; }
    tbody.innerHTML = '';
    for (const e of rows) {
      const tr = document.createElement('tr');
      tr.onclick = () => detail(e.execution_id);
      tr.innerHTML = `<td>${e.execution_id}</td><td>${e.workflow_name}</td>`
        + `<td><span class="pill ${e.status}">${e.status}</span></td>`;
      tbody.appendChild(tr);
    }
  } catch (err) { tbody.innerHTML = `<tr><td colspan="3">error: ${err}</td></tr>`; }
}
async function detail(id) {
  const el = document.getElementById('detail');
  el.textContent = 'loading…';
  try {
    const res = await fetch('/api/v1/workflows/' + encodeURIComponent(id));
    if (!res.ok) { el.textContent = 'not found'; return; }
    const d = await res.json();
    const acts = Object.entries(d.execution.activities || {})
      .map(([k, v]) => `<li>${k}: <span class="pill ${v}">${v}</span></li>`).join('');
    const events = (d.events || []).map((ev, i) => `${String(i + 1).padStart(3)}. ${JSON.stringify(ev)}`).join('\n');
    el.innerHTML = `<div><b>${d.execution.execution_id}</b> — ${d.execution.workflow_name} `
      + `v${d.execution.workflow_version} — <span class="pill ${d.execution.status}">${d.execution.status}</span></div>`
      + `<ul>${acts}</ul><h2>Timeline</h2><pre>${events.replace(/</g, '&lt;')}</pre>`;
  } catch (err) { el.textContent = 'error: ' + err; }
}
load();
</script>
</body>
</html>"#;

/// An API error rendered as the standard envelope.
#[derive(Debug)]
pub(crate) struct ApiError {
    pub(crate) status: StatusCode,
    code: &'static str,
    pub(crate) message: String,
}

impl ApiError {
    pub(crate) fn new(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            status,
            code,
            message: message.into(),
        }
    }
}

impl From<Error> for ApiError {
    fn from(e: Error) -> Self {
        match e {
            Error::Config(m) | Error::Invalid(m) => {
                ApiError::new(StatusCode::BAD_REQUEST, "validation_failed", m)
            }
            Error::NotFound(m) => ApiError::new(StatusCode::NOT_FOUND, "not_found", m),
            Error::Forbidden(m) => ApiError::new(StatusCode::FORBIDDEN, "forbidden", m),
            Error::Conflict(m) => ApiError::new(StatusCode::CONFLICT, "conflict", m),
            Error::QuotaExceeded(m) => {
                ApiError::new(StatusCode::TOO_MANY_REQUESTS, "quota_exceeded", m)
            }
            Error::Provider { message, .. } => {
                ApiError::new(StatusCode::BAD_GATEWAY, "provider_error", message)
            }
            other => ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal_error",
                other.to_string(),
            ),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let kind = match self.status.as_u16() {
            400..=499 => "client_error",
            _ => "server_error",
        };
        let body = json!({
            "error": {
                "code": self.code,
                "message": self.message,
                "type": kind,
                "status": self.status.as_u16(),
            }
        });
        (self.status, Json(body)).into_response()
    }
}
