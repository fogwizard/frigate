use crate::db::Db;
use crate::detect::CameraStatusMap;
use crate::stream_sign::{self, AuthFail};
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;
use tower_http::cors::CorsLayer;
use tower_http::services::ServeDir;
use tower_http::trace::TraceLayer;
use vigil_common::{Config, HealthStatus, StatusResponse};

pub struct AppState {
    pub cfg: Arc<Config>,
    pub db: Arc<Db>,
    pub started: Instant,
    pub infer_connected: Arc<tokio::sync::RwLock<bool>>,
    pub statuses: CameraStatusMap,
    pub version: String,
}

pub fn router(state: Arc<AppState>, web_dir: PathBuf) -> Router {
    let data_dir = state.cfg.paths.data_dir.clone();
    Router::new()
        .route("/api/health", get(health))
        .route("/api/status", get(status))
        .route("/api/cameras", get(cameras))
        .route("/api/events", get(events))
        .route("/api/events/{id}", get(event_one))
        .route("/api/recordings", get(recordings))
        .route("/api/snapshots/{cam}", get(snapshot))
        .route("/api/media/{*path}", get(media_file))
        .route("/api/streams", get(streams_list))
        .route("/api/streams/auth", get(streams_auth))
        .route("/api/streams/{camera}/play", get(streams_play))
        .nest_service("/files", ServeDir::new(data_dir))
        .fallback_service(ServeDir::new(web_dir).append_index_html_on_directories(true))
        .layer(CorsLayer::permissive())
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

async fn health(State(st): State<Arc<AppState>>) -> Json<HealthStatus> {
    Json(HealthStatus {
        ok: true,
        version: st.version.clone(),
        cameras: st.cfg.cameras.len(),
        infer_connected: *st.infer_connected.read().await,
        uptime_secs: st.started.elapsed().as_secs(),
    })
}

async fn status(State(st): State<Arc<AppState>>) -> Json<StatusResponse> {
    let cameras: Vec<_> = st.statuses.read().await.values().cloned().collect();
    Json(StatusResponse {
        health: HealthStatus {
            ok: true,
            version: st.version.clone(),
            cameras: st.cfg.cameras.len(),
            infer_connected: *st.infer_connected.read().await,
            uptime_secs: st.started.elapsed().as_secs(),
        },
        cameras,
    })
}

async fn cameras(State(st): State<Arc<AppState>>) -> Json<serde_json::Value> {
    let names: Vec<_> = st.cfg.cameras.keys().cloned().collect();
    Json(serde_json::json!({ "cameras": names }))
}

#[derive(Deserialize)]
struct ListQuery {
    camera: Option<String>,
    limit: Option<i64>,
    after: Option<DateTime<Utc>>,
    before: Option<DateTime<Utc>>,
}

async fn events(
    State(st): State<Arc<AppState>>,
    Query(q): Query<ListQuery>,
) -> Result<Json<serde_json::Value>, AppError> {
    let limit = q.limit.unwrap_or(50).clamp(1, 500);
    let rows = st
        .db
        .list_events(q.camera.as_deref(), limit)
        .map_err(AppError::internal)?;
    Ok(Json(serde_json::json!({ "events": rows })))
}

async fn event_one(
    State(st): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, AppError> {
    match st.db.get_event(&id).map_err(AppError::internal)? {
        Some(ev) => Ok(Json(serde_json::json!(ev))),
        None => Err(AppError::not_found("event not found")),
    }
}

async fn recordings(
    State(st): State<Arc<AppState>>,
    Query(q): Query<ListQuery>,
) -> Result<Json<serde_json::Value>, AppError> {
    let limit = q.limit.unwrap_or(100).clamp(1, 1000);
    let rows = st
        .db
        .list_segments(q.camera.as_deref(), q.after, q.before, limit)
        .map_err(AppError::internal)?;
    Ok(Json(serde_json::json!({ "recordings": rows })))
}

async fn snapshot(
    State(st): State<Arc<AppState>>,
    Path(cam): Path<String>,
) -> Result<Response, AppError> {
    let path = st.cfg.live_snapshots_dir().join(format!("{cam}.jpg"));
    if !path.exists() {
        return Err(AppError::not_found("snapshot not available"));
    }
    let bytes = std::fs::read(&path).map_err(AppError::internal)?;
    Ok((
        [(axum::http::header::CONTENT_TYPE, "image/jpeg")],
        bytes,
    )
        .into_response())
}

fn require_stream_token(st: &AppState, headers: &HeaderMap) -> Result<(), AppError> {
    let expected = st.cfg.stream.api_token.trim();
    if expected.is_empty() {
        return Ok(());
    }
    let got = headers
        .get("X-Vigil-Token")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if got != expected {
        return Err(AppError::unauthorized("missing or invalid X-Vigil-Token"));
    }
    Ok(())
}

fn require_stream_enabled(st: &AppState) -> Result<(), AppError> {
    if !st.cfg.stream.enabled {
        return Err(AppError::unavailable("stream sharing disabled"));
    }
    Ok(())
}

async fn streams_list(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, AppError> {
    require_stream_enabled(&st)?;
    require_stream_token(&st, &headers)?;
    let mut cameras: Vec<_> = st.cfg.cameras.keys().cloned().collect();
    cameras.sort();
    Ok(Json(serde_json::json!({ "cameras": cameras })))
}

async fn streams_play(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(camera): Path<String>,
) -> Result<Json<serde_json::Value>, AppError> {
    require_stream_enabled(&st)?;
    require_stream_token(&st, &headers)?;
    if !st.cfg.cameras.contains_key(&camera) {
        return Err(AppError::not_found("camera not found"));
    }
    let play = stream_sign::issue_play_url(&st.cfg.stream, &camera, Utc::now());
    Ok(Json(serde_json::json!({
        "camera": camera,
        "url": play.url,
        "expires_at": play.expires_at,
    })))
}

#[derive(Deserialize)]
struct StreamAuthQuery {
    camera: String,
    exp: i64,
    sig: String,
}

/// Nginx `auth_request` / MediaMTX external auth callback.
async fn streams_auth(
    State(st): State<Arc<AppState>>,
    Query(q): Query<StreamAuthQuery>,
) -> Result<StatusCode, AppError> {
    require_stream_enabled(&st)?;
    if !st.cfg.cameras.contains_key(&q.camera) {
        return Err(AppError::not_found("camera not found"));
    }
    match stream_sign::check_auth(&st.cfg.stream, &q.camera, q.exp, &q.sig, Utc::now()) {
        Ok(()) => Ok(StatusCode::OK),
        Err(AuthFail::Expired) => Err(AppError::forbidden("url expired")),
        Err(AuthFail::BadSig) => Err(AppError::forbidden("invalid signature")),
    }
}

async fn media_file(
    State(st): State<Arc<AppState>>,
    Path(path): Path<String>,
) -> Result<Response, AppError> {
    let base = st.cfg.paths.data_dir.canonicalize().unwrap_or_else(|_| st.cfg.paths.data_dir.clone());
    let joined = base.join(&path);
    let canon = joined.canonicalize().map_err(|_| AppError::not_found("not found"))?;
    if !canon.starts_with(&base) {
        return Err(AppError::not_found("not found"));
    }
    let bytes = std::fs::read(&canon).map_err(AppError::internal)?;
    let ctype = if path.ends_with(".mp4") {
        "video/mp4"
    } else if path.ends_with(".jpg") || path.ends_with(".jpeg") {
        "image/jpeg"
    } else {
        "application/octet-stream"
    };
    Ok(([(axum::http::header::CONTENT_TYPE, ctype)], bytes).into_response())
}

pub struct AppError {
    status: StatusCode,
    msg: String,
}

impl AppError {
    fn internal(e: impl ToString) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            msg: e.to_string(),
        }
    }
    fn not_found(msg: impl Into<String>) -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            msg: msg.into(),
        }
    }
    fn unauthorized(msg: impl Into<String>) -> Self {
        Self {
            status: StatusCode::UNAUTHORIZED,
            msg: msg.into(),
        }
    }
    fn forbidden(msg: impl Into<String>) -> Self {
        Self {
            status: StatusCode::FORBIDDEN,
            msg: msg.into(),
        }
    }
    fn unavailable(msg: impl Into<String>) -> Self {
        Self {
            status: StatusCode::SERVICE_UNAVAILABLE,
            msg: msg.into(),
        }
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        (self.status, Json(serde_json::json!({ "error": self.msg }))).into_response()
    }
}
