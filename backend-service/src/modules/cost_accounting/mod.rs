use axum::{routing, Router};
use std::sync::Arc;

use crate::AppState;

pub mod routes;

pub use routes::{ingest_event, get_aggregate};

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/events", routing::post(ingest_event))
        .route("/aggregate", routing::get(get_aggregate))
}
