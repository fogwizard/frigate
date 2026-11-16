//! WeChat miniprogram HTTP APIs matching examples/miniprogram contracts.

use crate::api::{AppError, AppState};
use crate::mp_db::MpSession;
use crate::stream_sign;
use axum::extract::{Path, State};
use axum::http::HeaderMap;
use axum::Json;
use chrono::Utc;
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::Arc;

pub async fn login(
    State(st): State<Arc<AppState>>,
    Json(body): Json<LoginBody>,
) -> Result<Json<Value>, AppError> {
    require_mp(&st)?;
    if body.code.trim().is_empty() {
        return Err(AppError::bad_request("missing code"));
    }
    let openid = resolve_openid(&st, &body.code).await?;
    let role = if st.cfg.miniprogram.is_admin(&openid) {
        "admin"
    } else {
        "user"
    };
    let token = st
        .db
        .create_session(&openid, role, st.cfg.miniprogram.session_ttl_secs)
        .map_err(AppError::internal)?;
    let (phone, wechat_id) = st.db.profile(&openid).map_err(AppError::internal)?;
    Ok(Json(json!({
        "openid": openid,
        "token": token,
        "role": role,
        "phone": phone,
        "wechatId": wechat_id,
    })))
}

#[derive(Deserialize)]
pub struct ProfileBody {
    #[serde(default)]
    pub phone: String,
    #[serde(default, rename = "wechatId")]
    pub wechat_id: String,
}

pub async fn profile_get(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<Json<Value>, AppError> {
    let session = session_from_headers(&st, &headers)?;
    let (phone, wechat_id) = st.db.profile(&session.openid).map_err(AppError::internal)?;
    Ok(Json(json!({ "phone": phone, "wechatId": wechat_id })))
}

pub async fn profile_save(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(body): Json<ProfileBody>,
) -> Result<Json<Value>, AppError> {
    let session = session_from_headers(&st, &headers)?;
    let phone = normalize_phone(&body.phone)?;
    let wechat_id = normalize_wechat(&body.wechat_id)?;
    if phone.is_empty() && wechat_id.is_empty() {
        return Err(AppError::bad_request("请填写手机号或微信号"));
    }
    let claimed = st
        .db
        .save_profile(&session.openid, &phone, &wechat_id)
        .map_err(AppError::internal)?;
    Ok(Json(json!({ "phone": phone, "wechatId": wechat_id, "claimed": claimed })))
}

#[derive(Deserialize)]
pub struct AssignBody {
    #[serde(default)]
    pub phone: String,
    #[serde(default, rename = "wechatId")]
    pub wechat_id: String,
    pub camera: String,
    #[serde(default)]
    pub number: String,
    #[serde(default)]
    pub label: String,
}

pub async fn boarding_assign(
    State(st): State<Arc<AppState>>,
    Json(body): Json<AssignBody>,
) -> Result<Json<Value>, AppError> {
    require_mp(&st)?;
    if !st.cfg.cameras.contains_key(&body.camera) {
        return Err(AppError::bad_request("未知镜头"));
    }
    let phone = normalize_phone(&body.phone)?;
    let wechat_id = normalize_wechat(&body.wechat_id)?;
    if phone.is_empty() && wechat_id.is_empty() {
        return Err(AppError::bad_request("请填写手机号或微信号"));
    }
    let number = if body.number.trim().is_empty() {
        body.camera.clone()
    } else {
        body.number.trim().to_string()
    };
    let label = if body.label.trim().is_empty() {
        format!("寄养 {number}")
    } else {
        body.label.trim().to_string()
    };
    let id = st
        .db
        .assign_boarding(&phone, &wechat_id, &body.camera, &number, &label)
        .map_err(AppError::internal)?;
    Ok(Json(json!({ "id": id, "phone": phone, "wechatId": wechat_id, "camera": body.camera, "number": number, "label": label })))
}

pub async fn boarding_assigned(State(st): State<Arc<AppState>>) -> Result<Json<Value>, AppError> {
    require_mp(&st)?;
    let rows = st.db.list_store_boarding().map_err(AppError::internal)?;
    let records: Vec<Value> = rows
        .into_iter()
        .map(|(id, phone, wechat_id, camera, number, label)| {
            json!({
                "id": id,
                "phone": phone,
                "wechatId": wechat_id,
                "camera": camera,
                "number": number,
                "label": label,
            })
        })
        .collect();
    Ok(Json(json!({ "records": records })))
}

fn normalize_phone(raw: &str) -> Result<String, AppError> {
    let digits: String = raw.chars().filter(|c| c.is_ascii_digit()).collect();
    let digits = digits
        .strip_prefix("86")
        .filter(|rest| rest.len() == 11)
        .unwrap_or(digits.as_str());
    if digits.is_empty() {
        return Ok(String::new());
    }
    if digits.len() != 11 || !digits.starts_with('1') {
        return Err(AppError::bad_request("手机号须为 11 位"));
    }
    Ok(digits.to_string())
}

fn normalize_wechat(raw: &str) -> Result<String, AppError> {
    let s = raw.trim();
    if s.is_empty() {
        return Ok(String::new());
    }
    if s.chars().count() > 32 || s.chars().any(char::is_whitespace) {
        return Err(AppError::bad_request("微信号不能含空格，且不超过 32 个字符"));
    }
    Ok(s.to_string())
}

#[derive(Deserialize)]
pub struct LoginBody {
    pub code: String,
}

async fn resolve_openid(st: &AppState, code: &str) -> Result<String, AppError> {
    let mp = &st.cfg.miniprogram;
    if mp.demo_mode || mp.wechat_app_secret.trim().is_empty() {
        return Ok(mp.demo_openid.clone());
    }
    let url = format!(
        "https://api.weixin.qq.com/sns/jscode2session?appid={}&secret={}&js_code={}&grant_type=authorization_code",
        urlencoding_lite(&mp.wechat_app_id),
        urlencoding_lite(&mp.wechat_app_secret),
        urlencoding_lite(code),
    );
    let resp: Value = reqwest::get(&url)
        .await
        .map_err(AppError::internal)?
        .json()
        .await
        .map_err(AppError::internal)?;
    if let Some(err) = resp.get("errcode").and_then(|v| v.as_i64()) {
        if err != 0 {
            return Err(AppError::unauthorized(format!(
                "wechat login failed: {}",
                resp.get("errmsg").and_then(|v| v.as_str()).unwrap_or("error")
            )));
        }
    }
    resp.get("openid")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| AppError::unauthorized("wechat login missing openid"))
}

