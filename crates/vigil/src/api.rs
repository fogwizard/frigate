use crate::db::Db;
use crate::detect::CameraStatusMap;
use crate::stream_sign::{self, AuthFail};
use axum::body::Body;
use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tower_http::cors::CorsLayer;
use tower_http::services::ServeDir;
use tower_http::set_header::SetResponseHeaderLayer;
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
        .route("/api/version", get(version))
        .route("/api/status", get(status))
        .route("/api/cameras", get(cameras))
        .route("/api/events", get(events))
        .route("/api/events/{id}", get(event_one))
        .route("/api/events/{id}/snapshot", get(event_snapshot))
        .route("/api/events/{id}/clip", get(event_clip))
        .route("/api/recordings", get(recordings))
        .route("/api/recordings/{id}/file", get(recording_file))
        .route("/api/snapshots/{cam}", get(snapshot))
        .route("/api/media/{*path}", get(media_file))
        .route("/api/miniprogram/login", post(crate::mp_api::login))
        .route(
            "/api/miniprogram/profile",
            get(crate::mp_api::profile_get).post(crate::mp_api::profile_save),
        )
        .route("/api/streams", get(crate::mp_api::streams_list))
        .route("/api/streams/auth", get(streams_auth))
        .route("/api/streams/{camera}/play", get(crate::mp_api::streams_play))
        .route("/flv/{file}", get(crate::mp_api::streams_flv))
        .route("/hls/segment.ts", get(crate::mp_api::streams_hls_segment))
        .route("/hls/{file}", get(crate::mp_api::streams_hls))
        .route("/api/boarding/records", get(crate::mp_api::boarding_records))
        .route("/api/boarding/assign", post(crate::mp_api::boarding_assign))
        .route("/api/boarding/assigned", get(crate::mp_api::boarding_assigned))
        .route("/api/boarding/cages", post(crate::mp_api::boarding_cages))
        .route("/api/boarding/orders", post(crate::mp_api::boarding_create_order))
        .route(
            "/api/boarding/orders/{order_id}",
            get(crate::mp_api::boarding_get_order),
        )
        .route("/api/feeding", post(crate::mp_api::feeding))
        .route("/api/feeding/clear", post(clear_feeding))
        .route("/api/watering", post(crate::mp_api::watering))
        .route("/api/doors/open", post(crate::mp_api::door_open))
        .nest_service("/files", ServeDir::new(data_dir))
        .fallback_service(ServeDir::new(web_dir).append_index_html_on_directories(true))
        .layer(CorsLayer::permissive())
        .layer(TraceLayer::new_for_http())
        .layer(SetResponseHeaderLayer::overriding(
            header::CACHE_CONTROL,
            HeaderValue::from_static("no-cache"),
        ))
        .with_state(state)
}

async fn data_dir_bytes_cached(dir: &std::path::Path) -> u64 {
    const FRESH: Duration = Duration::from_secs(5);
    static CACHE: Mutex<Option<(Instant, PathBuf, u64)>> = Mutex::new(None);
    {
        let slot = CACHE.lock().unwrap_or_else(|err| err.into_inner());
        if let Some((at, path, bytes)) = slot.as_ref() {
            if path == dir && at.elapsed() < FRESH {
                return *bytes;
            }
        }
    }
    let dir = dir.to_path_buf();
    let measured = tokio::task::spawn_blocking({
        let dir = dir.clone();
        move || crate::record::data_dir_bytes(&dir)
    })
    .await
    .unwrap_or(0);
    let mut slot = CACHE.lock().unwrap_or_else(|err| err.into_inner());
    *slot = Some((Instant::now(), dir, measured));
    measured
}

async fn health_status(st: &AppState) -> HealthStatus {
    HealthStatus {
        ok: true,
        version: st.version.clone(),
        cameras: st.cfg.cameras.len(),
        infer_connected: *st.infer_connected.read().await,
        uptime_secs: st.started.elapsed().as_secs(),
        data_bytes: data_dir_bytes_cached(&st.cfg.paths.data_dir).await,
        max_gb: st.cfg.record.max_gb,
        feed_serial: vigil_modbus::serial_link().to_string(),
        feed_in_secs: crate::feed::secs_until_next_feed(Utc::now()),
        feed_trying: crate::feed::feeding_in_progress(&st.cfg, &st.db, Utc::now()),
        feed_retries: crate::feed::open_failure_count(Utc::now()),
    }
}

async fn health(State(st): State<Arc<AppState>>) -> Json<HealthStatus> {
    Json(health_status(&st).await)
}

