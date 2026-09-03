//! Local RMS modular-monolith backend.
//!
//! The crate keeps Connect, Projects, Live, Replay, and Control as separate route modules while
//! sharing one in-memory transactional catalog. Replay routes deliberately have no Control API.

pub mod domain;

mod edge_heartbeat;
mod error;
mod import_storage;
mod local_security;
mod network_discovery;
mod routes;
mod rrd_fixture;
mod state;

use axum::{Json, Router, middleware, routing::get};
use serde_json::{Value, json};

pub use network_discovery::{
    DiscoveredSource, DiscoveryCancellation, DiscoveryObservation, DiscoveryProvider,
    DiscoveryProviderError, FakeDiscoveryProvider, PinnedDiscoveryEndpoint, ProviderVerification,
    ProviderVerificationStatus,
};
pub use state::AppState;

/// Builds the RMS API around the supplied catalog state.
pub fn router(state: AppState) -> Router {
    routes::router()
        .route("/health", get(health))
        .with_state(state)
        .layer(middleware::from_fn(local_security::validate_request))
}

/// Builds the local-development API with its deterministic fixture catalog.
pub fn fixture_router() -> Router {
    router(AppState::fixture())
}

/// Builds the local-development API backed by `RMS_STORAGE_DIR` or the OS data directory.
pub fn durable_fixture_router() -> std::io::Result<Router> {
    AppState::from_environment().map(router)
}

async fn health() -> Json<Value> {
    Json(json!({ "status": "ok", "service": "rms-server" }))
}