fn urlencoding_lite(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

pub fn require_mp(st: &AppState) -> Result<(), AppError> {
    if !st.cfg.miniprogram.enabled {
        return Err(AppError::unavailable("miniprogram API disabled"));
    }
    Ok(())
}

pub fn session_from_headers(st: &AppState, headers: &HeaderMap) -> Result<MpSession, AppError> {
    require_mp(st)?;
    let auth = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let token = auth
        .strip_prefix("Bearer ")
        .or_else(|| auth.strip_prefix("bearer "))
        .unwrap_or("")
        .trim();
    if token.is_empty() {
        return Err(AppError::unauthorized("missing Bearer token"));
    }
    st.db
        .get_session(token)
        .map_err(AppError::internal)?
        .ok_or_else(|| AppError::unauthorized("invalid or expired session"))
}

pub async fn streams_list(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<Json<Value>, AppError> {
    let session = session_from_headers(&st, &headers)?;
    if session.role != "admin" {
        return Err(AppError::forbidden("admin only"));
    }
    let mut cams: Vec<_> = st.cfg.cameras.keys().cloned().collect();
    cams.sort();
    let cameras: Vec<Value> = cams
        .into_iter()
        .enumerate()
        .map(|(i, camera)| {
            let number = format!("{:02}", i + 1);
            let can_control = st.cfg.miniprogram.can_control(&camera);
            json!({
                "camera": camera,
                "number": number,
                "label": format!("镜头 {number}"),
                "canControl": can_control,
            })
        })
        .collect();
    Ok(Json(json!({ "cameras": cameras })))
}

pub async fn streams_play(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(camera): Path<String>,
) -> Result<Json<Value>, AppError> {
    let session = session_from_headers(&st, &headers)?;
    if !st.cfg.cameras.contains_key(&camera) {
        return Err(AppError::not_found("camera not found"));
    }
    let allowed = if session.role == "admin" {
        true
    } else {
        st.db
            .has_active_boarding(&session.openid, &camera)
            .map_err(AppError::internal)?
    };
    if !allowed {
        return Err(AppError::forbidden("no access to this camera"));
    }
    if !st.cfg.stream.enabled {
        return Err(AppError::unavailable("stream sharing disabled"));
    }
    let play = stream_sign::issue_play_url(&st.cfg.stream, &camera, Utc::now());
    Ok(Json(json!({
        "camera": camera,
        "url": play.url,
        "expires_at": play.expires_at,
    })))
}

pub async fn boarding_records(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<Json<Value>, AppError> {
    let session = session_from_headers(&st, &headers)?;
    let records = st
        .db
        .list_boarding_records(&session.openid)
        .map_err(AppError::internal)?;
    let records: Vec<Value> = records
        .into_iter()
        .map(|r| {
            json!({
                "id": r.id,
                "camera": r.camera,
                "number": r.number,
                "label": r.label,
                "status": r.status,
                "paymentStatus": r.payment_status,
            })
        })
        .collect();
    Ok(Json(json!({ "records": records })))
}

#[derive(Deserialize)]
pub struct QrBody {
    #[serde(rename = "qrToken")]
    pub qr_token: String,
}

pub async fn boarding_cages(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(body): Json<QrBody>,
) -> Result<Json<Value>, AppError> {
    let _session = session_from_headers(&st, &headers)?;
    let Some((store_id, store_name)) = st
        .db
        .resolve_boarding_qr(&body.qr_token)
        .map_err(AppError::internal)?
    else {
        return Err(AppError::forbidden("invalid or expired boarding QR"));
    };
    let cages = st
        .db
        .list_cages(&store_id)
        .map_err(AppError::internal)?;
    let cages: Vec<Value> = cages
        .into_iter()
        .map(|c| {
            json!({
                "id": c.id,
                "number": c.number,
                "amount": c.amount,
                "available": c.available,
            })
        })
        .collect();
    Ok(Json(json!({ "storeName": store_name, "cages": cages })))
}

#[derive(Deserialize)]
pub struct CreateOrderBody {
    #[serde(rename = "qrToken")]
    pub qr_token: String,
    #[serde(rename = "cageId")]
    pub cage_id: String,
    #[serde(rename = "requestId")]
    pub request_id: String,
}

pub async fn boarding_create_order(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(body): Json<CreateOrderBody>,
) -> Result<Json<Value>, AppError> {
    let session = session_from_headers(&st, &headers)?;
    let Some((store_id, _)) = st
        .db
        .resolve_boarding_qr(&body.qr_token)
        .map_err(AppError::internal)?
    else {
        return Err(AppError::forbidden("invalid or expired boarding QR"));
    };
    let Some((cage, cage_store, label)) = st
        .db
        .get_cage(&body.cage_id)
        .map_err(AppError::internal)?
    else {
        return Err(AppError::not_found("cage not found"));
    };
    if cage_store != store_id {
        return Err(AppError::forbidden("cage does not belong to this store"));
    }
    if !cage.available && st.db.find_order_by_request(&session.openid, &body.request_id)
        .map_err(AppError::internal)?
        .is_none()
    {
        return Err(AppError::forbidden("cage unavailable"));
    }
    let demo_paid = st.cfg.miniprogram.demo_mode;
    let order = st
        .db
        .create_boarding_order(
            &session.openid,
            &body.request_id,
            &store_id,
            &cage,
            &label,
            demo_paid,
        )
        .map_err(AppError::internal)?;
    Ok(Json(order_json(&order, demo_paid)))
}

pub async fn boarding_get_order(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(order_id): Path<String>,
) -> Result<Json<Value>, AppError> {
    let session = session_from_headers(&st, &headers)?;
    let Some(order) = st.db.get_order(&order_id).map_err(AppError::internal)? else {
        return Err(AppError::not_found("order not found"));
    };
    if order.openid != session.openid && session.role != "admin" {
        return Err(AppError::forbidden("order access denied"));
    }
    Ok(Json(order_json(&order, st.cfg.miniprogram.demo_mode)))
}

fn order_json(order: &crate::mp_db::OrderRow, include_mock_payment: bool) -> Value {
    let mut v = json!({
        "orderId": order.id,
        "status": order.status,
        "paymentStatus": order.payment_status,
        "amount": order.amount,
        "camera": order.camera,
    });
    if let Some(ref rid) = order.record_id {
        v["recordId"] = json!(rid);
    }
    if order.payment_status == "pending" && include_mock_payment {
        // Placeholder so client validation passes; real WeChat pay replaces this in production.
        let ts = Utc::now().timestamp().to_string();
        v["payment"] = json!({
            "timeStamp": ts,
            "nonceStr": "demo_nonce",
            "package": "prepay_id=demo_prepay",
            "signType": "RSA",
            "paySign": "demo_sign",
        });
    }
    v
}

#[derive(Deserialize)]
pub struct CameraBody {
    pub camera: String,
}

pub async fn feeding(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(body): Json<CameraBody>,
) -> Result<Json<Value>, AppError> {
    authorize_device(&st, &headers, &body.camera, "feed")?;
    if st
        .cfg
        .cameras
        .get(&body.camera)
        .is_some_and(|c| c.feed_time == 0)
    {
        return Err(AppError::bad_request("feeding disabled"));
    }
    let feed = st.cfg.feed.clone();
    let slave = st
        .cfg
        .cameras
        .get(&body.camera)
        .and_then(|c| c.feed_slave);
    if feed.serial_enabled() && slave.is_none() {
        return Err(AppError::bad_request(
            "feeder slave not configured for camera",
        ));
    }
    let slave = slave.unwrap_or(0);
    let open_for = st
        .cfg
        .cameras
        .get(&body.camera)
        .map(|c| c.feed_time)
        .unwrap_or(0);
    let close_later = feed.clone();
    tokio::task::spawn_blocking(move || {
        vigil_modbus::write_feed(
            &feed.port,
            feed.baud,
            slave,
            feed.function,
            feed.address,
            feed.value,
        )
    })
    .await
    .map_err(AppError::internal)?
    .map_err(AppError::internal)?;
    if close_later.serial_enabled() {
        let camera = body.camera.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_secs(open_for)).await;
            let closed = tokio::task::spawn_blocking(move || {
                vigil_modbus::write_feed(
                    &close_later.port,
                    close_later.baud,
                    slave,
                    close_later.function,
                    close_later.address,
                    close_later.close_value,
                )
            })
            .await;
            match closed {
                Ok(Ok(())) => tracing::info!(%camera, "manual feeder closed"),
                Ok(Err(e)) => tracing::warn!(%camera, error = %e, "manual feeder close failed"),
                Err(e) => tracing::warn!(%camera, error = %e, "manual feeder close failed"),
            }
        });
    }
    tracing::info!(camera = %body.camera, slave, "feed command finished");
    Ok(Json(json!({ "success": true, "status": "accepted" })))
}

