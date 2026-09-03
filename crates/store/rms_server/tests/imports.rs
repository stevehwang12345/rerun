use std::{fmt::Write as _, path::PathBuf, time::Duration};

use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Method, Request, StatusCode, header},
};
use serde_json::{Value, json};
use tokio::time::{sleep, timeout};
use tower::ServiceExt as _;

const FIXTURE_RRD: &[u8] =
    include_bytes!("../../../../tests/assets/rrd/rms/rms_replay_50s_v0_36_1.rrd");

#[tokio::test]
async fn rrd_import_survives_restart_and_serves_private_ranges() {
    let storage = tempfile::tempdir().expect("temporary storage is available");
    let state =
        rms_server::AppState::fixture_with_storage(storage.path()).expect("fixture storage opens");
    let app = rms_server::router(state.clone());

    let (status, import) = upload(&app, "rms replay.rrd", "rrd", FIXTURE_RRD, None, None).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{import}");
    assert_eq!(import["status"], "processing");
    assert_eq!(import["format"], "rrd");
    assert_eq!(import["projectId"], "project-logistics");
    assert_eq!(import["deviceId"], "robot-07");
    assert!(
        import["dataSourceId"]
            .as_str()
            .is_some_and(|id| id.starts_with("import-source-"))
    );

    let import_id = import["id"].as_str().expect("import ID");
    let source_range = request(
        &app,
        Method::GET,
        &format!("/api/v1/recording-imports/{import_id}/artifact"),
        Body::empty(),
        &[(header::RANGE.as_str(), "bytes=0-3")],
    )
    .await;
    assert_eq!(source_range.status(), StatusCode::PARTIAL_CONTENT);
    assert_eq!(
        source_range.headers()[header::CACHE_CONTROL],
        "private, no-store"
    );
    assert_eq!(
        source_range.headers()[header::CONTENT_RANGE],
        "bytes 0-3/13739"
    );
    assert_eq!(
        to_bytes(source_range.into_body(), usize::MAX)
            .await
            .expect("source range is readable")
            .as_ref(),
        b"RRF2"
    );

    let ready = wait_for_import(&app, import_id, &["ready"], Duration::from_secs(15)).await;
    let recording_id = ready["recordingId"]
        .as_str()
        .expect("ready import has a Recording")
        .to_owned();

    drop(app);
    drop(state);
    let restarted = rms_server::AppState::fixture_with_storage(storage.path())
        .expect("durable registry reopens");
    let app = rms_server::router(restarted);
    let recovered = request_json(
        &app,
        Method::GET,
        &format!("/api/v1/recording-imports/{import_id}"),
        None,
    )
    .await;
    assert_eq!(recovered.0, StatusCode::OK);
    assert_eq!(recovered.1["status"], "ready");
    assert_eq!(recovered.1["recordingId"], recording_id);

    let workspace = request_json(
        &app,
        Method::GET,
        "/api/v1/projects/project-logistics/workspace",
        None,
    )
    .await
    .1;
    let recording = workspace["recordings"]
        .as_array()
        .expect("recordings array")
        .iter()
        .find(|recording| recording["id"] == recording_id)
        .expect("imported Recording is in the workspace");
    assert_eq!(recording["defaultTimeline"], "tick");
    assert_eq!(recording["timelines"][0]["kind"], "sequence");
    assert_eq!(recording["timelines"][0]["start"], "0");
    assert_eq!(recording["timelines"][0]["end"], "100");
    assert_eq!(recording["footerVerified"], true);

    let recording_range = request(
        &app,
        Method::GET,
        &format!("/rerun/recordings/{recording_id}"),
        Body::empty(),
        &[(header::RANGE.as_str(), "bytes=10-19")],
    )
    .await;
    assert_eq!(recording_range.status(), StatusCode::PARTIAL_CONTENT);
    assert_eq!(
        recording_range.headers()[header::CACHE_CONTROL],
        "private, max-age=31536000, immutable"
    );
    assert_eq!(recording_range.headers()[header::ACCEPT_RANGES], "bytes");
    assert_eq!(
        to_bytes(recording_range.into_body(), usize::MAX)
            .await
            .expect("recording range is readable")
            .as_ref(),
        &FIXTURE_RRD[10..20]
    );

    let replay = request_json(
        &app,
        Method::POST,
        "/api/v1/replay-sessions",
        Some(json!({
            "projectId": "project-logistics",
            "recordingId": recording_id,
            "openedBy": "import-test"
        })),
    )
    .await;
    assert_eq!(replay.0, StatusCode::CREATED);
    assert_eq!(replay.1["initialTimeline"], "tick");
    assert_eq!(
        replay.1["initialCursor"],
        json!({"kind": "sequence", "value": "0"})
    );
    let replay_id = replay.1["id"].as_str().expect("Replay ID");
    assert_eq!(
        request_json(
            &app,
            Method::DELETE,
            &format!("/api/v1/recording-imports/{import_id}"),
            None,
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    assert_eq!(
        request_json(
            &app,
            Method::DELETE,
            &format!("/api/v1/replay-sessions/{replay_id}"),
            None,
        )
        .await
        .0,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        request_json(
            &app,
            Method::DELETE,
            &format!("/api/v1/recording-imports/{import_id}"),
            None,
        )
        .await
        .0,
        StatusCode::NO_CONTENT
    );
}

#[tokio::test]
async fn idempotency_receipt_survives_restart_and_rejects_changed_content() {
    let storage = tempfile::tempdir().expect("temporary storage is available");
    let state =
        rms_server::AppState::fixture_with_storage(storage.path()).expect("fixture storage opens");
    let app = rms_server::router(state.clone());
    let key = "rrd-retry-2026-08-20";
    let first = upload_with_idempotency(&app, key, FIXTURE_RRD).await;
    assert_eq!(first.0, StatusCode::ACCEPTED, "{}", first.1);
    let import_id = first.1["id"].as_str().expect("Import ID").to_owned();
    wait_for_import(&app, &import_id, &["ready"], Duration::from_secs(15)).await;

    drop(app);
    drop(state);
    let restarted =
        rms_server::AppState::fixture_with_storage(storage.path()).expect("registry reopens");
    let app = rms_server::router(restarted);
    let retry = upload_with_idempotency(&app, key, FIXTURE_RRD).await;
    assert_eq!(retry.0, StatusCode::ACCEPTED, "{}", retry.1);
    assert_eq!(retry.1["id"], import_id);
    let imports = request_json(
        &app,
        Method::GET,
        "/api/v1/recording-imports?projectId=project-logistics",
        None,
    )
    .await
    .1;
    assert_eq!(
        imports
            .as_array()
            .expect("imports array")
            .iter()
            .filter(|import| import["id"] == import_id)
            .count(),
        1
    );

    let mut changed = FIXTURE_RRD.to_vec();
    changed[20] ^= 1;
    let conflict = upload_with_idempotency(&app, key, &changed).await;
    assert_eq!(conflict.0, StatusCode::CONFLICT, "{}", conflict.1);
}

#[tokio::test]
async fn metadata_is_validated_before_file_bytes_and_failures_are_cleaned() {
    let storage = tempfile::tempdir().expect("temporary storage is available");
    let state = rms_server::AppState::fixture_with_storage_limit(storage.path(), 64)
        .expect("fixture storage opens");
    let app = rms_server::router(state);

    let (wire_body, wire_content_type) = multipart_body(&[
        ("projectId", None, b"project-logistics"),
        ("deviceId", None, b"robot-07"),
        ("format", None, b"rrd"),
        ("file", Some("small.rrd"), FIXTURE_RRD),
    ]);
    let wire_limited = request(
        &app,
        Method::POST,
        "/api/v1/recording-imports",
        Body::from(wire_body),
        &[
            (header::CONTENT_TYPE.as_str(), &wire_content_type),
            (header::CONTENT_LENGTH.as_str(), "600000000"),
        ],
    )
    .await;
    assert_eq!(wire_limited.status(), StatusCode::PAYLOAD_TOO_LARGE);
    assert_imports_dir_empty(storage.path());

    let (body, content_type) =
        multipart_body(&[("file", None, FIXTURE_RRD), ("projectId", None, b"missing")]);
    let response = request(
        &app,
        Method::POST,
        "/api/v1/recording-imports",
        Body::from(body),
        &[(header::CONTENT_TYPE.as_str(), &content_type)],
    )
    .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_imports_dir_empty(storage.path());

    let (status, error) = upload(
        &app,
        "capture.rrd",
        "rrd",
        FIXTURE_RRD,
        Some("missing-project"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{error}");
    assert_imports_dir_empty(storage.path());

    let (status, error) = upload_with_mapping(&app, "data.csv", "csv", b"value\n1\n", "[]").await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{error}");
    assert_imports_dir_empty(storage.path());

    let (status, error) = upload(&app, "too-large.rrd", "rrd", FIXTURE_RRD, None, None).await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE, "{error}");
    assert_imports_dir_empty(storage.path());
}

#[tokio::test]
async fn ready_import_bumps_workspace_event_version() {
    let storage = tempfile::tempdir().expect("temporary storage is available");
    let app = rms_server::router(
        rms_server::AppState::fixture_with_storage(storage.path()).expect("fixture storage opens"),
    );
    let response = request(
        &app,
        Method::GET,
        "/api/v1/projects/project-logistics/events",
        Body::empty(),
        &[],
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let mut events = response.into_body().into_data_stream();
    timeout(Duration::from_secs(2), events.next())
        .await
        .expect("initial workspace event is prompt")
        .expect("initial event exists")
        .expect("initial event is readable");

    let import = upload(&app, "workspace.rrd", "rrd", FIXTURE_RRD, None, None)
        .await
        .1;
    let import_id = import["id"].as_str().expect("import ID");
    wait_for_import(&app, import_id, &["ready"], Duration::from_secs(15)).await;
    let workspace = request_json(
        &app,
        Method::GET,
        "/api/v1/projects/project-logistics/workspace",
        None,
    )
    .await
    .1;
    let expected = format!("\"snapshotVersion\":{}", workspace["snapshotVersion"]);
    let saw_ready_version = timeout(Duration::from_secs(3), async {
        while let Some(chunk) = events.next().await {
            let chunk = chunk.expect("workspace event is readable");
            if String::from_utf8_lossy(&chunk).contains(&expected) {
                return true;
            }
        }
        false
    })
    .await
    .unwrap_or(false);
    assert!(saw_ready_version, "SSE did not publish {expected}");
}

#[tokio::test]
async fn cancellation_hides_and_cleans_artifacts_without_late_ready_commit() {
    let storage = tempfile::tempdir().expect("temporary storage is available");
    let app = rms_server::router(
        rms_server::AppState::fixture_with_storage(storage.path()).expect("fixture storage opens"),
    );
    let mut csv = String::with_capacity(8 * 1024 * 1024);
    csv.push_str("x,y\n");
    for row in 0..500_000 {
        writeln!(csv, "{row},{}", row + 1).expect("writing to a String cannot fail");
    }
    let (status, import) = upload(&app, "large.csv", "csv", csv.as_bytes(), None, None).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{import}");
    let import_id = import["id"].as_str().expect("import ID");
    let import_source_id = import["dataSourceId"].as_str().expect("import source ID");
    let workspace = request_json(
        &app,
        Method::GET,
        "/api/v1/projects/project-logistics/workspace",
        None,
    )
    .await
    .1;
    let device_assignment_id = workspace["deviceAssignments"]
        .as_array()
        .expect("device assignments array")
        .iter()
        .find(|assignment| assignment["deviceId"] == "robot-07")
        .and_then(|assignment| assignment["id"].as_str())
        .expect("Robot assignment");
    let data_assignment_id = workspace["dataAssignments"]
        .as_array()
        .expect("data assignments array")
        .iter()
        .find(|assignment| assignment["dataSourceId"] == import_source_id)
        .and_then(|assignment| assignment["id"].as_str())
        .expect("Import source assignment");
    assert_eq!(
        request_json(
            &app,
            Method::DELETE,
            &format!(
                "/api/v1/projects/project-logistics/device-assignments/{device_assignment_id}"
            ),
            None,
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    assert_eq!(
        request_json(
            &app,
            Method::DELETE,
            &format!("/api/v1/projects/project-logistics/data-assignments/{data_assignment_id}"),
            None,
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    let cancelled = request_json(
        &app,
        Method::DELETE,
        &format!("/api/v1/recording-imports/{import_id}"),
        None,
    )
    .await;
    assert_eq!(cancelled.0, StatusCode::NO_CONTENT, "{}", cancelled.1);

    let hidden = request_json(
        &app,
        Method::GET,
        &format!("/api/v1/recording-imports/{import_id}/artifact"),
        None,
    )
    .await;
    assert_eq!(hidden.0, StatusCode::GONE, "{}", hidden.1);

    let tombstone = timeout(Duration::from_secs(15), async {
        loop {
            let import = request_json(
                &app,
                Method::GET,
                &format!("/api/v1/recording-imports/{import_id}"),
                None,
            )
            .await
            .1;
            assert_ne!(import["status"], "ready");
            if import["status"] == "cancelled" && import["artifactUrl"].is_null() {
                return import;
            }
            sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("worker acknowledges cancellation");
    assert!(tombstone["recordingId"].is_null());
    assert_imports_dir_empty(storage.path());

    let deleted = request_json(
        &app,
        Method::DELETE,
        &format!("/api/v1/recording-imports/{import_id}"),
        None,
    )
    .await;
    assert_eq!(deleted.0, StatusCode::NO_CONTENT, "{}", deleted.1);
}

#[tokio::test]
async fn output_quota_failure_releases_source_bytes_for_retry() {
    let storage = tempfile::tempdir().expect("temporary storage is available");
    let app = rms_server::router(
        rms_server::AppState::fixture_with_storage_limits(storage.path(), 20_000, 30_000)
            .expect("fixture storage opens"),
    );

    for attempt in 0..2 {
        let (status, import) = upload(
            &app,
            &format!("quota-{attempt}.rrd"),
            "rrd",
            FIXTURE_RRD,
            None,
            None,
        )
        .await;
        assert_eq!(status, StatusCode::ACCEPTED, "{import}");
        let import_id = import["id"].as_str().expect("import ID");
        let failed = wait_for_import(&app, import_id, &["failed"], Duration::from_secs(5)).await;
        assert!(
            failed["failureReason"]
                .as_str()
                .is_some_and(|reason| reason.contains("enough space")),
            "{failed}"
        );
        let deleted = timeout(Duration::from_secs(5), async {
            loop {
                let deleted = request_json(
                    &app,
                    Method::DELETE,
                    &format!("/api/v1/recording-imports/{import_id}"),
                    None,
                )
                .await;
                if deleted.0 != StatusCode::CONFLICT {
                    break deleted;
                }
                sleep(Duration::from_millis(25)).await;
            }
        })
        .await
        .expect("failed worker finishes cleanup");
        assert_eq!(deleted.0, StatusCode::NO_CONTENT, "{}", deleted.1);
    }
}

#[tokio::test]
async fn video_import_preserves_duration_nanoseconds_in_replay_contract() {
    let storage = tempfile::tempdir().expect("temporary storage is available");
    let app = rms_server::router(
        rms_server::AppState::fixture_with_storage(storage.path()).expect("fixture storage opens"),
    );
    let video =
        std::fs::read(workspace_root().join("tests/assets/video/Big_Buck_Bunny_1080_1s_h264.mp4"))
            .expect("video fixture is readable");
    let (status, import) = upload(&app, "camera.mp4", "video", &video, None, None).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{import}");
    let import_id = import["id"].as_str().expect("import ID");
    let ready = wait_for_import(
        &app,
        import_id,
        &["ready", "failed"],
        Duration::from_secs(15),
    )
    .await;
    assert_eq!(ready["status"], "ready", "{ready}");
    let recording_id = ready["recordingId"].as_str().expect("Recording ID");
    let workspace = request_json(
        &app,
        Method::GET,
        "/api/v1/projects/project-logistics/workspace",
        None,
    )
    .await
    .1;
    let recording = workspace["recordings"]
        .as_array()
        .expect("recordings array")
        .iter()
        .find(|recording| recording["id"] == recording_id)
        .expect("video Recording exists");
    let timeline = recording["timelines"]
        .as_array()
        .expect("timelines array")
        .iter()
        .find(|timeline| timeline["kind"] == "duration")
        .expect("DurationNs timeline is preserved");
    assert!(timeline["start"].as_str().is_some());
    assert!(timeline["end"].as_str().is_some());

    let replay = request_json(
        &app,
        Method::POST,
        "/api/v1/replay-sessions",
        Some(json!({
            "projectId": "project-logistics",
            "recordingId": recording_id,
            "openedBy": "duration-test"
        })),
    )
    .await;
    assert_eq!(replay.0, StatusCode::CREATED, "{}", replay.1);
    assert_eq!(replay.1["initialCursor"]["kind"], "duration");
    assert_eq!(replay.1["initialCursor"]["value"], timeline["start"]);
}

#[tokio::test]
async fn csv_import_materializes_real_entity_paths_without_placeholder_topics() {
    let storage = tempfile::tempdir().expect("temporary storage is available");
    let app = rms_server::router(
        rms_server::AppState::fixture_with_storage(storage.path()).expect("fixture storage opens"),
    );
    let csv = b"temperature,rear_camera_signal\n21.5,1\n22.0,0\n";
    let (status, import) = upload(&app, "telemetry.csv", "csv", csv, None, None).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{import}");
    let import_id = import["id"].as_str().expect("Import ID");
    let source_id = import["dataSourceId"]
        .as_str()
        .expect("import DataSource ID");
    let ready = wait_for_import(&app, import_id, &["ready"], Duration::from_secs(15)).await;
    let workspace = request_json(
        &app,
        Method::GET,
        "/api/v1/projects/project-logistics/workspace",
        None,
    )
    .await
    .1;
    let topics = workspace["topicsByDataSource"][source_id]
        .as_array()
        .expect("import topics are materialized");
    assert!(!topics.is_empty(), "{workspace}");
    assert!(topics.iter().all(|topic| {
        topic["path"]
            .as_str()
            .is_some_and(|path| path.starts_with("/csv/"))
            && topic["path"] != "/planning/status"
            && topic["message"].is_null()
            && topic["id"]
                .as_str()
                .is_some_and(|id| id.starts_with("topic-"))
    }));
    let recording_id = ready["recordingId"].as_str().expect("Recording ID");
    let recording = workspace["recordings"]
        .as_array()
        .expect("recordings array")
        .iter()
        .find(|recording| recording["id"] == recording_id)
        .expect("CSV Recording exists");
    let topic_ids = recording["topicIds"]
        .as_array()
        .expect("Recording topic IDs");
    assert_eq!(topic_ids.len(), topics.len());
    assert!(
        topic_ids
            .iter()
            .all(|id| topics.iter().any(|topic| topic["id"] == *id))
    );
}

#[tokio::test]
async fn recording_topic_snapshots_survive_later_imports_and_restart() {
    let storage = tempfile::tempdir().expect("temporary storage is available");
    let state =
        rms_server::AppState::fixture_with_storage(storage.path()).expect("fixture storage opens");
    let app = rms_server::router(state.clone());

    let (first_status, first_import) = upload(
        &app,
        "thermal.csv",
        "csv",
        b"temperature\n21.5\n",
        None,
        None,
    )
    .await;
    assert_eq!(first_status, StatusCode::ACCEPTED, "{first_import}");
    let first_import_id = first_import["id"].as_str().expect("first import ID");
    let source_id = first_import["dataSourceId"]
        .as_str()
        .expect("first DataSource ID")
        .to_owned();
    let first_ready =
        wait_for_import(&app, first_import_id, &["ready"], Duration::from_secs(15)).await;
    let first_recording_id = first_ready["recordingId"]
        .as_str()
        .expect("first Recording ID")
        .to_owned();
    let first_workspace = request_json(
        &app,
        Method::GET,
        "/api/v1/projects/project-logistics/workspace",
        None,
    )
    .await
    .1;
    let first_topics = first_workspace["topicsByRecording"][&first_recording_id]
        .as_array()
        .expect("first Recording has an immutable topic snapshot")
        .clone();
    assert_eq!(first_topics.len(), 1, "{first_workspace}");
    assert_eq!(first_topics[0]["path"], "/csv/temperature");

    let (second_status, second_import) = upload(
        &app,
        "rear-camera.csv",
        "csv",
        b"rear_camera_signal\n1\n",
        None,
        None,
    )
    .await;
    assert_eq!(second_status, StatusCode::ACCEPTED, "{second_import}");
    assert_eq!(second_import["dataSourceId"], source_id);
    let second_ready = wait_for_import(
        &app,
        second_import["id"].as_str().expect("second import ID"),
        &["ready"],
        Duration::from_secs(15),
    )
    .await;
    let second_recording_id = second_ready["recordingId"]
        .as_str()
        .expect("second Recording ID")
        .to_owned();
    let second_workspace = request_json(
        &app,
        Method::GET,
        "/api/v1/projects/project-logistics/workspace",
        None,
    )
    .await
    .1;
    assert_eq!(
        second_workspace["topicsByRecording"][&first_recording_id],
        Value::Array(first_topics.clone()),
        "a later import must not mutate the first Recording snapshot"
    );
    assert_eq!(
        second_workspace["topicsByRecording"][&second_recording_id][0]["path"],
        "/csv/rear_camera_signal"
    );
    assert_eq!(
        second_workspace["topicsByDataSource"][&source_id][0]["path"], "/csv/rear_camera_signal",
        "the mutable source projection should still describe the latest import"
    );

    drop(app);
    drop(state);
    let registry_path = storage.path().join("registry.json");
    let mut legacy_registry: Value = serde_json::from_slice(
        &std::fs::read(&registry_path).expect("durable registry is readable"),
    )
    .expect("durable registry is JSON");
    assert!(
        legacy_registry["topicsByRecording"]
            .as_object()
            .is_some_and(|snapshots| snapshots.contains_key(&first_recording_id)),
        "new imports must persist Recording topic snapshots"
    );
    legacy_registry["schemaVersion"] = json!(2);
    legacy_registry
        .as_object_mut()
        .expect("registry object")
        .remove("topicsByRecording");
    std::fs::write(
        &registry_path,
        serde_json::to_vec(&legacy_registry).expect("legacy registry encodes"),
    )
    .expect("legacy registry is written");

    let restarted_state = rms_server::AppState::fixture_with_storage(storage.path())
        .expect("legacy durable registry reopens and migrates");
    let restarted = rms_server::router(restarted_state.clone());
    let restarted_workspace = request_json(
        &restarted,
        Method::GET,
        "/api/v1/projects/project-logistics/workspace",
        None,
    )
    .await
    .1;
    assert_eq!(
        restarted_workspace["topicsByRecording"][&first_recording_id],
        Value::Array(first_topics.clone()),
        "the first immutable topic snapshot must survive restart"
    );
    assert_eq!(
        restarted_workspace["topicsByRecording"][&second_recording_id][0]["path"],
        "/csv/rear_camera_signal"
    );

    drop(restarted);
    drop(restarted_state);
    let migrated_registry: Value = serde_json::from_slice(
        &std::fs::read(&registry_path).expect("migrated registry is readable"),
    )
    .expect("migrated registry is JSON");
    assert_eq!(migrated_registry["schemaVersion"], 6);
    assert_eq!(
        migrated_registry["topicsByRecording"][&first_recording_id],
        Value::Array(first_topics),
        "RRD-derived snapshots must be persisted by the one-way migration"
    );
}

#[tokio::test]
async fn mcap_import_materializes_source_scoped_workspace_topics() {
    let storage = tempfile::tempdir().expect("temporary storage is available");
    let app = rms_server::router(
        rms_server::AppState::fixture_with_storage(storage.path()).expect("fixture storage opens"),
    );
    let mcap = std::fs::read(
        workspace_root().join("crates/store/re_importer/tests/assets/supported_ros2_messages.mcap"),
    )
    .expect("MCAP fixture is readable");
    let (status, import) = upload(
        &app,
        "supported_ros2_messages.mcap",
        "mcap",
        &mcap,
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{import}");
    let import_id = import["id"].as_str().expect("Import ID");
    let source_id = import["dataSourceId"]
        .as_str()
        .expect("import DataSource ID");
    let ready = wait_for_import(&app, import_id, &["ready"], Duration::from_secs(30)).await;
    let workspace = request_json(
        &app,
        Method::GET,
        "/api/v1/projects/project-logistics/workspace",
        None,
    )
    .await
    .1;
    let topics = workspace["topicsByDataSource"][source_id]
        .as_array()
        .expect("MCAP topics are materialized");
    assert!(!topics.is_empty(), "{workspace}");
    assert!(topics.iter().all(|topic| {
        topic["path"]
            .as_str()
            .is_some_and(|path| path.starts_with('/') && path != "/planning/status")
            && topic["id"]
                .as_str()
                .is_some_and(|id| id.starts_with("topic-"))
            && topic["message"].is_null()
    }));
    let recording_id = ready["recordingId"].as_str().expect("Recording ID");
    let recording = workspace["recordings"]
        .as_array()
        .expect("recordings array")
        .iter()
        .find(|recording| recording["id"] == recording_id)
        .expect("MCAP Recording exists");
    assert_eq!(
        recording["topicIds"].as_array().map(Vec::len),
        Some(topics.len())
    );
}

#[tokio::test]
async fn physical_robot_mcap_imports_to_ready_recording_and_range_replay() {
    let storage = tempfile::tempdir().expect("temporary storage is available");
    let app = rms_server::router(
        rms_server::AppState::fixture_with_storage(storage.path()).expect("fixture storage opens"),
    );

    // This is a checked-in excerpt of NVIDIA's live-recorded r2b Galileo robot dataset.
    // Read it at runtime so the 19 MB physical recording is not embedded in the test binary.
    let mcap = std::fs::read(workspace_root().join("tests/assets/mcap/r2b_galileo.mcap"))
        .expect("physical r2b Galileo MCAP is readable");
    let (status, import) = upload(&app, "r2b_galileo.mcap", "mcap", &mcap, None, None).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{import}");
    assert_eq!(import["status"], "processing", "{import}");
    let import_id = import["id"].as_str().expect("Import ID");
    let source_id = import["dataSourceId"]
        .as_str()
        .expect("import DataSource ID");
    let ready = wait_for_import(
        &app,
        import_id,
        &["ready", "failed"],
        Duration::from_mins(1),
    )
    .await;
    let failure_detail = if ready["status"] == "failed" {
        sleep(Duration::from_millis(100)).await;
        std::fs::read(storage.path().join("registry.json"))
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
            .and_then(|registry| {
                registry["importArtifacts"][import_id]["internalDetail"]
                    .as_str()
                    .map(str::to_owned)
            })
            .unwrap_or_else(|| "internal import detail is unavailable".to_owned())
    } else {
        String::new()
    };
    assert_eq!(
        ready["status"], "ready",
        "physical MCAP import failed: {ready}\nInternal detail: {failure_detail}"
    );
    assert_eq!(
        ready["sourceSha256"], "6c65717c9e45cdcb397f8bd40055afcd6b761dee838724a2052c91ca60611cf5",
        "the imported bytes must be the reviewed physical r2b excerpt"
    );

    let recording_id = ready["recordingId"].as_str().expect("Recording ID");
    let workspace = request_json(
        &app,
        Method::GET,
        "/api/v1/projects/project-logistics/workspace",
        None,
    )
    .await
    .1;
    let topics = workspace["topicsByDataSource"][source_id]
        .as_array()
        .expect("physical MCAP topics are materialized");
    for expected_path in [
        "/front_stereo_camera/left/image_compressed",
        "/front_stereo_imu/imu",
        "/chassis/battery_state",
    ] {
        assert!(
            topics.iter().any(|topic| topic["path"] == expected_path),
            "physical sensor topic {expected_path} is missing: {topics:?}"
        );
    }
    assert!(
        topics.iter().all(|topic| topic["message"].is_null()),
        "imported topics must come from recording metadata, not placeholder messages"
    );

    let recording = workspace["recordings"]
        .as_array()
        .expect("recordings array")
        .iter()
        .find(|recording| recording["id"] == recording_id)
        .expect("physical MCAP Recording exists");
    assert_eq!(recording["status"], "ready", "{recording}");
    assert_eq!(recording["footerVerified"], true, "{recording}");
    assert_eq!(recording["defaultTimeline"], "message_log_time");
    assert!(
        recording["rrdVersion"]
            .as_str()
            .is_some_and(|version| !version.is_empty()),
        "converted Recording must expose its RRD version: {recording}"
    );
    let content_sha256 = recording["contentSha256"]
        .as_str()
        .expect("converted Recording has a content hash");
    assert_eq!(content_sha256.len(), 64, "{recording}");
    assert_eq!(
        recording["manifestHash"],
        format!("sha256:{content_sha256}"),
        "the Replay manifest must bind the verified RRD"
    );
    let timeline = recording["timelines"]
        .as_array()
        .expect("timelines array")
        .iter()
        .find(|timeline| timeline["name"] == "message_log_time")
        .expect("MCAP message log timeline is preserved");
    assert_eq!(timeline["kind"], "timestamp", "{timeline}");
    let timeline_start = timeline["start"]
        .as_str()
        .expect("timestamp start is lossless text")
        .parse::<i64>()
        .expect("timestamp start is an integer");
    let timeline_end = timeline["end"]
        .as_str()
        .expect("timestamp end is lossless text")
        .parse::<i64>()
        .expect("timestamp end is an integer");
    assert!(timeline_start < timeline_end, "{timeline}");
    assert!(
        recording["durationSeconds"]
            .as_f64()
            .is_some_and(|duration| duration > 0.0),
        "physical Recording must have a nonzero duration: {recording}"
    );

    let replay = request_json(
        &app,
        Method::POST,
        "/api/v1/replay-sessions",
        Some(json!({
            "projectId": "project-logistics",
            "recordingId": recording_id,
            "openedBy": "physical-robot-import-test"
        })),
    )
    .await;
    assert_eq!(replay.0, StatusCode::CREATED, "{}", replay.1);
    assert_eq!(replay.1["initialTimeline"], "message_log_time");
    assert_eq!(replay.1["initialCursor"]["kind"], "timestamp");
    assert_eq!(replay.1["initialCursor"]["value"], timeline["start"]);
    let replay_url = replay.1["streamUrl"]
        .as_str()
        .expect("Replay session has a stream URL");
    let replay_range = request(
        &app,
        Method::GET,
        replay_url,
        Body::empty(),
        &[(header::RANGE.as_str(), "bytes=0-15")],
    )
    .await;
    assert_eq!(replay_range.status(), StatusCode::PARTIAL_CONTENT);
    assert_eq!(replay_range.headers()[header::ACCEPT_RANGES], "bytes");
    assert!(
        replay_range.headers()[header::CONTENT_RANGE]
            .to_str()
            .is_ok_and(|value| value.starts_with("bytes 0-15/")),
        "Replay range must describe the verified Recording"
    );
    let replay_bytes = to_bytes(replay_range.into_body(), usize::MAX)
        .await
        .expect("Replay range is readable");
    assert_eq!(replay_bytes.len(), 16);
    assert_eq!(&replay_bytes[..4], b"RRF2");
}

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..")
}

#[tokio::test]
async fn catalog_assignments_and_live_recording_survive_restart() {
    let storage = tempfile::tempdir().expect("temporary storage is available");
    let state =
        rms_server::AppState::fixture_with_storage(storage.path()).expect("fixture storage opens");
    let app = rms_server::router(state.clone());

    let integration = request_json(
        &app,
        Method::POST,
        "/api/v1/integrations",
        Some(json!({
            "organizationId": "org-rms",
            "name": "Durable ROS 2",
            "kind": "ros2",
            "endpointLabel": "Restart test"
        })),
    )
    .await;
    assert_eq!(integration.0, StatusCode::CREATED, "{}", integration.1);
    let integration_id = integration.1["id"].as_str().expect("Integration ID");
    let device = request_json(
        &app,
        Method::POST,
        "/api/v1/devices",
        Some(json!({
            "id": "durable-robot",
            "organizationId": "org-rms",
            "integrationId": integration_id,
            "name": "Durable Robot",
            "kind": "robot",
            "status": "online",
            "health": "normal"
        })),
    )
    .await;
    assert_eq!(device.0, StatusCode::CREATED, "{}", device.1);
    let source = request_json(
        &app,
        Method::POST,
        "/api/v1/data-sources",
        Some(json!({
            "id": "durable-source",
            "integrationId": integration_id,
            "deviceId": "durable-robot",
            "name": "Durable telemetry",
            "protocol": "ROS 2 + Rerun",
            "status": "recording",
            "liveUrl": "/rerun/fixture/rms-replay.rrd",
            "topicIds": ["sensors/custom_temperature", "camera/rear/image"]
        })),
    )
    .await;
    assert_eq!(source.0, StatusCode::CREATED, "{}", source.1);
    let project = request_json(
        &app,
        Method::POST,
        "/api/v1/projects",
        Some(json!({
            "organizationId": "org-rms",
            "name": "Durable project",
            "deviceIds": ["durable-robot"],
            "dataSourceIds": ["durable-source"]
        })),
    )
    .await;
    assert_eq!(project.0, StatusCode::CREATED, "{}", project.1);
    let project_id = project.1["id"].as_str().expect("Project ID").to_owned();
    let live = request_json(
        &app,
        Method::POST,
        "/api/v1/live-sessions",
        Some(json!({
            "projectId": project_id,
            "deviceId": "durable-robot",
            "dataSourceId": "durable-source",
            "openedBy": "restart-test"
        })),
    )
    .await;
    assert_eq!(live.0, StatusCode::CREATED, "{}", live.1);
    let live_id = live.1["id"].as_str().expect("LiveSession ID");
    let closed = request_json(
        &app,
        Method::DELETE,
        &format!("/api/v1/live-sessions/{live_id}"),
        None,
    )
    .await;
    assert_eq!(closed.0, StatusCode::OK, "{}", closed.1);
    let recording_id = closed.1["id"].as_str().expect("Recording ID").to_owned();
    let workspace = request_json(
        &app,
        Method::GET,
        &format!("/api/v1/projects/{project_id}/workspace"),
        None,
    )
    .await
    .1;
    let data_assignment_id = workspace["dataAssignments"][0]["id"]
        .as_str()
        .expect("DataAssignment ID");
    let device_assignment_id = workspace["deviceAssignments"][0]["id"]
        .as_str()
        .expect("DeviceAssignment ID");
    assert_eq!(
        request_json(
            &app,
            Method::DELETE,
            &format!("/api/v1/projects/{project_id}/data-assignments/{data_assignment_id}"),
            None,
        )
        .await
        .0,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        request_json(
            &app,
            Method::DELETE,
            &format!("/api/v1/projects/{project_id}/device-assignments/{device_assignment_id}"),
            None,
        )
        .await
        .0,
        StatusCode::NO_CONTENT
    );

    drop(app);
    drop(state);
    let restarted =
        rms_server::AppState::fixture_with_storage(storage.path()).expect("registry reopens");
    let app = rms_server::router(restarted);
    let workspace = request_json(
        &app,
        Method::GET,
        &format!("/api/v1/projects/{project_id}/workspace"),
        None,
    )
    .await;
    assert_eq!(workspace.0, StatusCode::OK, "{}", workspace.1);
    assert_eq!(workspace.1["deviceAssignments"], json!([]));
    assert_eq!(workspace.1["dataAssignments"], json!([]));
    assert!(
        workspace.1["recordings"]
            .as_array()
            .is_some_and(|recordings| recordings.iter().any(|item| item["id"] == recording_id))
    );
    let topics = request_json(
        &app,
        Method::GET,
        "/api/v1/data-sources/durable-source/topics",
        None,
    )
    .await;
    assert_eq!(topics.0, StatusCode::OK, "{}", topics.1);
    assert!(topics.1.as_array().is_some_and(|topics| {
        topics
            .iter()
            .any(|topic| topic["path"] == "/sensors/custom_temperature")
            && topics
                .iter()
                .any(|topic| topic["path"] == "/camera/rear/image")
    }));
}

async fn upload(
    app: &Router,
    file_name: &str,
    format: &str,
    bytes: &[u8],
    project_id: Option<&str>,
    data_source_id: Option<&str>,
) -> (StatusCode, Value) {
    let project_id = project_id.unwrap_or("project-logistics");
    let mut fields = vec![
        ("projectId", None, project_id.as_bytes()),
        ("deviceId", None, b"robot-07".as_slice()),
        ("format", None, format.as_bytes()),
    ];
    if let Some(data_source_id) = data_source_id {
        fields.push(("dataSourceId", None, data_source_id.as_bytes()));
    }
    fields.push(("file", Some(file_name), bytes));
    upload_fields(app, &fields).await
}

async fn upload_with_mapping(
    app: &Router,
    file_name: &str,
    format: &str,
    bytes: &[u8],
    mapping: &str,
) -> (StatusCode, Value) {
    upload_fields(
        app,
        &[
            ("projectId", None, b"project-logistics"),
            ("deviceId", None, b"robot-07"),
            ("format", None, format.as_bytes()),
            ("mapping", None, mapping.as_bytes()),
            ("file", Some(file_name), bytes),
        ],
    )
    .await
}

async fn upload_with_idempotency(
    app: &Router,
    idempotency_key: &str,
    bytes: &[u8],
) -> (StatusCode, Value) {
    let (body, content_type) = multipart_body(&[
        ("projectId", None, b"project-logistics"),
        ("deviceId", None, b"robot-07"),
        ("format", None, b"rrd"),
        ("file", Some("retry.rrd"), bytes),
    ]);
    let response = request(
        app,
        Method::POST,
        "/api/v1/recording-imports",
        Body::from(body),
        &[
            (header::CONTENT_TYPE.as_str(), &content_type),
            ("idempotency-key", idempotency_key),
        ],
    )
    .await;
    let status = response.status();
    let bytes = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("response body is readable");
    (
        status,
        serde_json::from_slice(&bytes).expect("response body is JSON"),
    )
}

async fn upload_fields(
    app: &Router,
    fields: &[(&str, Option<&str>, &[u8])],
) -> (StatusCode, Value) {
    let (body, content_type) = multipart_body(fields);
    let response = request(
        app,
        Method::POST,
        "/api/v1/recording-imports",
        Body::from(body),
        &[(header::CONTENT_TYPE.as_str(), &content_type)],
    )
    .await;
    let status = response.status();
    let bytes = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("response body is readable");
    let json = serde_json::from_slice(&bytes)
        .unwrap_or_else(|_| panic!("response was not JSON: {}", String::from_utf8_lossy(&bytes)));
    (status, json)
}

fn multipart_body(fields: &[(&str, Option<&str>, &[u8])]) -> (Vec<u8>, String) {
    let boundary = "rms-import-test-boundary";
    let mut body = Vec::new();
    for (name, file_name, value) in fields {
        body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
        if let Some(file_name) = file_name {
            body.extend_from_slice(
                format!(
                    "Content-Disposition: form-data; name=\"{name}\"; filename=\"{file_name}\"\r\n\r\n"
                )
                .as_bytes(),
            );
        } else {
            body.extend_from_slice(
                format!("Content-Disposition: form-data; name=\"{name}\"\r\n\r\n").as_bytes(),
            );
        }
        body.extend_from_slice(value);
        body.extend_from_slice(b"\r\n");
    }
    body.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
    (body, format!("multipart/form-data; boundary={boundary}"))
}

async fn wait_for_import(
    app: &Router,
    import_id: &str,
    terminal_statuses: &[&str],
    deadline: Duration,
) -> Value {
    timeout(deadline, async {
        loop {
            let (status, import) = request_json(
                app,
                Method::GET,
                &format!("/api/v1/recording-imports/{import_id}"),
                None,
            )
            .await;
            assert_eq!(status, StatusCode::OK, "{import}");
            if import["status"]
                .as_str()
                .is_some_and(|status| terminal_statuses.contains(&status))
            {
                return import;
            }
            sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("import reached a terminal state")
}

fn assert_imports_dir_empty(storage: &std::path::Path) {
    let count = std::fs::read_dir(storage.join("imports"))
        .expect("imports directory exists")
        .count();
    assert_eq!(count, 0, "partial upload directory was not cleaned");
}

async fn request_json(
    app: &Router,
    method: Method,
    uri: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let (body, headers) = if let Some(body) = body {
        (
            Body::from(body.to_string()),
            vec![(header::CONTENT_TYPE.as_str(), "application/json")],
        )
    } else {
        (Body::empty(), Vec::new())
    };
    let response = request(app, method, uri, body, &headers).await;
    let status = response.status();
    if status == StatusCode::NO_CONTENT {
        return (status, Value::Null);
    }
    let bytes = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("response body is readable");
    (
        status,
        serde_json::from_slice(&bytes).expect("response body is JSON"),
    )
}

async fn request(
    app: &Router,
    method: Method,
    uri: &str,
    body: Body,
    headers: &[(&str, &str)],
) -> axum::response::Response {
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header(header::HOST, "127.0.0.1:8080");
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    app.clone()
        .oneshot(builder.body(body).expect("request is valid"))
        .await
        .expect("request succeeds")
}

use tokio_stream::StreamExt as _;
