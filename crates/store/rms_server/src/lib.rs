//! Local RMS modular-monolith backend.
//!
//! The crate keeps Connect, Projects, Live, Replay, and Control as separate route modules while
//! sharing one in-memory transactional catalog. Replay routes deliberately have no Control API.

pub mod domain;

mod error;
mod routes;
mod state;

use axum::{Json, Router, routing::get};
use serde_json::{Value, json};

pub use state::AppState;

/// Builds the RMS API around the supplied catalog state.
pub fn router(state: AppState) -> Router {
    routes::router()
        .route("/health", get(health))
        .with_state(state)
}

/// Builds the local-development API with its deterministic fixture catalog.
pub fn fixture_router() -> Router {
    router(AppState::fixture())
}

async fn health() -> Json<Value> {
    Json(json!({ "status": "ok", "service": "rms-server" }))
}
