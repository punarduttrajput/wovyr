//! Tool-discovery route: `GET /api/v1/tools`.
//!
//! Lists the run registry's tools — the built-ins **and** any enabled plugin
//! capabilities registered at startup — with id, description, and category, so UIs (the
//! dashboard's Agent Studio tool picker) can offer them without hardcoding names. Pure
//! discovery metadata (no values, no tenant data), so it is unauthenticated like
//! `/healthz` and `/metrics`.

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
async fn list_tools(
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
