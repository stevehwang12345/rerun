use std::{convert::Infallible, time::Duration};

use axum::{
    Json, Router,
    body::Body,
    extract::{Path, State},
    http::{HeaderValue, StatusCode, header},
    response::{IntoResponse as _, Response, Sse, sse::Event},
    routing::{get, post},
};
use serde_json::json;
use tokio::sync::{mpsc, watch};
use tokio_stream::wrappers::ReceiverStream;

use crate::{
    AppState,
    domain::{
        CreateLiveSessionRequest, CreateReplaySessionRequest, LiveSession, Recording,
        RecordingProjectSnapshot, ReplaySession,
    },
    error::{ApiError, ApiResult},
    state::{new_id, now_iso},
};

const RRD_FIXTURE: &[u8] =
    include_bytes!("../../../../../tests/assets/rrd/snippets/views/spatial3d.rrd");

pub(super) fn router() -> Router<AppState> {
    Router::new()
        .route("/api/v1/live-sessions", post(create_live_session))
        .route(
            "/api/v1/live-sessions/{session_id}",
            get(get_live_session).delete(close_live_session),
        )
        .route(
            "/api/v1/live-sessions/{session_id}/events",
            get(live_events),
        )
        .route("/api/v1/replay-sessions", post(create_replay_session))
        .route(
            "/api/v1/replay-sessions/{session_id}",
            get(get_replay_session).delete(close_replay_session),
        )
        .route("/rerun/live/{session_id}", get(open_live_stream))
        .route("/rerun/replay/{session_id}", get(open_replay_stream))
        .route(
            "/rerun/recordings/{recording_id}",
            get(open_recording_stream),
        )
        .route("/rerun/fixture/spatial3d.rrd", get(open_fixture_stream))
}

async fn create_live_session(
    State(state): State<AppState>,
    Json(request): Json<CreateLiveSessionRequest>,
) -> ApiResult<(StatusCode, Json<LiveSession>)> {
    if request.opened_by.trim().is_empty() {
        return Err(ApiError::bad_request("openedBy is required."));
    }
    let mut catalog = state.catalog.write().await;
    let project = catalog
        .projects
        .get(&request.project_id)
        .cloned()
        .ok_or_else(|| ApiError::not_found("Project", &request.project_id))?;
    let device = catalog
        .devices
        .get(&request.device_id)
        .cloned()
        .ok_or_else(|| ApiError::not_found("Device", &request.device_id))?;
    let source = catalog
        .data_sources
        .get(&request.data_source_id)
        .cloned()
        .ok_or_else(|| ApiError::not_found("DataSource", &request.data_source_id))?;
    if project.organization_id != device.organization_id {
        return Err(ApiError::conflict(
            "Project and Device must belong to the same organization.",
        ));
    }
    if source.device_id != device.id {
        return Err(ApiError::conflict(
            "DataSource does not belong to the requested Device.",
        ));
    }
    if device.status == "offline" {
        return Err(ApiError::conflict(
            "An offline Device cannot start a LiveSession.",
        ));
    }
    if matches!(source.status.as_str(), "offline" | "degraded") {
        return Err(ApiError::conflict(
            "DataSource is not available for a LiveSession.",
        ));
    }
    let device_is_assigned = catalog.device_assignments.values().any(|assignment| {
        assignment.project_id == request.project_id
            && assignment.device_id == request.device_id
            && assignment.valid_to.is_none()
    });
    let source_is_assigned = catalog.data_assignments.values().any(|assignment| {
        assignment.project_id == request.project_id
            && assignment.data_source_id == request.data_source_id
            && assignment.valid_to.is_none()
    });
    if !device_is_assigned || !source_is_assigned {
        return Err(ApiError::conflict(
            "Device and DataSource must both be assigned to the Project.",
        ));
    }

    let resource_version = catalog.bump_version();
    let session_id = new_id("live-session");
    let session = LiveSession {
        id: session_id.clone(),
        project_id: request.project_id,
        device_id: request.device_id,
        data_source_id: request.data_source_id,
        opened_by: request.opened_by,
        status: "open".to_owned(),
        play_state: "following".to_owned(),
        source_health: if device.status == "degraded" {
            "delayed"
        } else {
            "fresh"
        }
        .to_owned(),
        stream_url: format!("/rerun/live/{session_id}"),
        started_at: now_iso(),
        closed_at: None,
        resource_version,
    };
    catalog
        .live_sessions
        .insert(session.id.clone(), session.clone());
    let (shutdown, _receiver) = watch::channel(false);
    catalog
        .live_session_shutdown
        .insert(session.id.clone(), shutdown);
    Ok((StatusCode::CREATED, Json(session)))
}

async fn get_live_session(
    State(state): State<AppState>,
    Path(session_id): Path<String>,
) -> ApiResult<Json<LiveSession>> {
    let catalog = state.catalog.read().await;
    catalog
        .live_sessions
        .get(&session_id)
        .cloned()
        .map(Json)
        .ok_or_else(|| ApiError::not_found("LiveSession", &session_id))
}