pub async fn watering(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(body): Json<CameraBody>,
) -> Result<Json<Value>, AppError> {
    device_action(&st, &headers, &body.camera, "water").await
}

fn authorize_device(
    st: &AppState,
    headers: &HeaderMap,
    camera: &str,
    kind: &str,
) -> Result<(), AppError> {
    let session = session_from_headers(st, headers)?;
    if !st.cfg.cameras.contains_key(camera) {
        return Err(AppError::not_found("camera not found"));
    }
    let ok = if session.role == "admin" {
        st.cfg.miniprogram.can_control(camera)
    } else {
        st.db
            .has_active_boarding(&session.openid, camera)
            .map_err(AppError::internal)?
    };
    if !ok {
        return Err(AppError::forbidden(format!("no {kind} permission")));
    }
    tracing::info!(openid = %session.openid, %camera, %kind, "miniprogram device command accepted");
    Ok(())
}

async fn device_action(
    st: &AppState,
    headers: &HeaderMap,
    camera: &str,
    kind: &str,
) -> Result<Json<Value>, AppError> {
    authorize_device(st, headers, camera, kind)?;
    Ok(Json(json!({ "success": true, "status": "accepted" })))
}

pub async fn door_open(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(body): Json<QrBody>,
) -> Result<Json<Value>, AppError> {
    let session = session_from_headers(&st, &headers)?;
    let ok = st
        .db
        .consume_door_token(&body.qr_token)
        .map_err(AppError::internal)?;
    if !ok {
        return Err(AppError::forbidden("invalid, expired, or used door QR"));
    }
    // Demo: treat as opened. Production would call lock hardware here.
    tracing::info!(openid = %session.openid, "door open accepted");
    let status = if st.cfg.miniprogram.demo_mode {
        "opened"
    } else {
        "accepted"
    };
    Ok(Json(json!({ "success": true, "status": status })))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Db;
    use crate::detect::new_status_map;
    use axum::http::{header::AUTHORIZATION, HeaderValue, StatusCode};
    use axum::response::IntoResponse;
    use std::collections::HashMap;
    use std::time::Instant;
    use vigil_common::{
        CameraConfig, CameraFfmpeg, CameraInput, Config, MiniprogramConfig, PathsConfig,
    };

    fn cam(name: &str) -> (String, CameraConfig) {
        (
            name.into(),
            CameraConfig {
                ffmpeg: CameraFfmpeg {
                    inputs: vec![CameraInput {
                        path: format!("rtsp://127.0.0.1/{name}"),
                        roles: vec!["record".into(), "detect".into()],
                    }],
                },
                feed_slave: name.strip_prefix("cam").and_then(|n| n.parse().ok()),
                feed_time: 8,
            },
        )
    }

    fn setup(control_cameras: Vec<String>) -> (Arc<AppState>, String, String) {
        let dir = std::env::temp_dir().join(format!("vigil-mp-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let db_path = dir.join("t.db");

        let mut cfg: Config = serde_yaml::from_str("{}").unwrap();
        cfg.cameras = HashMap::from([cam("cam1"), cam("cam2")]);
        cfg.paths = PathsConfig {
            data_dir: dir,
            db_path: db_path.clone(),
        };
        cfg.miniprogram = MiniprogramConfig {
            enabled: true,
            demo_mode: true,
            demo_openid: "demo-admin".into(),
            admin_openids: vec!["demo-admin".into()],
            control_cameras,
            ..Default::default()
        };

        let db = Db::open(&db_path).unwrap();
        db.migrate_miniprogram().unwrap();
        let names: Vec<String> = cfg.cameras.keys().cloned().collect();
        db.seed_miniprogram_demo(&names).unwrap();

        let st = Arc::new(AppState {
            cfg: Arc::new(cfg),
            db: Arc::new(db),
            started: Instant::now(),
            infer_connected: Arc::new(tokio::sync::RwLock::new(false)),
            statuses: new_status_map(&names),
            version: "test".into(),
        });
        let admin = st
            .db
            .create_session("demo-admin", "admin", 3600)
            .unwrap();
        let user = st.db.create_session("demo-user", "user", 3600).unwrap();
        (st, admin, user)
    }

    fn auth(token: &str) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {token}")).unwrap(),
        );
        h
    }

    fn err_status(err: AppError) -> StatusCode {
        err.into_response().status()
    }

    #[tokio::test]
    async fn feed_cam1_calls_write_feed_with_its_slave() {
        let (st, admin, _) = setup(vec!["cam1".into()]);
        assert_eq!(st.cfg.cameras["cam1"].feed_slave, Some(1));
        let resp = feeding(
            State(st),
            auth(&admin),
            Json(CameraBody {
                camera: "cam1".into(),
            }),
        )
        .await
        .unwrap();
        assert_eq!(resp.0["success"], true);
        assert_eq!(vigil_modbus::last_slave(), 1);
    }

    #[tokio::test]
    async fn feed_and_water_admin_ok_when_control_allowed() {
        let (st, admin, _) = setup(vec!["cam1".into()]);
        let feed = feeding(
            State(st.clone()),
            auth(&admin),
            Json(CameraBody {
                camera: "cam1".into(),
            }),
        )
        .await
        .unwrap();
        assert_eq!(feed.0["success"], true);
        assert_eq!(feed.0["status"], "accepted");

        let water = watering(
            State(st.clone()),
            auth(&admin),
            Json(CameraBody {
                camera: "cam1".into(),
            }),
        )
        .await
        .unwrap();
        assert_eq!(water.0["success"], true);

        let denied = feeding(
            State(st),
            auth(&admin),
            Json(CameraBody {
                camera: "cam2".into(),
            }),
        )
        .await
        .unwrap_err();
        assert_eq!(err_status(denied), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn feed_user_requires_active_boarding() {
        let (st, _, user) = setup(vec![]);
        let denied = feeding(
            State(st.clone()),
            auth(&user),
            Json(CameraBody {
                camera: "cam1".into(),
            }),
        )
        .await
        .unwrap_err();
        assert_eq!(err_status(denied), StatusCode::FORBIDDEN);

        // Activate boarding via demo order
        let cages = st.db.list_cages("store_demo").unwrap();
        let cage = cages.iter().find(|c| c.camera == "cam1").unwrap();
        let label = format!("寄养房 {}", cage.camera);
        st.db
            .create_boarding_order(
                "demo-user",
                "req-feed-1",
                "store_demo",
                cage,
                &label,
                true,
            )
            .unwrap();

        let ok = feeding(
            State(st),
            auth(&user),
            Json(CameraBody {
                camera: "cam1".into(),
            }),
        )
        .await
        .unwrap();
        assert_eq!(ok.0["success"], true);
    }

    #[tokio::test]
    async fn feed_unknown_camera_not_found() {
        let (st, admin, _) = setup(vec![]);
        let err = feeding(
            State(st),
            auth(&admin),
            Json(CameraBody {
                camera: "nope".into(),
            }),
        )
        .await
        .unwrap_err();
        assert_eq!(err_status(err), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn feed_rejects_missing_bearer() {
        let (st, _, _) = setup(vec![]);
        let err = feeding(
            State(st),
            HeaderMap::new(),
            Json(CameraBody {
                camera: "cam1".into(),
            }),
        )
        .await
        .unwrap_err();
        assert_eq!(err_status(err), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn door_open_once_then_reject_replay() {
        let (st, _, user) = setup(vec![]);
        let first = door_open(
            State(st.clone()),
            auth(&user),
            Json(QrBody {
                qr_token: "door_challenge_demo123456".into(),
            }),
        )
        .await
        .unwrap();
        assert_eq!(first.0["success"], true);
        assert_eq!(first.0["status"], "opened");

        let replay = door_open(
            State(st.clone()),
            auth(&user),
            Json(QrBody {
                qr_token: "door_challenge_demo123456".into(),
            }),
        )
        .await
        .unwrap_err();
        assert_eq!(err_status(replay), StatusCode::FORBIDDEN);

        let bad = door_open(
            State(st),
            auth(&user),
            Json(QrBody {
                qr_token: "door_challenge_unknown99".into(),
            }),
        )
        .await
        .unwrap_err();
        assert_eq!(err_status(bad), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn store_assigns_boarding_by_phone_and_the_user_claims_it() {
        let (st, _, user) = setup(vec![]);
        let assigned = boarding_assign(
            State(st.clone()),
            Json(AssignBody {
                phone: "13800138000".into(),
                wechat_id: "shop_cat".into(),
                camera: "cam1".into(),
                number: "01".into(),
                label: "寄养房 01".into(),
            }),
        )
        .await
        .unwrap();
        assert_eq!(assigned.0["camera"], "cam1");
        let before = boarding_records(State(st.clone()), auth(&user)).await.unwrap();
        assert_eq!(before.0["records"].as_array().unwrap().len(), 0);
        let saved = profile_save(
            State(st.clone()),
            auth(&user),
            Json(ProfileBody {
                phone: "138-0013-8000".into(),
                wechat_id: String::new(),
            }),
        )
        .await
        .unwrap();
        assert_eq!(saved.0["phone"], "13800138000");
        assert_eq!(saved.0["claimed"].as_u64(), Some(1));
        let after = boarding_records(State(st), auth(&user)).await.unwrap();
        assert_eq!(after.0["records"][0]["camera"], "cam1");
    }

    #[tokio::test]
    async fn door_open_requires_login() {
        let (st, _, _) = setup(vec![]);
        let err = door_open(
            State(st),
            HeaderMap::new(),
            Json(QrBody {
                qr_token: "door_challenge_demo123456".into(),
            }),
        )
        .await
        .unwrap_err();
        assert_eq!(err_status(err), StatusCode::UNAUTHORIZED);
    }
}
