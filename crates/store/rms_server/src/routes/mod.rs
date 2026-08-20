use axum::Router;

use crate::AppState;

mod catalog;
mod control;
mod projects;
mod sessions;

pub(crate) fn router() -> Router<AppState> {
    Router::new()
        .merge(catalog::router())
        .merge(projects::router())
        .merge(sessions::router())
        .merge(control::router())
}