async fn close_live_session(
    State(state): State<AppState>,
    Path(session_id): Path<String>,
) -> ApiResult<Json<Recording>> {
    let mut catalog = state.catalog.write().await;
    let session = catalog
        .live_sessions
        .get(&session_id)
        .cloned()
        .ok_or_else(|| ApiError::not_found("LiveSession", &session_id))?;
    if session.status != "open" {
        return Err(ApiError::conflict("LiveSession is already closed."));
    }
    let project = catalog
        .projects
        .get(&session.project_id)
        .cloned()
        .ok_or_else(|| ApiError::not_found("Project", &session.project_id))?;
    let source = catalog
        .data_sources
        .get(&session.data_source_id)
        .cloned()
        .ok_or_else(|| ApiError::not_found("DataSource", &session.data_source_id))?;
    let device_assignment = catalog
        .device_assignments
        .values()
        .find(|assignment| {
            assignment.project_id == session.project_id
                && assignment.device_id == session.device_id
                && assignment.valid_to.is_none()
        })
        .cloned()
        .ok_or_else(|| ApiError::conflict("Active DeviceAssignment is required."))?;
    let data_assignment = catalog
        .data_assignments
        .values()
        .find(|assignment| {
            assignment.project_id == session.project_id
                && assignment.data_source_id == session.data_source_id
                && assignment.valid_to.is_none()
        })
        .cloned()
        .ok_or_else(|| ApiError::conflict("Active DataAssignment is required."))?;

    let closed_at = now_iso();
    let resource_version = catalog.bump_version();
    let mut closed_session = session.clone();
    closed_session.status = "closed".to_owned();
    closed_session.closed_at = Some(closed_at.clone());
    closed_session.resource_version = resource_version;
    catalog
        .live_sessions
        .insert(session.id.clone(), closed_session);
    catalog
        .leases
        .retain(|_, lease| lease.live_session_id != session.id);
    if let Some(shutdown) = catalog.live_session_shutdown.remove(&session.id) {
        shutdown.send_replace(true);
    }

    let recording_id = new_id("recording");
    let recording = Recording {
        id: recording_id.clone(),
        organization_id: project.organization_id,
        project_id: project.id.clone(),
        device_id: session.device_id,
        data_source_id: source.id.clone(),
        name: format!("{} 운용 기록", project.name),
        status: "ready".to_owned(),
        rrd_url: format!("/rerun/recordings/{recording_id}"),
        captured_at: session.started_at,
        duration_label: "방금 종료".to_owned(),
        topic_ids: source.topic_ids,
        mapping_version: source.mapping_version,
        project_snapshot: RecordingProjectSnapshot {
            project_id: project.id,
            project_name: project.name,
            captured_at: closed_at,
            device_assignment_id: device_assignment.id,
            data_assignment_id: data_assignment.id,
        },
        resource_version,
        manifest_hash: format!("sha256:{recording_id}-immutable-local-fixture"),
    };
    catalog
        .recordings
        .insert(recording.id.clone(), recording.clone());
    Ok(Json(recording))
}

async fn create_replay_session(
    State(state): State<AppState>,
    Json(request): Json<CreateReplaySessionRequest>,
) -> ApiResult<(StatusCode, Json<ReplaySession>)> {
    if request.opened_by.trim().is_empty() {
        return Err(ApiError::bad_request("openedBy is required."));
    }
    let mut catalog = state.catalog.write().await;
    if !catalog.projects.contains_key(&request.project_id) {
        return Err(ApiError::not_found("Project", &request.project_id));
    }
    let recording = catalog
        .recordings
        .get(&request.recording_id)
        .cloned()
        .ok_or_else(|| ApiError::not_found("Recording", &request.recording_id))?;
    if recording.project_id != request.project_id {
        return Err(ApiError::conflict(
            "Recording is not part of the requested Project snapshot.",
        ));
    }
    if recording.status != "ready" {
        return Err(ApiError::conflict(
            "Only a ready Recording can start a ReplaySession.",
        ));
    }

    let resource_version = catalog.bump_version();
    let session_id = new_id("replay-session");
    let session = ReplaySession {
        id: session_id.clone(),
        project_id: request.project_id,
        recording_id: request.recording_id,
        device_id: recording.device_id,
        opened_by: request.opened_by,
        status: "open".to_owned(),
        stream_url: format!("/rerun/replay/{session_id}"),
        cursor_seconds: 0.0,
        opened_at: now_iso(),
        closed_at: None,
        resource_version,
    };
    catalog
        .replay_sessions
        .insert(session.id.clone(), session.clone());
    Ok((StatusCode::CREATED, Json(session)))
}

async fn get_replay_session(
    State(state): State<AppState>,
    Path(session_id): Path<String>,
) -> ApiResult<Json<ReplaySession>> {
    let catalog = state.catalog.read().await;
    catalog
        .replay_sessions
        .get(&session_id)
        .cloned()
        .map(Json)
        .ok_or_else(|| ApiError::not_found("ReplaySession", &session_id))
}

