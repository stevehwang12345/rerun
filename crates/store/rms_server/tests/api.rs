use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Method, Request, StatusCode, header},
};
use serde_json::{Value, json};
use tokio::time::{Duration, timeout};
use tokio_stream::StreamExt as _;
use tower::ServiceExt as _;

#[tokio::test]
async fn local_origin_host_and_preflight_policy_is_fail_closed() {
    let app = rms_server::fixture_router();
    let rebinding = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/v1/projects")
                .header(header::HOST, "attacker.example:8080")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(rebinding.status(), StatusCode::FORBIDDEN);

    let evil_origin = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/v1/projects")
                .header(header::HOST, "127.0.0.1:8080")
                .header(header::ORIGIN, "https://evil.example")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(evil_origin.status(), StatusCode::FORBIDDEN);

    let preflight = app
        .oneshot(
            Request::builder()
                .method(Method::OPTIONS)
                .uri("/api/v1/recording-imports")
                .header(header::HOST, "localhost:8080")
                .header(header::ORIGIN, "http://localhost:4173")
                .header(header::ACCESS_CONTROL_REQUEST_METHOD, "POST")
                .header(
                    header::ACCESS_CONTROL_REQUEST_HEADERS,
                    "content-type,idempotency-key,x-rms-request-id",
                )
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(preflight.status(), StatusCode::NO_CONTENT);
    assert_eq!(
        preflight.headers()[header::ACCESS_CONTROL_ALLOW_ORIGIN],
        "http://localhost:4173"
    );
    assert_ne!(
        preflight.headers()[header::ACCESS_CONTROL_ALLOW_ORIGIN],
        "*"
    );
    assert!(
        preflight.headers()[header::ACCESS_CONTROL_ALLOW_HEADERS]
            .to_str()
            .unwrap()
            .contains("Idempotency-Key")
    );
    assert!(
        preflight.headers()[header::ACCESS_CONTROL_ALLOW_HEADERS]
            .to_str()
            .unwrap()
            .contains("X-RMS-Request-ID")
    );
    assert_eq!(
        preflight.headers()[header::ACCESS_CONTROL_ALLOW_CREDENTIALS],
        "true"
    );
}

#[tokio::test]
async fn catalog_and_workspace_match_the_web_contract() {
    let app = rms_server::fixture_router();
    let (status, integrations) =
        request_json(&app, Method::GET, "/api/v1/integrations", None).await;
    assert_eq!(status, StatusCode::OK);
    let integration = &integrations[0];
    for field in [
        "organizationId",
        "kind",
        "endpointLabel",
        "lastHealthAt",
        "createdAt",
        "resourceVersion",
    ] {
        assert!(integration.get(field).is_some(), "missing {field}");
    }

    let (status, workspace) = request_json(
        &app,
        Method::GET,
        "/api/v1/projects/project-logistics/workspace",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    for field in [
        "snapshotVersion",
        "capturedAt",
        "project",
        "deviceAssignments",
        "dataAssignments",
        "devices",
        "dataSources",
        "recordings",
        "topicsByRecording",
        "topicsByDataSource",
    ] {
        assert!(workspace.get(field).is_some(), "missing {field}");
    }
    assert!(workspace.get("assignments").is_none());

    let first_device = &workspace["devices"][0];
    let first_source = &workspace["dataSources"][0];
    assert!(first_device.get("integrationId").is_some());
    assert!(first_device.get("projectId").is_none());
    assert!(first_source.get("liveUrl").is_some());
    assert!(first_source.get("lastDataAt").is_some());
    assert!(first_source.get("organizationId").is_none());
    assert!(first_source.get("projectId").is_none());

    let (status, topics) = request_json(
        &app,
        Method::GET,
        "/api/v1/data-sources/robot-07-source/topics",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(topics[0]["dataSourceId"], "robot-07-source");

    let (status, recordings) = request_json(
        &app,
        Method::GET,
        "/api/v1/projects/project-logistics/recordings",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(recordings[0].get("projectId").is_some());
    assert!(recordings[0]["projectSnapshot"].get("capturedAt").is_some());
    assert_eq!(recordings[0]["defaultTimeline"], "tick");
    assert_eq!(recordings[0]["durationSeconds"], 50.0);
    assert_eq!(recordings[0]["rrdVersion"], "0.36.1");
    assert_eq!(recordings[0]["footerVerified"], true);
    assert_eq!(
        recordings[0]["contentSha256"],
        "0beb50db77d303ef8a4f80b1d45dc84f4fa1416357eb1605d82ea08a938ce414"
    );
    assert_eq!(
        recordings[0]["timelines"],
        json!([{
            "name": "tick",
            "kind": "sequence",
            "start": "0",
            "end": "100",
            "durationSeconds": 50.0,
            "fps": 2.0
        }])
    );
}

#[tokio::test]
async fn newly_registered_data_source_materializes_its_topic_contract() {
    let app = rms_server::fixture_router();
    let (_, integration) = request_json(
        &app,
        Method::POST,
        "/api/v1/integrations",
        Some(json!({
            "organizationId": "org-rms",
            "name": "새 ROS 2 연동",
            "kind": "ros2",
            "endpointLabel": "테스트 Edge"
        })),
    )
    .await;
    let integration_id = integration["id"]
        .as_str()
        .expect("Integration response has an ID");
    let (_, device) = request_json(
        &app,
        Method::POST,
        "/api/v1/devices",
        Some(json!({
            "organizationId": "org-rms",
            "integrationId": integration_id,
            "name": "Test Robot",
            "kind": "robot",
            "status": "online",
            "health": "normal",
            "operationMode": "대기",
            "taskName": "할당 없음",
            "taskProgress": 0,
            "lastSeenAt": "2026-08-20T00:00:00Z"
        })),
    )
    .await;
    let device_id = device["id"].as_str().expect("Device response has an ID");
    let (_, source) = request_json(
        &app,
        Method::POST,
        "/api/v1/data-sources",
        Some(json!({
            "integrationId": integration_id,
            "deviceId": device_id,
            "name": "실시간",
            "protocol": "ROS 2 + Rerun",
            "status": "recording",
            "liveUrl": "http://127.0.0.1:8080/rerun/fixture/spatial3d.rrd",
            "topicIds": ["pose", "battery"],
            "lastDataAt": "2026-08-20T00:00:00Z"
        })),
    )
    .await;
    let source_id = source["id"]
        .as_str()
        .expect("DataSource response has an ID");
    let (status, topics) = request_json(
        &app,
        Method::GET,
        &format!("/api/v1/data-sources/{source_id}/topics"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(topics.as_array().map(Vec::len), Some(2));
    assert_eq!(topics[0]["dataSourceId"], source_id);
}

#[tokio::test]
async fn project_assignment_is_explicit_and_control_is_exclusive() {
    let app = rms_server::fixture_router();
    let (status, project) = request_json(
        &app,
        Method::POST,
        "/api/v1/projects",
        Some(json!({
            "organizationId": "org-rms",
            "name": "공유 관제",
            "description": "Observe-only project",
            "status": "active"
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert!(project.get("createdAt").is_some());
    assert!(project.get("resourceVersion").is_some());
    let project_id = project["id"].as_str().expect("Project response has an ID");

    let (status, assignment) = request_json(
        &app,
        Method::POST,
        &format!("/api/v1/projects/{project_id}/device-assignments"),
        Some(json!({ "deviceId": "robot-07", "accessMode": "observe" })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(assignment["accessMode"], "observe");
    assert!(assignment.get("validFrom").is_some());
    assert!(assignment.get("resourceVersion").is_some());

    let (status, _) = request_json(
        &app,
        Method::POST,
        &format!("/api/v1/projects/{project_id}/data-assignments"),
        Some(json!({
            "dataSourceId": "robot-07-source",
            "visibility": "operator"
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let (status, live_session) = request_json(
        &app,
        Method::POST,
        "/api/v1/live-sessions",
        Some(json!({
            "projectId": project_id,
            "deviceId": "robot-07",
            "dataSourceId": "robot-07-source",
            "openedBy": "observer-01"
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let observe_session_id = live_session["id"]
        .as_str()
        .expect("Observe LiveSession response has an ID");
    let (status, error) = request_json(
        &app,
        Method::POST,
        &format!("/api/v1/live-sessions/{observe_session_id}/control-leases"),
        Some(json!({ "scope": "motion", "expectedDeviceVersion": 142 })),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(error["code"], "conflict");

    let (status, error) = request_json(
        &app,
        Method::POST,
        &format!("/api/v1/projects/{project_id}/device-assignments"),
        Some(json!({ "deviceId": "drone-03", "accessMode": "control" })),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(error["code"], "conflict");
}

#[tokio::test]
async fn live_control_and_close_flow_match_the_web_contract() {
    let app = rms_server::fixture_router();
    let live_session = create_live_session(&app, "robot-07", "robot-07-source").await;
    assert_eq!(live_session["status"], "open");
    assert_eq!(live_session["playState"], "following");
    assert_eq!(live_session["sourceHealth"], "fresh");
    assert!(live_session.get("resourceVersion").is_some());
    assert!(
        live_session["streamUrl"]
            .as_str()
            .is_some_and(|url| url.starts_with('/'))
    );
    let live_session_id = live_session["id"]
        .as_str()
        .expect("LiveSession response has an ID");

    let (status, control_state) = request_json(
        &app,
        Method::GET,
        &format!("/api/v1/live-sessions/{live_session_id}/control-state"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(control_state["lease"].is_null());

    let (status, lease) = request_json(
        &app,
        Method::POST,
        &format!("/api/v1/live-sessions/{live_session_id}/control-leases"),
        Some(json!({ "scope": "motion", "expectedDeviceVersion": 142 })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(lease["liveSessionId"], live_session_id);
    let lease_id = lease["id"].as_str().expect("Lease response has an ID");
    let lease_epoch = lease["epoch"]
        .as_u64()
        .expect("Lease response has an epoch");

    let command = json!({
        "liveSessionId": live_session_id,
        "deviceId": "robot-07",
        "commandType": "pause_mission",
        "expectedDeviceVersion": 142,
        "idempotencyKey": "api-test-command",
        "leaseId": lease_id,
        "leaseEpoch": lease_epoch,
        "sessionMode": "live",
        "issuedAt": "2026-08-20T00:00:00Z",
        "expiresAt": "2099-08-20T00:00:00Z"
    });
    let (status, receipt) = request_json(
        &app,
        Method::POST,
        &format!("/api/v1/live-sessions/{live_session_id}/commands"),
        Some(command.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(receipt["status"], "accepted");
    assert!(receipt.get("deviceId").is_none());
    let (_, duplicate) = request_json(
        &app,
        Method::POST,
        &format!("/api/v1/live-sessions/{live_session_id}/commands"),
        Some(command.clone()),
    )
    .await;
    assert_eq!(duplicate["commandId"], receipt["commandId"]);

    let mut conflicting_commands = Vec::new();
    let mut different_type = command.clone();
    different_type["commandType"] = json!("safe_stop");
    conflicting_commands.push(different_type);
    let mut different_device = command.clone();
    different_device["deviceId"] = json!("drone-03");
    conflicting_commands.push(different_device);
    let mut different_version = command.clone();
    different_version["expectedDeviceVersion"] = json!(143);
    conflicting_commands.push(different_version);
    let mut different_lease = command;
    different_lease["leaseId"] = json!("lease-other");
    conflicting_commands.push(different_lease);

    for conflicting_command in conflicting_commands {
        let (status, error) = request_json(
            &app,
            Method::POST,
            &format!("/api/v1/live-sessions/{live_session_id}/commands"),
            Some(conflicting_command),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(error["code"], "conflict");
        assert_eq!(
            error["message"],
            "Idempotency key is already bound to a different command request."
        );
    }

    let (status, recording) = request_json(
        &app,
        Method::DELETE,
        &format!("/api/v1/live-sessions/{live_session_id}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(recording["projectId"], "project-logistics");
    assert_eq!(recording["status"], "ready");
    assert_eq!(recording["defaultTimeline"], "tick");
    assert_eq!(recording["durationSeconds"], 50.0);
    assert_eq!(recording["footerVerified"], true);
    assert!(
        recording["projectSnapshot"]
            .get("deviceAssignmentId")
            .is_some()
    );

    let (status, closed) = request_json(
        &app,
        Method::GET,
        &format!("/api/v1/live-sessions/{live_session_id}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(closed["status"], "closed");
    assert!(closed.get("closedAt").is_some());
}

#[tokio::test]
async fn replay_is_command_free_and_can_be_closed() {
    let app = rms_server::fixture_router();
    let (status, replay_session) = request_json(
        &app,
        Method::POST,
        "/api/v1/replay-sessions",
        Some(json!({
            "projectId": "project-logistics",
            "recordingId": "recording-robot-07-incident",
            "openedBy": "operator-01"
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(replay_session["status"], "open");
    assert_eq!(replay_session["deviceId"], "robot-07");
    assert_eq!(replay_session["cursorSeconds"], 0.0);
    assert_eq!(replay_session["initialTimeline"], "tick");
    assert_eq!(
        replay_session["initialCursor"],
        json!({ "kind": "sequence", "value": "0" })
    );
    assert_eq!(replay_session["initialPlayState"], "playing");
    assert_eq!(replay_session["initialSpeed"], 1.0);
    assert_eq!(replay_session["initialLoop"], json!({ "mode": "off" }));
    assert!(replay_session.get("openedAt").is_some());
    assert!(
        replay_session["streamUrl"]
            .as_str()
            .is_some_and(|url| url.starts_with('/'))
    );
    let replay_session_id = replay_session["id"]
        .as_str()
        .expect("ReplaySession response has an ID");

    let response = request(
        &app,
        Method::GET,
        &format!("/rerun/replay/{replay_session_id}"),
        None,
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);

    let response = request(
        &app,
        Method::POST,
        &format!("/api/v1/replay-sessions/{replay_session_id}/commands"),
        Some(json!({ "commandType": "safe_stop" })),
    )
    .await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);

    let response = request(
        &app,
        Method::DELETE,
        &format!("/api/v1/replay-sessions/{replay_session_id}"),
        None,
    )
    .await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    let (_, closed) = request_json(
        &app,
        Method::GET,
        &format!("/api/v1/replay-sessions/{replay_session_id}"),
        None,
    )
    .await;
    assert_eq!(closed["status"], "closed");
    assert!(closed.get("closedAt").is_some());
    let response = request(
        &app,
        Method::GET,
        &format!("/rerun/replay/{replay_session_id}"),
        None,
    )
    .await;
    assert_eq!(response.status(), StatusCode::CONFLICT);
}

#[tokio::test]
async fn active_live_session_fences_assignment_deletion_and_closes_its_capabilities() {
    let app = rms_server::fixture_router();
    let (_, workspace) = request_json(
        &app,
        Method::GET,
        "/api/v1/projects/project-logistics/workspace",
        None,
    )
    .await;
    let device_assignment_id = workspace["deviceAssignments"]
        .as_array()
        .expect("Workspace device assignments are an array")
        .iter()
        .find(|assignment| assignment["deviceId"] == "robot-07")
        .and_then(|assignment| assignment["id"].as_str())
        .expect("Robot-07 assignment exists")
        .to_owned();
    let data_assignment_id = workspace["dataAssignments"]
        .as_array()
        .expect("Workspace data assignments are an array")
        .iter()
        .find(|assignment| assignment["dataSourceId"] == "robot-07-source")
        .and_then(|assignment| assignment["id"].as_str())
        .expect("Robot-07 source assignment exists")
        .to_owned();

    let live_session = create_live_session(&app, "robot-07", "robot-07-source").await;
    let live_session_id = live_session["id"]
        .as_str()
        .expect("LiveSession response has an ID");
    let (status, _) = request_json(
        &app,
        Method::POST,
        &format!("/api/v1/live-sessions/{live_session_id}/control-leases"),
        Some(json!({ "scope": "motion", "expectedDeviceVersion": 142 })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);

    let response = request(
        &app,
        Method::GET,
        &format!("/api/v1/live-sessions/{live_session_id}/events"),
        None,
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let mut event_stream = response.into_body().into_data_stream();
    let first_event = timeout(Duration::from_secs(1), event_stream.next())
        .await
        .expect("Live SSE emits without delay")
        .expect("Live SSE remains open")
        .expect("Live SSE body is readable");
    assert!(String::from_utf8_lossy(&first_event).contains("topic.value.changed"));

    for (assignment_kind, assignment_id) in [
        ("device-assignments", device_assignment_id.as_str()),
        ("data-assignments", data_assignment_id.as_str()),
    ] {
        let (status, error) = request_json(
            &app,
            Method::DELETE,
            &format!("/api/v1/projects/project-logistics/{assignment_kind}/{assignment_id}"),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(error["code"], "conflict");
        assert!(
            error["message"]
                .as_str()
                .is_some_and(|message| message.starts_with("Close the active LiveSession"))
        );
    }

    let (status, recording) = request_json(
        &app,
        Method::DELETE,
        &format!("/api/v1/live-sessions/{live_session_id}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let recording_url = recording["rrdUrl"]
        .as_str()
        .expect("Finalized Recording has an RRD URL");
    assert!(recording_url.starts_with("/rerun/recordings/"));

    let stream_end = timeout(Duration::from_secs(1), event_stream.next())
        .await
        .expect("Closing LiveSession promptly ends its SSE stream");
    assert!(stream_end.is_none());

    let (status, control_state) = request_json(
        &app,
        Method::GET,
        &format!("/api/v1/live-sessions/{live_session_id}/control-state"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(control_state["lease"].is_null());

    let response = request(
        &app,
        Method::GET,
        &format!("/rerun/live/{live_session_id}"),
        None,
    )
    .await;
    assert_eq!(response.status(), StatusCode::CONFLICT);
    let response = request(&app, Method::GET, recording_url, None).await;
    assert_eq!(response.status(), StatusCode::OK);

    for (assignment_kind, assignment_id) in [
        ("data-assignments", data_assignment_id.as_str()),
        ("device-assignments", device_assignment_id.as_str()),
    ] {
        let response = request(
            &app,
            Method::DELETE,
            &format!("/api/v1/projects/project-logistics/{assignment_kind}/{assignment_id}"),
            None,
        )
        .await;
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
    }
}

#[tokio::test]
async fn failed_live_session_close_keeps_session_and_existing_sse_open() {
    let storage = tempfile::tempdir().expect("Temporary RMS storage is available");
    let state = rms_server::AppState::fixture_with_storage(storage.path())
        .expect("RMS fixture storage opens");
    let app = rms_server::router(state);
    let live_session = create_live_session(&app, "robot-07", "robot-07-source").await;
    let live_session_id = live_session["id"]
        .as_str()
        .expect("LiveSession response has an ID");

    let response = request(
        &app,
        Method::GET,
        &format!("/api/v1/live-sessions/{live_session_id}/events"),
        None,
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let mut event_stream = response.into_body().into_data_stream();
    let first_event = timeout(Duration::from_secs(1), event_stream.next())
        .await
        .expect("Live SSE emits without delay")
        .expect("Live SSE remains open")
        .expect("Live SSE body is readable");
    assert!(String::from_utf8_lossy(&first_event).contains("topic.value.changed"));

    std::fs::remove_dir_all(storage.path()).expect("Temporary storage can be made unavailable");
    let (status, _) = request_json(
        &app,
        Method::DELETE,
        &format!("/api/v1/live-sessions/{live_session_id}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);

    let (status, session) = request_json(
        &app,
        Method::GET,
        &format!("/api/v1/live-sessions/{live_session_id}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(session["status"], "open");

    let next_event = timeout(Duration::from_secs(2), event_stream.next())
        .await
        .expect("Live SSE continues after the failed close")
        .expect("Live SSE remains open after the failed close")
        .expect("Live SSE body remains readable");
    assert!(String::from_utf8_lossy(&next_event).contains("topic.value.changed"));
}

#[tokio::test]
async fn offline_device_replay_and_recording_immutability_are_preserved() {
    let app = rms_server::fixture_router();
    let (status, _) = request_json(
        &app,
        Method::POST,
        "/api/v1/live-sessions",
        Some(json!({
            "projectId": "project-logistics",
            "deviceId": "robot-21",
            "dataSourceId": "robot-21-source",
            "openedBy": "operator-01"
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);

    let (status, replay) = request_json(
        &app,
        Method::POST,
        "/api/v1/replay-sessions",
        Some(json!({
            "projectId": "project-logistics",
            "recordingId": "recording-robot-21-maintenance",
            "openedBy": "operator-01"
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(replay["status"], "open");

    let (_, before) = request_json(
        &app,
        Method::GET,
        "/api/v1/projects/project-logistics/workspace",
        None,
    )
    .await;
    let recording_before = fixture_recording(&before, "recording-robot-07-incident").clone();
    let assignment_id = before["deviceAssignments"]
        .as_array()
        .expect("Workspace device assignments are an array")
        .iter()
        .find(|assignment| assignment["deviceId"] == "robot-07")
        .and_then(|assignment| assignment["id"].as_str())
        .expect("Robot assignment exists")
        .to_owned();
    let response = request(
        &app,
        Method::DELETE,
        &format!("/api/v1/projects/project-logistics/device-assignments/{assignment_id}"),
        None,
    )
    .await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);

    let (_, after) = request_json(
        &app,
        Method::GET,
        "/api/v1/projects/project-logistics/workspace",
        None,
    )
    .await;
    let recording_after = fixture_recording(&after, "recording-robot-07-incident");
    assert_eq!(
        recording_after["manifestHash"],
        recording_before["manifestHash"]
    );
    assert_eq!(
        recording_after["projectSnapshot"],
        recording_before["projectSnapshot"]
    );
    let response = request(
        &app,
        Method::POST,
        "/api/v1/recordings/recording-robot-07-incident",
        Some(json!({ "manifestHash": "tampered" })),
    )
    .await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn rrd_and_sse_routes_are_local_and_streamable() {
    let app = rms_server::fixture_router();
    let session = create_live_session(&app, "robot-07", "robot-07-source").await;
    let session_id = session["id"]
        .as_str()
        .expect("LiveSession response has an ID");

    let response = request(
        &app,
        Method::GET,
        &format!("/rerun/live/{session_id}"),
        None,
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers().get(header::CONTENT_TYPE),
        Some(&"application/x-rerun".parse().expect("valid header"))
    );
    assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
    assert_eq!(response.headers()[header::ACCEPT_RANGES], "bytes");
    assert!(response.headers().get(header::ETAG).is_some());
    assert!(
        response
            .headers()
            .get(header::ACCESS_CONTROL_EXPOSE_HEADERS)
            .is_none()
    );
    assert!(
        response
            .headers()
            .get(header::ACCESS_CONTROL_ALLOW_ORIGIN)
            .is_none()
    );
    assert!(response.headers().get(header::LOCATION).is_none());
    let bytes = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("RRD body is readable");
    assert_eq!(
        bytes.as_ref(),
        include_bytes!("../../../../tests/assets/rrd/rms/rms_replay_50s_v0_36_1.rrd")
    );

    let response = request(
        &app,
        Method::GET,
        &format!("/api/v1/live-sessions/{session_id}/events"),
        None,
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert!(
        response.headers()[header::CONTENT_TYPE]
            .to_str()
            .expect("SSE content type is text")
            .starts_with("text/event-stream")
    );

    let response = request(
        &app,
        Method::GET,
        "/api/v1/projects/project-logistics/events",
        None,
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert!(
        response.headers()[header::CONTENT_TYPE]
            .to_str()
            .expect("SSE content type is text")
            .starts_with("text/event-stream")
    );
}

#[tokio::test]
async fn rrd_byte_ranges_etag_and_if_range_are_http_compliant() {
    let app = rms_server::fixture_router();
    let uri = "/rerun/fixture/rms-replay.rrd";
    let full = request(&app, Method::GET, uri, None).await;
    assert_eq!(full.status(), StatusCode::OK);
    assert_eq!(
        full.headers()[header::CACHE_CONTROL],
        "public, max-age=31536000, immutable"
    );
    let etag = full.headers()[header::ETAG]
        .to_str()
        .expect("ETag is text")
        .to_owned();
    let total_len = full.headers()[header::CONTENT_LENGTH]
        .to_str()
        .expect("Content-Length is text")
        .parse::<usize>()
        .expect("Content-Length is numeric");
    assert_eq!(total_len, 13_739);

    let partial = request_with_headers(
        &app,
        Method::GET,
        uri,
        None,
        &[(header::RANGE.as_str(), "bytes=10-19")],
    )
    .await;
    assert_eq!(partial.status(), StatusCode::PARTIAL_CONTENT);
    assert_eq!(
        partial.headers()[header::CONTENT_RANGE],
        "bytes 10-19/13739"
    );
    assert_eq!(partial.headers()[header::CONTENT_LENGTH], "10");
    let partial_bytes = to_bytes(partial.into_body(), usize::MAX)
        .await
        .expect("Partial RRD body is readable");
    assert_eq!(
        partial_bytes.as_ref(),
        &include_bytes!("../../../../tests/assets/rrd/rms/rms_replay_50s_v0_36_1.rrd")[10..20]
    );

    let if_range_match = request_with_headers(
        &app,
        Method::GET,
        uri,
        None,
        &[
            (header::RANGE.as_str(), "bytes=-8"),
            (header::IF_RANGE.as_str(), etag.as_str()),
        ],
    )
    .await;
    assert_eq!(if_range_match.status(), StatusCode::PARTIAL_CONTENT);
    assert_eq!(if_range_match.headers()[header::CONTENT_LENGTH], "8");

    let if_range_miss = request_with_headers(
        &app,
        Method::GET,
        uri,
        None,
        &[
            (header::RANGE.as_str(), "bytes=0-7"),
            (header::IF_RANGE.as_str(), "\"different-version\""),
        ],
    )
    .await;
    assert_eq!(if_range_miss.status(), StatusCode::OK);
    assert_eq!(if_range_miss.headers()[header::CONTENT_LENGTH], "13739");

    let unsatisfiable = request_with_headers(
        &app,
        Method::GET,
        uri,
        None,
        &[(header::RANGE.as_str(), "bytes=99999-")],
    )
    .await;
    assert_eq!(unsatisfiable.status(), StatusCode::RANGE_NOT_SATISFIABLE);
    assert_eq!(
        unsatisfiable.headers()[header::CONTENT_RANGE],
        "bytes */13739"
    );
    assert_eq!(unsatisfiable.headers()[header::CONTENT_LENGTH], "0");
}

async fn create_live_session(app: &Router, device_id: &str, data_source_id: &str) -> Value {
    let (status, session) = request_json(
        app,
        Method::POST,
        "/api/v1/live-sessions",
        Some(json!({
            "projectId": "project-logistics",
            "deviceId": device_id,
            "dataSourceId": data_source_id,
            "openedBy": "operator-01"
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    session
}

fn fixture_recording<'a>(workspace: &'a Value, recording_id: &str) -> &'a Value {
    workspace["recordings"]
        .as_array()
        .expect("Workspace recordings are an array")
        .iter()
        .find(|recording| recording["id"] == recording_id)
        .expect("Fixture recording exists")
}

async fn request_json(
    app: &Router,
    method: Method,
    uri: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let response = request(app, method, uri, body).await;
    let status = response.status();
    let bytes = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("Response body is readable");
    let value = serde_json::from_slice(&bytes).expect("Response body is valid JSON");
    (status, value)
}

async fn request(
    app: &Router,
    method: Method,
    uri: &str,
    body: Option<Value>,
) -> axum::response::Response {
    request_with_headers(app, method, uri, body, &[]).await
}

async fn request_with_headers(
    app: &Router,
    method: Method,
    uri: &str,
    body: Option<Value>,
    headers: &[(&str, &str)],
) -> axum::response::Response {
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header(header::HOST, "127.0.0.1:8080");
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    let body = if let Some(value) = body {
        builder = builder.header("content-type", "application/json");
        Body::from(value.to_string())
    } else {
        Body::empty()
    };
    let request = builder.body(body).expect("Request is valid");
    app.clone()
        .oneshot(request)
        .await
        .expect("Router serves the request")
}
