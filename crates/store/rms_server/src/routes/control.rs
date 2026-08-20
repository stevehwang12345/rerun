use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    routing::{delete, get, post},
};
use serde_json::{Value, json};

use crate::{
    AppState,
    domain::{
        AccessMode, CommandReceipt, ControlLease, LiveSession, RequestControlLease,
        SendCommandRequest,
    },
    error::{ApiError, ApiResult},
    state::{Catalog, iso_from_millis, new_id, now_iso, now_ms},
};

const LEASE_DURATION_MS: i64 = 120_000;

pub(super) fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/api/v1/live-sessions/{session_id}/control-leases",
            post(request_control_lease),
        )
        .route(
            "/api/v1/live-sessions/{session_id}/control-state",
            get(get_control_state),
        )
        .route(
            "/api/v1/live-sessions/{session_id}/control-leases/{lease_id}",
            delete(release_control_lease),
        )
        .route(
            "/api/v1/live-sessions/{session_id}/commands",
            post(send_command),
        )
}

async fn request_control_lease(
    State(state): State<AppState>,
    Path(session_id): Path<String>,
    Json(request): Json<RequestControlLease>,
) -> ApiResult<(StatusCode, Json<ControlLease>)> {
    let mut catalog = state.catalog.write().await;
    let session = catalog
        .live_sessions
        .get(&session_id)
        .cloned()
        .ok_or_else(|| ApiError::not_found("LiveSession", &session_id))?;
    if session.status != "open" || session.play_state != "following" {
        return Err(ApiError::conflict(
            "Control requires an active following LiveSession.",
        ));
    }
    ensure_session_control_scope(&catalog, &session)?;
    let device = catalog
        .devices
        .get(&session.device_id)
        .cloned()
        .ok_or_else(|| ApiError::not_found("Device", &session.device_id))?;
    ensure_device_can_be_controlled(
        &device.status,
        &device.health,
        device.state_version,
        request.expected_device_version,
    )?;

    let current_time = now_ms();
    let expired_lease_ids = catalog
        .leases
        .values()
        .filter(|lease| lease.device_id == device.id && lease.expires_at_ms <= current_time)
        .map(|lease| lease.id.clone())
        .collect::<Vec<_>>();
    for lease_id in expired_lease_ids {
        catalog.leases.remove(&lease_id);
    }
    if catalog
        .leases
        .values()
        .any(|lease| lease.device_id == device.id)
    {
        return Err(ApiError::conflict(
            "Another operator already holds the Device control lease.",
        ));
    }

    let next_epoch = catalog
        .lease_epochs
        .get(&device.id)
        .copied()
        .unwrap_or(0)
        .saturating_add(1);
    catalog.lease_epochs.insert(device.id.clone(), next_epoch);
    let expires_at_ms = current_time.saturating_add(LEASE_DURATION_MS);
    let lease = ControlLease {
        id: new_id("lease"),
        live_session_id: session.id,
        device_id: device.id,
        holder_id: session.opened_by.clone(),
        holder_name: if session.opened_by == "operator-01" {
            "나".to_owned()
        } else {
            session.opened_by
        },
        expires_at: iso_from_millis(expires_at_ms),
        epoch: next_epoch,
        expires_at_ms,
    };
    catalog.leases.insert(lease.id.clone(), lease.clone());
    catalog.bump_version();
    Ok((StatusCode::CREATED, Json(lease)))
}

async fn release_control_lease(
    State(state): State<AppState>,
    Path((session_id, lease_id)): Path<(String, String)>,
) -> ApiResult<StatusCode> {
    let mut catalog = state.catalog.write().await;
    let lease = catalog
        .leases
        .get(&lease_id)
        .ok_or_else(|| ApiError::not_found("ControlLease", &lease_id))?;
    if lease.live_session_id != session_id {
        return Err(ApiError::not_found("ControlLease", &lease_id));
    }
    catalog.leases.remove(&lease_id);
    catalog.bump_version();
    Ok(StatusCode::NO_CONTENT)
}