/// Public, no auth — for HTTPS smoke tests (e.g. https://www.rujibao.cn/api/version).
async fn version(State(st): State<Arc<AppState>>) -> impl IntoResponse {
    let body = if st.version.starts_with('v') {
        st.version.clone()
    } else {
        format!("v{}", st.version)
    };
    (
        [(header::CONTENT_TYPE, HeaderValue::from_static("text/plain; charset=utf-8"))],
        body,
    )
}

async fn status(State(st): State<Arc<AppState>>) -> Json<StatusResponse> {
    let cameras: Vec<_> = st.statuses.read().await.values().cloned().collect();
    Json(StatusResponse {
        health: health_status(&st).await,
        cameras,
    })
}

#[derive(Deserialize)]
struct ClearQuery {
    camera: Option<String>,
}

/// Delete this UTC day's feed rows. Restart Vigil after changing the clock so the
/// scheduler reads the empty day from the database.
async fn clear_feeding(
    State(st): State<Arc<AppState>>,
    Query(q): Query<ClearQuery>,
) -> Result<Json<serde_json::Value>, AppError> {
    let camera = q
        .camera
        .as_deref()
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(str::to_string);
    if let Some(name) = &camera {
        if !st.cfg.cameras.contains_key(name) {
            return Err(AppError::not_found("camera not found"));
        }
    }
    let day = Utc::now().date_naive();
    let removed = st
        .db
        .delete_feed_events_on_day(camera.as_deref(), day)
        .map_err(AppError::internal)?;
    for ev in &removed {
        remove_feed_file(ev.snapshot_path.as_deref());
        remove_feed_file(ev.clip_path.as_deref());
    }
    let cameras = match &camera {
        Some(name) => vec![name.clone()],
        None => {
            let mut names: Vec<String> = st.cfg.cameras.keys().cloned().collect();
            names.sort();
            names
        }
    };
    let n = removed.len();
    tracing::info!(%day, removed = n, ?camera, "cleared today's feed events");
    Ok(Json(serde_json::json!({
        "success": true,
        "day": day.to_string(),
        "removed": n,
        "cameras": cameras,
    })))
}

fn remove_feed_file(path: Option<&str>) {
    let Some(path) = path else {
        return;
    };
    let file = std::path::Path::new(path);
    let parent = file
        .parent()
        .and_then(|dir| dir.file_name())
        .and_then(|name| name.to_str());
    if matches!(parent, Some("clips") | Some("snapshots")) {
        let _ = std::fs::remove_file(file);
    }
}

async fn cameras(State(st): State<Arc<AppState>>) -> Json<serde_json::Value> {
    let mut names: Vec<_> = st.cfg.cameras.keys().cloned().collect();
    names.sort();
    let streams: Vec<_> = names
        .iter()
        .map(|name| {
            serde_json::json!({
                "name": name,
                "live_url": st.cfg.stream.browser_play_url(name),
            })
        })
        .collect();
    Json(serde_json::json!({ "cameras": names, "streams": streams }))
}

#[derive(Deserialize)]
struct ListQuery {
    camera: Option<String>,
    limit: Option<i64>,
    offset: Option<i64>,
    after: Option<DateTime<Utc>>,
    before: Option<DateTime<Utc>>,
}

