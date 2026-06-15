use axum::{
    extract::{Query, State, WebSocketUpgrade},
    http::{HeaderMap, StatusCode},
    response::{sse, Sse},
    Json,
};
use chrono::{Duration, Utc};
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use std::convert::Infallible;
use std::sync::Arc;
use tokio_stream::wrappers::BroadcastStream;
use tracing::{error, info};
use uuid::Uuid;

use crate::auth::AuthService;
use crate::db::Database;
use crate::models::*;
use crate::pubsub::PubSubRelay;

pub struct AppState {
    pub db: Database,
    pub auth: AuthService,
    pub pubsub: Arc<PubSubRelay>,
    pub refresh_ttl_days: i64,
    pub jwt_ttl_hours: i64,
}

// POST /api/v1/clips
pub async fn upload_clip(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(upload): Json<ClipUpload>,
) -> Result<(StatusCode, Json<ClipResponse>), (StatusCode, Json<ErrorResponse>)> {
    let claims = extract_claims(&state.auth, &headers)?;
    let user_id: Uuid = claims
        .user_id
        .parse()
        .map_err(|_| error_response(StatusCode::BAD_REQUEST, "invalid_user_id", "Invalid user ID"))?;

    let clip = Clip {
        id: upload.clip_id,
        user_id,
        device_id: upload.device_id,
        content: upload.content.clone(),
        format: upload.format.clone(),
        e2e: upload.e2e,
        created_at: upload.timestamp,
    };

    let inserted = state.db.insert_clip(&clip).await.map_err(|e| {
        error!("DB error: {e}");
        error_response(StatusCode::INTERNAL_SERVER_ERROR, "db_error", "Database error")
    })?;

    if !inserted {
        return Err(error_response(
            StatusCode::CONFLICT,
            "duplicate",
            "Clip already exists",
        ));
    }

    let audit = AuditLog {
        id: Uuid::new_v4(),
        user_id,
        device_id: Some(upload.device_id),
        action: "clip_upload".into(),
        details: serde_json::json!({ "clip_id": clip.id }),
        timestamp: Utc::now(),
    };
    let _ = state.db.insert_audit_log(&audit).await;

    let pub_msg = PubSubClipMessage {
        msg_type: "clip".into(),
        user_id,
        clip_id: clip.id,
        device_id: clip.device_id,
        timestamp: clip.created_at,
        content: upload.content,
        e2e: upload.e2e,
    };
    let _ = state.pubsub.publish(pub_msg).await;

    info!(clip_id = %clip.id, "Clip uploaded and published");

    Ok((
        StatusCode::CREATED,
        Json(ClipResponse {
            status: "ok".into(),
            clip_id: clip.id,
        }),
    ))
}

// GET /api/v1/clips
pub async fn get_clips(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(params): Query<ClipQueryParams>,
) -> Result<Json<Vec<Clip>>, (StatusCode, Json<ErrorResponse>)> {
    let claims = extract_claims(&state.auth, &headers)?;
    let user_id: Uuid = claims.user_id.parse().map_err(|_| {
        error_response(StatusCode::BAD_REQUEST, "invalid_user_id", "Invalid user ID")
    })?;

    let limit = params.limit.unwrap_or(50).min(200);
    let clips = state.db.get_clips(user_id, limit).await.map_err(|e| {
        error!("DB error: {e}");
        error_response(StatusCode::INTERNAL_SERVER_ERROR, "db_error", "Database error")
    })?;

    Ok(Json(clips))
}

#[derive(Debug, Deserialize)]
pub struct ClipQueryParams {
    pub limit: Option<i64>,
}

