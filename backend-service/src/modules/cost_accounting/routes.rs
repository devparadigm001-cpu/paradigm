use axum::{
    extract::{Query, State},
    http::StatusCode,
    Json,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, sync::Arc};
use uuid::Uuid;

use crate::AppState;

// ── Ingest ────────────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct IngestRequest {
    pub device_id: String,
    pub user_id: Option<String>,
    /// 'ghost_mode' | 'record_mode' | 'chat_mode' | 'form_memory'
    pub feature: String,
    /// Matches confidence_calibration.model_source values, e.g. 'qwen2.5-0.5b-label'
    pub model_source: String,
    pub cost_usd: f64,
    /// False for out-of-scope Chat Mode commands that shouldn't count against tier limits.
    pub billable: bool,
}

#[derive(Debug, Serialize)]
pub struct IngestResponse {
    pub id: Uuid,
}

pub async fn ingest_event(
    State(state): State<Arc<AppState>>,
    Json(body): Json<IngestRequest>,
) -> Result<(StatusCode, Json<IngestResponse>), (StatusCode, String)> {
    let id = Uuid::new_v4();

    sqlx::query(
        "INSERT INTO execution_cost_events \
         (id, device_id, user_id, feature, model_source, cost_usd, billable) \
         VALUES ($1, $2, $3, $4, $5, $6, $7)",
    )
    .bind(id)
    .bind(&body.device_id)
    .bind(&body.user_id)
    .bind(&body.feature)
    .bind(&body.model_source)
    .bind(body.cost_usd)
    .bind(body.billable)
    .execute(&state.db)
    .await
    .map_err(|e| {
        tracing::error!("Failed to insert cost event: {}", e);
        (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
    })?;

    tracing::debug!(
        id = %id,
        device_id = %body.device_id,
        feature = %body.feature,
        model_source = %body.model_source,
        cost_usd = body.cost_usd,
        billable = body.billable,
        "Cost event recorded"
    );

    Ok((StatusCode::CREATED, Json(IngestResponse { id })))
}

// ── Aggregate ─────────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct AggregateParams {
    pub device_id: Option<String>,
    pub user_id: Option<String>,
    /// Start of the time window (RFC 3339, e.g. "2025-01-01T00:00:00Z").
    pub from: DateTime<Utc>,
    /// End of the time window. Defaults to now.
    pub to: Option<DateTime<Utc>>,
    /// When true, only billable events are counted.
    pub billable_only: Option<bool>,
}

#[derive(Debug, Serialize, Default)]
pub struct FeatureSummary {
    pub total_cost_usd: f64,
    pub event_count: i64,
    pub billable_cost_usd: f64,
    pub billable_count: i64,
}

#[derive(Debug, Serialize)]
pub struct AggregateResponse {
    pub total_cost_usd: f64,
    pub event_count: i64,
    pub billable_cost_usd: f64,
    pub billable_count: i64,
    pub by_feature: HashMap<String, FeatureSummary>,
}

#[derive(sqlx::FromRow)]
struct FeatureRow {
    feature: String,
    event_count: i64,
    total_cost_usd: f64,
    billable_count: i64,
    billable_cost_usd: f64,
}

pub async fn get_aggregate(
    State(state): State<Arc<AppState>>,
    Query(params): Query<AggregateParams>,
) -> Result<Json<AggregateResponse>, (StatusCode, String)> {
    if params.device_id.is_none() && params.user_id.is_none() {
        return Err((
            StatusCode::BAD_REQUEST,
            "At least one of device_id or user_id is required".into(),
        ));
    }

    let to = params.to.unwrap_or_else(Utc::now);

    // The identity filter is wrapped in parens so "device_id only" and
    // "user_id only" both work without touching the time bounds.
    let mut qb = sqlx::QueryBuilder::<sqlx::Postgres>::new(
        "SELECT \
            feature, \
            COUNT(*) AS event_count, \
            COALESCE(SUM(cost_usd), 0.0) AS total_cost_usd, \
            COUNT(*) FILTER (WHERE billable) AS billable_count, \
            COALESCE(SUM(cost_usd) FILTER (WHERE billable), 0.0) AS billable_cost_usd \
         FROM execution_cost_events \
         WHERE (",
    );

    match (&params.device_id, &params.user_id) {
        (Some(did), Some(uid)) => {
            qb.push("device_id = ").push_bind(did.as_str());
            qb.push(" AND user_id = ").push_bind(uid.as_str());
        }
        (Some(did), None) => {
            qb.push("device_id = ").push_bind(did.as_str());
        }
        (None, Some(uid)) => {
            qb.push("user_id = ").push_bind(uid.as_str());
        }
        (None, None) => unreachable!(),
    }

    qb.push(") AND recorded_at >= ").push_bind(params.from);
    qb.push(" AND recorded_at < ").push_bind(to);

    if params.billable_only.unwrap_or(false) {
        qb.push(" AND billable = true");
    }

    qb.push(" GROUP BY feature");

    let rows: Vec<FeatureRow> = qb
        .build_query_as()
        .fetch_all(&state.db)
        .await
        .map_err(|e| {
            tracing::error!("Aggregate query failed: {}", e);
            (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
        })?;

    let mut by_feature: HashMap<String, FeatureSummary> = HashMap::new();
    let mut total_cost_usd = 0.0f64;
    let mut event_count = 0i64;
    let mut billable_cost_usd = 0.0f64;
    let mut billable_count = 0i64;

    for row in rows {
        total_cost_usd += row.total_cost_usd;
        event_count += row.event_count;
        billable_cost_usd += row.billable_cost_usd;
        billable_count += row.billable_count;

        by_feature.insert(
            row.feature,
            FeatureSummary {
                total_cost_usd: row.total_cost_usd,
                event_count: row.event_count,
                billable_cost_usd: row.billable_cost_usd,
                billable_count: row.billable_count,
            },
        );
    }

    Ok(Json(AggregateResponse {
        total_cost_usd,
        event_count,
        billable_cost_usd,
        billable_count,
        by_feature,
    }))
}