async fn close_replay_session(
    State(state): State<AppState>,
    Path(session_id): Path<String>,
) -> ApiResult<StatusCode> {
    let mut catalog = state.catalog.write().await;
    let mut session = catalog
        .replay_sessions
        .get(&session_id)
        .cloned()
        .ok_or_else(|| ApiError::not_found("ReplaySession", &session_id))?;
    if session.status == "closed" {
        return Ok(StatusCode::NO_CONTENT);
    }
    session.status = "closed".to_owned();
    session.closed_at = Some(now_iso());
    session.resource_version = catalog.bump_version();
    catalog.replay_sessions.insert(session_id, session);
    Ok(StatusCode::NO_CONTENT)
}

async fn open_live_stream(
    State(state): State<AppState>,
    Path(session_id): Path<String>,
) -> ApiResult<Response> {
    let catalog = state.catalog.read().await;
    let session = catalog
        .live_sessions
        .get(&session_id)
        .ok_or_else(|| ApiError::not_found("LiveSession", &session_id))?;
    if session.status != "open" {
        return Err(ApiError::conflict("LiveSession is closed."));
    }
    Ok(rrd_response())
}

async fn open_replay_stream(
    State(state): State<AppState>,
    Path(session_id): Path<String>,
) -> ApiResult<Response> {
    let catalog = state.catalog.read().await;
    let session = catalog
        .replay_sessions
        .get(&session_id)
        .ok_or_else(|| ApiError::not_found("ReplaySession", &session_id))?;
    if session.status != "open" {
        return Err(ApiError::conflict("ReplaySession is closed."));
    }
    Ok(rrd_response())
}

async fn open_recording_stream(
    State(state): State<AppState>,
    Path(recording_id): Path<String>,
) -> ApiResult<Response> {
    let catalog = state.catalog.read().await;
    let recording = catalog
        .recordings
        .get(&recording_id)
        .ok_or_else(|| ApiError::not_found("Recording", &recording_id))?;
    if recording.status != "ready" {
        return Err(ApiError::conflict("Recording is not ready."));
    }
    Ok(rrd_response())
}

fn rrd_response() -> Response {
    let mut response = Body::from(RRD_FIXTURE).into_response();
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/octet-stream"),
    );
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response.headers_mut().insert(
        header::ACCESS_CONTROL_ALLOW_ORIGIN,
        HeaderValue::from_static("*"),
    );
    response
}

async fn open_fixture_stream() -> Response {
    rrd_response()
}

async fn live_events(
    State(state): State<AppState>,
    Path(session_id): Path<String>,
) -> ApiResult<Response> {
    let catalog = state.catalog.read().await;
    let session = catalog
        .live_sessions
        .get(&session_id)
        .cloned()
        .ok_or_else(|| ApiError::not_found("LiveSession", &session_id))?;
    if session.status != "open" {
        return Err(ApiError::conflict("LiveSession is closed."));
    }
    let device = catalog
        .devices
        .get(&session.device_id)
        .cloned()
        .ok_or_else(|| ApiError::not_found("Device", &session.device_id))?;
    let _source = catalog
        .data_sources
        .get(&session.data_source_id)
        .cloned()
        .ok_or_else(|| ApiError::not_found("DataSource", &session.data_source_id))?;
    let mut shutdown = catalog
        .live_session_shutdown
        .get(&session_id)
        .ok_or_else(|| ApiError::conflict("LiveSession event stream is unavailable."))?
        .subscribe();
    drop(catalog);

    let (sender, receiver) = mpsc::channel::<Result<Event, Infallible>>(4);
    tokio::spawn(async move {
        let topic_id = if device.kind == "drone" {
            "altitude"
        } else {
            "velocity"
        };
        let mut tick = 0_u64;
        let mut interval = tokio::time::interval(Duration::from_millis(1_200));
        loop {
            tokio::select! {
                biased;
                changed = shutdown.changed() => {
                    if changed.is_err() || *shutdown.borrow() {
                        break;
                    }
                }
                _ = interval.tick() => {
                    tick = tick.saturating_add(1);
                    let event = json!({
                        "eventId": format!("live-event-{}-{tick}", session.id),
                        "type": "topic.value.changed",
                        "occurredAt": now_iso(),
                        "projectId": session.project_id,
                        "deviceId": session.device_id,
                        "liveSessionId": session.id,
                        "resourceVersion": device.state_version.saturating_add(tick),
                        "data": {
                            "id": topic_id,
                            "value": if topic_id == "altitude" { "0.0" } else { "1.18" },
                            "samples": [0.9, 1.0, 1.1, 1.18],
                            "quality": if device.status == "degraded" { "delayed" } else { "fresh" },
                            "updatedAt": now_iso()
                        }
                    });
                    let event = Event::default()
                        .event("topic.value.changed")
                        .data(event.to_string());
                    if sender.send(Ok(event)).await.is_err() {
                        break;
                    }
                }
            }
        }
    });
    Ok(Sse::new(ReceiverStream::new(receiver))
        .keep_alive(
            axum::response::sse::KeepAlive::new()
                .interval(Duration::from_secs(10))
                .text("keep-alive"),
        )
        .into_response())
}
