use axum::{middleware, routing, Router};
use std::sync::Arc;
use tower_http::cors::{Any, CorsLayer};
use tower_http::trace::TraceLayer;

mod auth;
mod config;
mod db;
mod modules;

use config::Config;

#[derive(Clone)]
pub struct AppState {
    pub db: sqlx::PgPool,
    pub config: Arc<Config>,
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "paradigm_service=info,tower_http=info".into()),
        )
        .with_target(false)
        .init();

    dotenvy::dotenv().ok();

    let config = Arc::new(Config::from_env());
    let pool = db::connect(&config.database_url).await;

    let state = Arc::new(AppState {
        db: pool,
        config: config.clone(),
    });

    let cost_routes = modules::cost_accounting::router()
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            auth::require_api_key,
        ));

    let app = Router::new()
        .route("/health", routing::get(|| async { "ok" }))
        .nest("/v1/cost", cost_routes)
        .with_state(state)
        .layer(
            CorsLayer::new()
                .allow_origin(Any)
                .allow_methods(Any)
                .allow_headers(Any),
        )
        .layer(TraceLayer::new_for_http());

    let addr = format!("0.0.0.0:{}", config.port);
    tracing::info!("paradigm-service listening on {}", addr);

    let listener = tokio::net::TcpListener::bind(&addr).await.unwrap();
    axum::serve(listener, app).await.unwrap();
}
