use crate::AppState;
use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde::Serialize;
use uuid::Uuid;

#[derive(Debug, Serialize)]
struct JobView {
    id: String,
    tenant_id: String,
    source: String,
    status: String,
    attempts: i32,
    last_error: Option<String>,
    created_at: String,
    updated_at: String,
    first_inbox_id: String,
    last_inbox_id: String,
}

#[derive(Debug, Serialize)]
struct ErrorBody {
    error: String,
}

fn json_error(status: StatusCode, msg: impl Into<String>) -> Response {
    (status, Json(ErrorBody { error: msg.into() })).into_response()
}

pub async fn get_job(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    let job_id: Uuid = match id.parse() {
        Ok(v) => v,
        Err(_) => return json_error(StatusCode::BAD_REQUEST, "invalid job id"),
    };

    let job = match infra::jobs::get_job(&state.pool, job_id).await {
        Ok(v) => v,
        Err(_) => return json_error(StatusCode::INTERNAL_SERVER_ERROR, "failed to fetch job"),
    };

    let Some(j) = job else {
        return json_error(StatusCode::NOT_FOUND, "job not found");
    };

    let view = JobView {
        id: j.id.to_string(),
        tenant_id: j.tenant_id.to_string(),
        source: j.source,
        status: j.status,
        attempts: j.attempts,
        last_error: j.last_error,
        created_at: j.created_at.to_rfc3339(),
        updated_at: j.updated_at.to_rfc3339(),
        first_inbox_id: j.first_inbox_id.to_string(),
        last_inbox_id: j.last_inbox_id.to_string(),
    };

    (StatusCode::OK, Json(view)).into_response()
}
