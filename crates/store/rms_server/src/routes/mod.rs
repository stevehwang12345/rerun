use axum::Router;

use crate::AppState;

mod catalog;
mod control;
mod discovery;
mod edge_agents;
mod imports;
mod projects;
mod sessions;

pub(crate) fn router() -> Router<AppState> {
    Router::new()
        .merge(catalog::router())
        .merge(discovery::router())
        .merge(edge_agents::router())
        .merge(projects::router())
        .merge(imports::router())
        .merge(sessions::router())
        .merge(control::router())
}