async fn send_command(
    State(state): State<AppState>,
    Path(session_id): Path<String>,
    Json(request): Json<SendCommandRequest>,
) -> ApiResult<(StatusCode, Json<CommandReceipt>)> {
    if request.command_type.trim().is_empty() || request.idempotency_key.trim().is_empty() {
        return Err(ApiError::bad_request(
            "commandType and idempotencyKey are required.",
        ));
    }
    if !matches!(
        request.command_type.as_str(),
        "pause_mission" | "resume_mission" | "safe_stop"
    ) {
        return Err(ApiError::bad_request("Unsupported commandType."));
    }
    validate_command_times(&request)?;

    let mut catalog = state.catalog.write().await;
    let idempotency_scope = format!("{session_id}:{}", request.idempotency_key);
    if let Some((original_request, receipt)) = catalog.command_receipts.get(&idempotency_scope) {
        if original_request == &request {
            return Ok((StatusCode::ACCEPTED, Json(receipt.clone())));
        }
        return Err(ApiError::conflict(
            "Idempotency key is already bound to a different command request.",
        ));
    }
    let session = catalog
        .live_sessions
        .get(&session_id)
        .cloned()
        .ok_or_else(|| ApiError::not_found("LiveSession", &session_id))?;
    if session.status != "open" || session.play_state != "following" {
        return Err(ApiError::conflict(
            "Commands require an active following LiveSession.",
        ));
    }
    ensure_session_control_scope(&catalog, &session)?;
    if request
        .live_session_id
        .as_ref()
        .is_some_and(|request_session_id| request_session_id != &session.id)
        || request.device_id != session.device_id
        || request
            .session_mode
            .as_ref()
            .is_some_and(|mode| mode != "live")
    {
        return Err(ApiError::conflict(
            "Command context does not match the LiveSession route.",
        ));
    }
    let device = catalog
        .devices
        .get(&session.device_id)
        .cloned()
        .ok_or_else(|| ApiError::not_found("Device", &session.device_id))?;
    ensure_device_can_be_controlled(
        &device.status,
        &device.health,
        device.state_version,
        request.expected_device_version,
    )?;
    let lease = catalog
        .leases
        .get(&request.lease_id)
        .ok_or_else(|| ApiError::not_found("ControlLease", &request.lease_id))?;
    if lease.live_session_id != session.id
        || lease.device_id != device.id
        || lease.epoch != request.lease_epoch
        || lease.holder_id != session.opened_by
        || lease.expires_at_ms <= now_ms()
    {
        return Err(ApiError::conflict(
            "The command lease is stale or does not match the LiveSession.",
        ));
    }

    let message = match request.command_type.as_str() {
        "pause_mission" => "Mission 일시정지 요청을 수락했습니다.",
        "resume_mission" => "Mission 재개 요청을 수락했습니다.",
        _ => "안전 정지 요청을 수락했습니다.",
    };
    let request_for_idempotency = request.clone();
    let receipt = CommandReceipt {
        command_id: new_id("command"),
        live_session_id: session.id,
        command_type: request.command_type,
        status: "accepted".to_owned(),
        message: message.to_owned(),
        created_at: now_iso(),
    };
    catalog.command_receipts.insert(
        idempotency_scope,
        (request_for_idempotency, receipt.clone()),
    );
    catalog.bump_version();
    Ok((StatusCode::ACCEPTED, Json(receipt)))
}

async fn get_control_state(
    State(state): State<AppState>,
    Path(session_id): Path<String>,
) -> ApiResult<Json<Value>> {
    let catalog = state.catalog.read().await;
    if !catalog.live_sessions.contains_key(&session_id) {
        return Err(ApiError::not_found("LiveSession", &session_id));
    }
    let lease = catalog
        .leases
        .values()
        .find(|lease| lease.live_session_id == session_id && lease.expires_at_ms > now_ms());
    Ok(Json(json!({ "lease": lease })))
}

fn ensure_device_can_be_controlled(
    status: &str,
    health: &str,
    state_version: u64,
    expected_state_version: u64,
) -> ApiResult<()> {
    if status != "online" {
        return Err(ApiError::conflict(
            "Device must be online before control is allowed.",
        ));
    }
    if matches!(health, "restricted" | "critical") {
        return Err(ApiError::conflict("Device health does not allow control."));
    }
    if state_version != expected_state_version {
        return Err(ApiError::conflict(
            "Device stateVersion changed; refresh before requesting control.",
        ));
    }
    Ok(())
}

fn ensure_session_control_scope(catalog: &Catalog, session: &LiveSession) -> ApiResult<()> {
    let has_control_assignment = catalog.device_assignments.values().any(|assignment| {
        assignment.project_id == session.project_id
            && assignment.device_id == session.device_id
            && assignment.access_mode == AccessMode::Control
            && assignment.valid_to.is_none()
    });
    let has_data_assignment = catalog.data_assignments.values().any(|assignment| {
        assignment.project_id == session.project_id
            && assignment.data_source_id == session.data_source_id
            && assignment.valid_to.is_none()
    });
    if !has_control_assignment || !has_data_assignment {
        Err(ApiError::conflict(
            "LiveSession no longer has a control-capable Project assignment.",
        ))
    } else {
        Ok(())
    }
}

fn validate_command_times(request: &SendCommandRequest) -> ApiResult<()> {
    request
        .issued_at
        .parse::<jiff::Timestamp>()
        .map_err(|err| ApiError::bad_request(format!("Invalid issuedAt: {err}")))?;
    let expires_at = request
        .expires_at
        .parse::<jiff::Timestamp>()
        .map_err(|err| ApiError::bad_request(format!("Invalid expiresAt: {err}")))?;
    if expires_at <= jiff::Timestamp::now() {
        return Err(ApiError::conflict(
            "The command validity window has expired.",
        ));
    }
    Ok(())
}
