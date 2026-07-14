//! Tool-discovery route: `GET /api/v1/tools`.
//!
//! Lists the run registry's tools — the built-ins **and** any enabled plugin
//! capabilities registered at startup — with id, description, and category, so UIs (the
//! dashboard's Agent Studio tool picker) can offer them without hardcoding names. Pure
//! discovery metadata (no values, no tenant data), so it is unauthenticated like
//! `/healthz` and `/metrics`.
//!
//! `.unwrap()`/`.expect()`/`unreachable!()` on request-derived data are denied here
//! (RM-AIM-P3 SRV-306) — a malformed client request must return a mapped `ApiError`,
//! never panic.

#![cfg_attr(
    not(test),
    warn(clippy::unwrap_used, clippy::expect_used, clippy::unreachable)
)]

use crate::AppState;
use crate::hardening::{PageQuery, paginate};
use axum::{
    Json, Router,
    extract::{Query, State},
    routing::get,
};
use serde_json::{Value, json};
use std::sync::Arc;

pub(crate) fn routes() -> Router<Arc<AppState>> {
    Router::new().route("/api/v1/tools", get(list_tools))
}

/// `GET /api/v1/tools` — the registered tool catalog (id + description +
/// category), cursor-paginated (overview §6, RM-GA-P4 API-701).
#[utoipa::path(
    get,
    path = "/api/v1/tools",
    tag = "tools",
    params(
        ("limit" = Option<usize>, Query, description = "Max items per page (default 25, max 100)."),
        ("cursor" = Option<String>, Query, description = "Opaque pagination cursor from a prior page's next_cursor."),
    ),
    responses((status = 200, description = "The registered tool catalog (built-ins + enabled plugin tools).")),
)]
pub(crate) async fn list_tools(
    State(state): State<Arc<AppState>>,
    Query(page): Query<PageQuery>,
) -> Json<Value> {
    let tools: Vec<Value> = state
        .registry
        .metadata()
        .into_iter()
        .map(|m| {
            json!({
                "id": m.id,
                "description": m.description,
                "category": m.category,
                "permissions": m.permissions,
            })
        })
        .collect();
    Json(paginate(tools, &page.page()))
}
