use axum::{
    extract::{Request, State},
    http::StatusCode,
    middleware::Next,
    response::Response,
};
use std::sync::Arc;

use crate::AppState;

/// Middleware that validates the `X-Api-Key` header against `config.api_secret`.
/// If `api_secret` is empty (dev mode), all requests are allowed through.
pub async fn require_api_key(
    State(state): State<Arc<AppState>>,
    request: Request,
    next: Next,
) -> Result<Response, StatusCode> {
    if state.config.api_secret.is_empty() {
        return Ok(next.run(request).await);
    }

    let key = request
        .headers()
        .get("x-api-key")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    if key != state.config.api_secret {
        tracing::warn!("Rejected request: invalid or missing X-Api-Key");
        return Err(StatusCode::UNAUTHORIZED);
    }

    Ok(next.run(request).await)
}
