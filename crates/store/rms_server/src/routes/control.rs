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
    ensure_command_dispatch_available(state.simulated_control_enabled)?;
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
    ensure_session_source_is_control_safe(&mut catalog, &session)?;
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
    ensure_command_dispatch_available(state.simulated_control_enabled)?;
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
    ensure_session_source_is_control_safe(&mut catalog, &session)?;
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

fn ensure_command_dispatch_available(simulated_control_enabled: bool) -> ApiResult<()> {
    if simulated_control_enabled {
        Ok(())
    } else {
        Err(ApiError::unavailable(
            "Actuator command dispatch is not configured; simulated control is disabled.",
        ))
    }
}

fn ensure_session_control_scope(catalog: &Catalog, session: &LiveSession) -> ApiResult<()> {
    let uses_observation_only_edge = catalog
        .devices
        .get(&session.device_id)
        .and_then(|device| catalog.integrations.get(&device.integration_id))
        .is_some_and(|integration| integration.kind == "rms_edge");
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
    if uses_observation_only_edge {
        Err(ApiError::conflict(
            "The RMS Edge Agent adapters are observation-only; actuator command dispatch is not configured.",
        ))
    } else if !has_control_assignment || !has_data_assignment {
        Err(ApiError::conflict(
            "LiveSession no longer has a control-capable Project assignment.",
        ))
    } else {
        Ok(())
    }
}

fn ensure_session_source_is_control_safe(
    catalog: &mut Catalog,
    session: &LiveSession,
) -> ApiResult<()> {
    let source_is_safe = session.source_health == "fresh"
        && catalog
            .data_sources
            .get(&session.data_source_id)
            .is_some_and(|source| {
                source.device_id == session.device_id && source.status == "recording"
            });
    if source_is_safe {
        return Ok(());
    }

    let lease_count = catalog.leases.len();
    catalog
        .leases
        .retain(|_, lease| lease.live_session_id != session.id);
    if catalog.leases.len() != lease_count {
        catalog.bump_version();
    }
    Err(ApiError::conflict(
        "Live source is not fresh and recording; control was revoked.",
    ))
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

#[cfg(test)]
mod tests {
    use super::*;

    fn live_session(source_health: &str) -> LiveSession {
        LiveSession {
            id: "live-session-source-safety".to_owned(),
            project_id: "project-logistics".to_owned(),
            device_id: "robot-07".to_owned(),
            data_source_id: "robot-07-source".to_owned(),
            opened_by: "operator-01".to_owned(),
            status: "open".to_owned(),
            play_state: "following".to_owned(),
            source_health: source_health.to_owned(),
            stream_url: "/rerun/live/live-session-source-safety".to_owned(),
            started_at: "2026-08-20T00:00:00Z".to_owned(),
            closed_at: None,
            resource_version: 1,
        }
    }

    fn lease(session: &LiveSession) -> ControlLease {
        ControlLease {
            id: "lease-source-safety".to_owned(),
            live_session_id: session.id.clone(),
            device_id: session.device_id.clone(),
            holder_id: session.opened_by.clone(),
            holder_name: "나".to_owned(),
            expires_at: "2099-08-20T00:00:00Z".to_owned(),
            epoch: 1,
            expires_at_ms: i64::MAX,
        }
    }

    #[tokio::test]
    async fn command_fails_closed_and_revokes_lease_when_source_becomes_offline() {
        let state = AppState::fixture();
        let session = live_session("fresh");
        let lease = lease(&session);
        {
            let mut catalog = state.catalog.write().await;
            catalog
                .live_sessions
                .insert(session.id.clone(), session.clone());
            catalog.leases.insert(lease.id.clone(), lease.clone());
            catalog
                .data_sources
                .get_mut(&session.data_source_id)
                .expect("Fixture source exists")
                .status = "offline".to_owned();
        }

        let result = send_command(
            State(state.clone()),
            Path(session.id.clone()),
            Json(SendCommandRequest {
                live_session_id: Some(session.id.clone()),
                device_id: session.device_id,
                command_type: "safe_stop".to_owned(),
                expected_device_version: 142,
                idempotency_key: "unsafe-source-command".to_owned(),
                lease_id: lease.id,
                lease_epoch: lease.epoch,
                session_mode: Some("live".to_owned()),
                issued_at: "2026-08-20T00:00:00Z".to_owned(),
                expires_at: "2099-08-20T00:00:00Z".to_owned(),
            }),
        )
        .await;
        assert!(result.is_err());

        let catalog = state.catalog.read().await;
        assert!(
            catalog
                .leases
                .values()
                .all(|lease| lease.live_session_id != session.id)
        );
        assert!(catalog.command_receipts.is_empty());
    }

    #[tokio::test]
    async fn delayed_or_removed_sources_revoke_existing_session_leases() {
        for source_case in ["delayed", "removed"] {
            let state = AppState::fixture();
            let session = live_session(if source_case == "delayed" {
                "delayed"
            } else {
                "fresh"
            });
            let mut catalog = state.catalog.write().await;
            catalog
                .leases
                .insert("lease-source-safety".to_owned(), lease(&session));
            if source_case == "removed" {
                catalog.data_sources.remove(&session.data_source_id);
            }

            assert!(ensure_session_source_is_control_safe(&mut catalog, &session).is_err());
            assert!(catalog.leases.is_empty());
        }
    }

    #[tokio::test]
    async fn observation_only_edge_integration_never_accepts_control() {
        let state = AppState::fixture();
        let session = live_session("fresh");
        let mut catalog = state.catalog.write().await;
        {
            let integration = catalog
                .integrations
                .get_mut("integration-logistics")
                .expect("fixture integration exists");
            integration.kind = "rms_edge".to_owned();
            integration.status = "testing".to_owned();
        }
        assert!(ensure_session_control_scope(&catalog, &session).is_err());
        catalog
            .integrations
            .get_mut("integration-logistics")
            .expect("fixture integration exists")
            .status = "connected".to_owned();
        assert!(ensure_session_control_scope(&catalog, &session).is_err());
    }

    #[test]
    fn production_control_fails_closed_without_a_dispatcher() {
        assert!(ensure_command_dispatch_available(false).is_err());
        assert!(ensure_command_dispatch_available(true).is_ok());
    }
}