async fn events(
    State(st): State<Arc<AppState>>,
    Query(q): Query<ListQuery>,
) -> Result<Json<serde_json::Value>, AppError> {
    let limit = q.limit.unwrap_or(50).clamp(1, 500);
    let offset = q.offset.unwrap_or(0).max(0);
    let total = st
        .db
        .count_events(q.camera.as_deref())
        .map_err(AppError::internal)?;
    let rows = st
        .db
        .list_events(q.camera.as_deref(), limit, offset)
        .map_err(AppError::internal)?;
    Ok(Json(serde_json::json!({
        "events": rows,
        "total": total,
        "offset": offset,
        "limit": limit
    })))
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

async fn event_snapshot(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Response, AppError> {
    let ev = st
        .db
        .get_event(&id)
        .map_err(AppError::internal)?
        .ok_or_else(|| AppError::not_found("event not found"))?;
    let path = ev
        .snapshot_path
        .ok_or_else(|| AppError::not_found("snapshot not available"))?;
    serve_stored(&st.cfg.snapshots_dir(), &path, &headers)
}

async fn event_clip(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Response, AppError> {
    let ev = st
        .db
        .get_event(&id)
        .map_err(AppError::internal)?
        .ok_or_else(|| AppError::not_found("event not found"))?;
    let path = ev
        .clip_path
        .ok_or_else(|| AppError::not_found("clip not available"))?;
    serve_stored(&st.cfg.clips_dir(), &path, &headers)
}

async fn recording_file(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> Result<Response, AppError> {
    let seg = st
        .db
        .get_segment(id)
        .map_err(AppError::internal)?
        .ok_or_else(|| AppError::not_found("recording not found"))?;
    serve_stored(&st.cfg.recordings_dir(), &seg.path, &headers)
}

fn serve_stored(root: &std::path::Path, stored: &str, headers: &HeaderMap) -> Result<Response, AppError> {
    let file = std::path::PathBuf::from(stored);
    let canon = file.canonicalize().map_err(|_| AppError::not_found("not found"))?;
    let root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    if !canon.starts_with(&root) {
        return Err(AppError::not_found("not found"));
    }
    let bytes = std::fs::read(&canon).map_err(AppError::internal)?;
    let ctype = if canon.extension().and_then(|e| e.to_str()) == Some("mp4") {
        "video/mp4"
    } else {
        "image/jpeg"
    };
    Ok(ranged_bytes(bytes, ctype, headers))
}

fn ranged_bytes(bytes: Vec<u8>, ctype: &'static str, headers: &HeaderMap) -> Response {
    let len = bytes.len() as u64;
    let range = headers
        .get(header::RANGE)
        .and_then(|v| v.to_str().ok())
        .and_then(|h| parse_byte_range(h, len));
    if let Some((start, end)) = range {
        let slice = bytes[start as usize..=end as usize].to_vec();
        return Response::builder()
            .status(StatusCode::PARTIAL_CONTENT)
            .header(header::CONTENT_TYPE, ctype)
            .header(header::ACCEPT_RANGES, "bytes")
            .header(
                header::CONTENT_RANGE,
                format!("bytes {start}-{end}/{len}"),
            )
            .header(header::CONTENT_LENGTH, slice.len())
            .body(Body::from(slice))
            .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response());
    }
    Response::builder()
        .header(header::CONTENT_TYPE, ctype)
        .header(header::ACCEPT_RANGES, "bytes")
        .header(header::CONTENT_LENGTH, bytes.len())
        .body(Body::from(bytes))
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

fn parse_byte_range(header: &str, len: u64) -> Option<(u64, u64)> {
    let rest = header.strip_prefix("bytes=")?;
    let (start, end) = rest.split_once('-')?;
    let start: u64 = start.parse().ok()?;
    if start >= len {
        return None;
    }
    let end = if end.is_empty() {
        len - 1
    } else {
        end.parse::<u64>().ok()?.min(len - 1)
    };
    if end < start {
        return None;
    }
    Some((start, end))
}

async fn recordings(
    State(st): State<Arc<AppState>>,
    Query(q): Query<ListQuery>,
) -> Result<Json<serde_json::Value>, AppError> {
    let limit = q.limit.unwrap_or(50).clamp(1, 200);
    let offset = q.offset.unwrap_or(0).max(0);
    let total = st
        .db
        .count_segments(q.camera.as_deref(), q.after, q.before)
        .map_err(AppError::internal)?;
    let rows = st
        .db
        .list_segments(q.camera.as_deref(), q.after, q.before, limit, offset)
        .map_err(AppError::internal)?;
    Ok(Json(serde_json::json!({
        "recordings": rows,
        "total": total,
        "offset": offset,
        "limit": limit
    })))
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

#[derive(Deserialize)]
struct StreamAuthQuery {
    camera: String,
    exp: i64,
    sig: String,
}

/// Nginx `auth_request` / go2rtc edge auth callback (signed URL, no Bearer).
async fn streams_auth(
    State(st): State<Arc<AppState>>,
    Query(q): Query<StreamAuthQuery>,
) -> Result<StatusCode, AppError> {
    if !st.cfg.stream.enabled {
        return Err(AppError::unavailable("stream sharing disabled"));
    }
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

#[derive(Debug)]
pub struct AppError {
    status: StatusCode,
    msg: String,
}

impl AppError {
    pub(crate) fn internal(e: impl ToString) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            msg: e.to_string(),
        }
    }
    pub(crate) fn not_found(msg: impl Into<String>) -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            msg: msg.into(),
        }
    }
    pub(crate) fn unauthorized(msg: impl Into<String>) -> Self {
        Self {
            status: StatusCode::UNAUTHORIZED,
            msg: msg.into(),
        }
    }
    pub(crate) fn forbidden(msg: impl Into<String>) -> Self {
        Self {
            status: StatusCode::FORBIDDEN,
            msg: msg.into(),
        }
    }
    pub(crate) fn unavailable(msg: impl Into<String>) -> Self {
        Self {
            status: StatusCode::SERVICE_UNAVAILABLE,
            msg: msg.into(),
        }
    }
    pub(crate) fn bad_request(msg: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            msg: msg.into(),
        }
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        (self.status, Json(serde_json::json!({ "error": self.msg }))).into_response()
    }
}