// POST /api/v1/pair/request
pub async fn pair_request(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<Json<PairTokenResponse>, (StatusCode, Json<ErrorResponse>)> {
    let claims = extract_claims(&state.auth, &headers)?;
    let user_id: Uuid = claims.user_id.parse().map_err(|_| {
        error_response(StatusCode::BAD_REQUEST, "invalid_user_id", "Invalid user ID")
    })?;

    let token = AuthService::generate_pairing_token();
    let expires_at = Utc::now() + Duration::minutes(10);

    state
        .db
        .create_pairing_token(user_id, &token, expires_at)
        .await
        .map_err(|e| {
            error!("DB error: {e}");
            error_response(StatusCode::INTERNAL_SERVER_ERROR, "db_error", "Database error")
        })?;

    let audit = AuditLog {
        id: Uuid::new_v4(),
        user_id,
        device_id: None,
        action: "pair_request".into(),
        details: serde_json::json!({}),
        timestamp: Utc::now(),
    };
    let _ = state.db.insert_audit_log(&audit).await;

    Ok(Json(PairTokenResponse {
        token,
        expires_at,
    }))
}

// POST /api/v1/pair/confirm
pub async fn pair_confirm(
    State(state): State<Arc<AppState>>,
    Json(body): Json<PairConfirm>,
) -> Result<Json<PairResponse>, (StatusCode, Json<ErrorResponse>)> {
    let user_id = state
        .db
        .consume_pairing_token(&body.token)
        .await
        .map_err(|e| {
            error!("DB error: {e}");
            error_response(StatusCode::INTERNAL_SERVER_ERROR, "db_error", "Database error")
        })?
        .ok_or_else(|| {
            error_response(
                StatusCode::UNAUTHORIZED,
                "invalid_token",
                "Pairing token is invalid or expired",
            )
        })?;

    let device = state
        .db
        .create_device(user_id, &body.device_name, body.public_key.as_deref())
        .await
        .map_err(|e| {
            error!("DB error: {e}");
            error_response(StatusCode::INTERNAL_SERVER_ERROR, "db_error", "Database error")
        })?;

    let jwt = state.auth.create_jwt(user_id, device.id).map_err(|e| {
        error!("JWT error: {e}");
        error_response(StatusCode::INTERNAL_SERVER_ERROR, "auth_error", "Failed to create JWT")
    })?;
    let (refresh_token, refresh_expires_at) = state
        .db
        .create_refresh_token(user_id, device.id, state.refresh_ttl_days)
        .await
        .map_err(|e| {
            error!("DB error: {e}");
            error_response(StatusCode::INTERNAL_SERVER_ERROR, "db_error", "Database error")
        })?;
    let jwt_expires_at = Utc::now() + Duration::hours(state.jwt_ttl_hours);

    let audit = AuditLog {
        id: Uuid::new_v4(),
        user_id,
        device_id: Some(device.id),
        action: "device_paired".into(),
        details: serde_json::json!({ "device_name": body.device_name }),
        timestamp: Utc::now(),
    };
    let _ = state.db.insert_audit_log(&audit).await;

    info!(device_id = %device.id, "Device paired");

    Ok(Json(PairResponse {
        user_id,
        device_id: device.id,
        device_token: AuthService::generate_pairing_token(),
        jwt,
        refresh_token,
        jwt_expires_at,
        refresh_expires_at,
    }))
}

// GET /api/v1/devices
pub async fn get_devices(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<Json<Vec<Device>>, (StatusCode, Json<ErrorResponse>)> {
    let claims = extract_claims(&state.auth, &headers)?;
    let user_id: Uuid = claims.user_id.parse().map_err(|_| {
        error_response(StatusCode::BAD_REQUEST, "invalid_user_id", "Invalid user ID")
    })?;

    let devices = state.db.get_devices(user_id).await.map_err(|e| {
        error!("DB error: {e}");
        error_response(StatusCode::INTERNAL_SERVER_ERROR, "db_error", "Database error")
    })?;

    Ok(Json(devices))
}

// DELETE /api/v1/devices/:id
pub async fn revoke_device(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    axum::extract::Path(device_id): axum::extract::Path<Uuid>,
) -> Result<StatusCode, (StatusCode, Json<ErrorResponse>)> {
    let claims = extract_claims(&state.auth, &headers)?;
    let user_id: Uuid = claims.user_id.parse().map_err(|_| {
        error_response(StatusCode::BAD_REQUEST, "invalid_user_id", "Invalid user ID")
    })?;

    let device = state.db.get_device(device_id).await.map_err(|e| {
        error!("DB error: {e}");
        error_response(StatusCode::INTERNAL_SERVER_ERROR, "db_error", "Database error")
    })?;

    match device {
        Some(d) if d.user_id == user_id => {
            state.db.revoke_device(device_id).await.map_err(|e| {
                error!("DB error: {e}");
                error_response(StatusCode::INTERNAL_SERVER_ERROR, "db_error", "Database error")
            })?;
            let _ = state.db.revoke_refresh_tokens_for_device(device_id).await;

            let audit = AuditLog {
                id: Uuid::new_v4(),
                user_id,
                device_id: Some(device_id),
                action: "device_revoked".into(),
                details: serde_json::json!({}),
                timestamp: Utc::now(),
            };
            let _ = state.db.insert_audit_log(&audit).await;

            info!(device_id = %device_id, "Device revoked");
            Ok(StatusCode::NO_CONTENT)
        }
        Some(_) => Err(error_response(
            StatusCode::FORBIDDEN,
            "forbidden",
            "Device belongs to another user",
        )),
        None => Err(error_response(
            StatusCode::NOT_FOUND,
            "not_found",
            "Device not found",
        )),
    }
}

// GET /api/v1/audit-logs
pub async fn get_audit_logs(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(params): Query<AuditQueryParams>,
) -> Result<Json<Vec<AuditLog>>, (StatusCode, Json<ErrorResponse>)> {
    let claims = extract_claims(&state.auth, &headers)?;
    let user_id: Uuid = claims.user_id.parse().map_err(|_| {
        error_response(StatusCode::BAD_REQUEST, "invalid_user_id", "Invalid user ID")
    })?;

    let limit = params.limit.unwrap_or(100).min(500);
    let logs = state.db.get_audit_logs(user_id, limit).await.map_err(|e| {
        error!("DB error: {e}");
        error_response(StatusCode::INTERNAL_SERVER_ERROR, "db_error", "Database error")
    })?;

    Ok(Json(logs))
}

#[derive(Debug, Deserialize)]
pub struct AuditQueryParams {
    pub limit: Option<i64>,
}

// WebSocket /api/v1/ws
pub async fn ws_handler(
    State(state): State<Arc<AppState>>,
    Query(params): Query<TokenQuery>,
    ws: WebSocketUpgrade,
) -> Result<axum::response::Response, (StatusCode, Json<ErrorResponse>)> {
    let claims = state.auth.validate_jwt(&params.token).map_err(|_| {
        error_response(StatusCode::UNAUTHORIZED, "invalid_token", "Invalid JWT")
    })?;

    let user_id: Uuid = claims.user_id.parse().map_err(|_| {
        error_response(StatusCode::BAD_REQUEST, "invalid_user_id", "Invalid user ID")
    })?;

    let device_id: Uuid = claims.device_id.parse().map_err(|_| {
        error_response(StatusCode::BAD_REQUEST, "invalid_device_id", "Invalid device ID")
    })?;

    // Check device not revoked
    if let Ok(Some(device)) = state.db.get_device(device_id).await {
        if device.revoked {
            return Err(error_response(
                StatusCode::FORBIDDEN,
                "device_revoked",
                "Device has been revoked",
            ));
        }
    }

    let pubsub = state.pubsub.clone();

    Ok(ws.on_upgrade(move |socket| async move {
        handle_ws(socket, pubsub, user_id, device_id).await;
    }))
}

async fn handle_ws(
    socket: axum::extract::ws::WebSocket,
    pubsub: Arc<PubSubRelay>,
    user_id: Uuid,
    device_id: Uuid,
) {
    let (mut ws_tx, mut ws_rx) = socket.split();
    let mut rx = pubsub.subscribe(user_id).await;

    info!(user_id = %user_id, device_id = %device_id, "WebSocket client connected");

    let send_task = tokio::spawn(async move {
        while let Ok(msg) = rx.recv().await {
            if msg.device_id == device_id {
                continue; // don't echo back to sender
            }
            let ws_msg = WsMessage {
                msg_type: "clip".into(),
                payload: serde_json::to_value(&msg).unwrap_or_default(),
            };
            let text = serde_json::to_string(&ws_msg).unwrap_or_default();
            if ws_tx
                .send(axum::extract::ws::Message::Text(text.into()))
                .await
                .is_err()
            {
                break;
            }
        }
    });

    let recv_task = tokio::spawn(async move {
        while let Some(Ok(msg)) = ws_rx.next().await {
            match msg {
                axum::extract::ws::Message::Ping(_) | axum::extract::ws::Message::Pong(_) => {}
                axum::extract::ws::Message::Close(_) => break,
                _ => {}
            }
        }
    });

    tokio::select! {
        _ = send_task => {},
        _ = recv_task => {},
    }

    info!(user_id = %user_id, device_id = %device_id, "WebSocket client disconnected");
}

// SSE /api/v1/sse
pub async fn sse_handler(
    State(state): State<Arc<AppState>>,
    Query(params): Query<TokenQuery>,
) -> Result<Sse<impl futures_util::Stream<Item = Result<sse::Event, Infallible>>>, (StatusCode, Json<ErrorResponse>)>
{
    let claims = state.auth.validate_jwt(&params.token).map_err(|_| {
        error_response(StatusCode::UNAUTHORIZED, "invalid_token", "Invalid JWT")
    })?;

    let user_id: Uuid = claims.user_id.parse().map_err(|_| {
        error_response(StatusCode::BAD_REQUEST, "invalid_user_id", "Invalid user ID")
    })?;

    let rx = state.pubsub.subscribe(user_id).await;

    info!(user_id = %user_id, "SSE client connected");

    let stream = BroadcastStream::new(rx).filter_map(|result| async {
        match result {
            Ok(msg) => {
                let data = serde_json::to_string(&msg).unwrap_or_default();
                Some(Ok(sse::Event::default().event("clip").data(data)))
            }
            Err(_) => None,
        }
    });

    Ok(Sse::new(stream).keep_alive(sse::KeepAlive::default()))
}

#[derive(Debug, Deserialize)]
pub struct TokenQuery {
    pub token: String,
}

// Bootstrap: create initial user and get JWT
// POST /api/v1/bootstrap
pub async fn bootstrap(
    State(state): State<Arc<AppState>>,
    Json(body): Json<BootstrapRequest>,
) -> Result<Json<BootstrapResponse>, (StatusCode, Json<ErrorResponse>)> {
    let user = state.db.create_user(&body.email).await.map_err(|e| {
        error!("DB error: {e}");
        error_response(StatusCode::INTERNAL_SERVER_ERROR, "db_error", "Database error")
    })?;

    let device = state
        .db
        .create_device(user.id, &body.device_name, body.public_key.as_deref())
        .await
        .map_err(|e| {
            error!("DB error: {e}");
            error_response(StatusCode::INTERNAL_SERVER_ERROR, "db_error", "Database error")
        })?;

    let jwt = state.auth.create_jwt(user.id, device.id).map_err(|e| {
        error!("JWT error: {e}");
        error_response(StatusCode::INTERNAL_SERVER_ERROR, "auth_error", "Auth error")
    })?;
    let (refresh_token, refresh_expires_at) = state
        .db
        .create_refresh_token(user.id, device.id, state.refresh_ttl_days)
        .await
        .map_err(|e| {
            error!("DB error: {e}");
            error_response(StatusCode::INTERNAL_SERVER_ERROR, "db_error", "Database error")
        })?;
    let jwt_expires_at = Utc::now() + Duration::hours(state.jwt_ttl_hours);

    Ok(Json(BootstrapResponse {
        user_id: user.id,
        device_id: device.id,
        jwt,
        refresh_token,
        jwt_expires_at,
        refresh_expires_at,
    }))
}

#[derive(Debug, Deserialize)]
pub struct BootstrapRequest {
    pub email: String,
    pub device_name: String,
    pub public_key: Option<String>,
}

#[derive(Debug, serde::Serialize)]
pub struct BootstrapResponse {
    pub user_id: Uuid,
    pub device_id: Uuid,
    pub jwt: String,
    pub refresh_token: String,
    pub jwt_expires_at: chrono::DateTime<Utc>,
    pub refresh_expires_at: chrono::DateTime<Utc>,
}

#[derive(Debug, Deserialize)]
pub struct RefreshRequest {
    pub refresh_token: String,
}

#[derive(Debug, serde::Serialize)]
pub struct RefreshResponse {
    pub jwt: String,
    pub refresh_token: String,
    pub jwt_expires_at: chrono::DateTime<Utc>,
    pub refresh_expires_at: chrono::DateTime<Utc>,
}

// POST /api/v1/auth/refresh
pub async fn refresh_token(
    State(state): State<Arc<AppState>>,
    Json(body): Json<RefreshRequest>,
) -> Result<Json<RefreshResponse>, (StatusCode, Json<ErrorResponse>)> {
    let pair = state
        .db
        .consume_refresh_token(&body.refresh_token)
        .await
        .map_err(|e| {
            error!("DB error: {e}");
            error_response(StatusCode::INTERNAL_SERVER_ERROR, "db_error", "Database error")
        })?
        .ok_or_else(|| {
            error_response(
                StatusCode::UNAUTHORIZED,
                "invalid_refresh",
                "Refresh token is invalid, expired, or already used",
            )
        })?;
    let (user_id, device_id) = pair;

    // Make sure the device hasn't been revoked.
    if let Ok(Some(dev)) = state.db.get_device(device_id).await {
        if dev.revoked {
            return Err(error_response(
                StatusCode::FORBIDDEN,
                "device_revoked",
                "Device has been revoked",
            ));
        }
    }

    let jwt = state.auth.create_jwt(user_id, device_id).map_err(|e| {
        error!("JWT error: {e}");
        error_response(StatusCode::INTERNAL_SERVER_ERROR, "auth_error", "Failed to create JWT")
    })?;
    let (new_refresh, refresh_expires_at) = state
        .db
        .create_refresh_token(user_id, device_id, state.refresh_ttl_days)
        .await
        .map_err(|e| {
            error!("DB error: {e}");
            error_response(StatusCode::INTERNAL_SERVER_ERROR, "db_error", "Database error")
        })?;
    let jwt_expires_at = Utc::now() + Duration::hours(state.jwt_ttl_hours);

    Ok(Json(RefreshResponse {
        jwt,
        refresh_token: new_refresh,
        jwt_expires_at,
        refresh_expires_at,
    }))
}

// Helpers
fn extract_claims(
    auth: &AuthService,
    headers: &HeaderMap,
) -> Result<Claims, (StatusCode, Json<ErrorResponse>)> {
    let auth_header = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| {
            error_response(
                StatusCode::UNAUTHORIZED,
                "missing_auth",
                "Missing Authorization header",
            )
        })?;

    let token = auth_header
        .strip_prefix("Bearer ")
        .ok_or_else(|| {
            error_response(
                StatusCode::UNAUTHORIZED,
                "invalid_auth",
                "Invalid Authorization format",
            )
        })?;

    auth.validate_jwt(token).map_err(|_| {
        error_response(
            StatusCode::UNAUTHORIZED,
            "invalid_token",
            "Invalid or expired JWT",
        )
    })
}

fn error_response(
    status: StatusCode,
    code: &str,
    message: &str,
) -> (StatusCode, Json<ErrorResponse>) {
    (
        status,
        Json(ErrorResponse {
            code: code.into(),
            message: message.into(),
            details: None,
        }),
    )
}
