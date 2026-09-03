use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use async_trait::async_trait;
use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Method, Request, StatusCode, header},
};
use serde_json::{Value, json};
use tokio::sync::Semaphore;
use tower::ServiceExt as _;

use rms_server::{
    DiscoveryCancellation, DiscoveryObservation, DiscoveryProvider, DiscoveryProviderError,
    ProviderVerification, ProviderVerificationStatus,
};

const ORGANIZATION_ID: &str = "org-rms";

#[derive(Clone)]
struct BlockingVerificationProvider {
    observation: DiscoveryObservation,
    verification_calls: Arc<AtomicUsize>,
    verification_started: Arc<Semaphore>,
    verification_release: Arc<Semaphore>,
}

impl BlockingVerificationProvider {
    fn new(observation: DiscoveryObservation) -> Self {
        Self {
            observation,
            verification_calls: Arc::new(AtomicUsize::new(0)),
            verification_started: Arc::new(Semaphore::new(0)),
            verification_release: Arc::new(Semaphore::new(0)),
        }
    }

    async fn wait_until_verification_started(&self) {
        let permit =
            tokio::time::timeout(Duration::from_secs(1), self.verification_started.acquire())
                .await
                .expect("verification provider should be called")
                .expect("verification start semaphore should remain open");
        permit.forget();
    }

    fn release_one_verification(&self) {
        self.verification_release.add_permits(1);
    }

    fn verification_call_count(&self) -> usize {
        self.verification_calls.load(Ordering::Acquire)
    }
}

#[async_trait]
impl DiscoveryProvider for BlockingVerificationProvider {
    async fn discover(
        &self,
        cancellation: DiscoveryCancellation,
    ) -> Result<Vec<DiscoveryObservation>, DiscoveryProviderError> {
        if cancellation.is_cancelled() {
            Ok(Vec::new())
        } else {
            Ok(vec![self.observation.clone()])
        }
    }

    async fn verify(
        &self,
        candidate: &DiscoveryObservation,
        cancellation: DiscoveryCancellation,
    ) -> Result<ProviderVerification, DiscoveryProviderError> {
        self.verification_calls.fetch_add(1, Ordering::AcqRel);
        self.verification_started.add_permits(1);
        tokio::select! {
            permit = self.verification_release.acquire() => {
                permit
                    .expect("verification release semaphore should remain open")
                    .forget();
                let observation = (candidate.fingerprint == self.observation.fingerprint)
                    .then(|| self.observation.clone());
                Ok(ProviderVerification {
                    status: if observation.is_some() {
                        ProviderVerificationStatus::Verified
                    } else {
                        ProviderVerificationStatus::Unavailable
                    },
                    observation,
                })
            }
            () = wait_until_cancelled(cancellation) => Ok(ProviderVerification {
                status: ProviderVerificationStatus::Unavailable,
                observation: None,
            }),
        }
    }
}

#[tokio::test]
async fn discovery_is_sanitized_and_approval_is_atomic_observe_only() {
    let state = rms_server::AppState::fixture();
    let app = rms_server::router(state);
    let before_devices = request_json(&app, Method::GET, "/api/v1/devices", None, None)
        .await
        .1;

    let session = start_session(&app, "discovery-start-atomic").await;
    let snapshot = wait_until_ready(&app, session["id"].as_str().unwrap()).await;
    assert_eq!(snapshot["session"]["status"], "ready");
    assert_eq!(snapshot["session"]["candidateCount"], 1);
    let candidate = &snapshot["candidates"][0];
    for field in [
        "id",
        "sessionId",
        "displayName",
        "category",
        "status",
        "lastSeenAt",
        "sourceCount",
        "supportsLive",
    ] {
        assert!(
            candidate.get(field).is_some(),
            "missing {field}: {candidate}"
        );
    }
    for secret in [
        "endpoint",
        "endpoints",
        "ip",
        "address",
        "port",
        "protocol",
        "liveUrl",
    ] {
        assert!(
            candidate.get(secret).is_none(),
            "leaked {secret}: {candidate}"
        );
    }

    let devices_before_approval = request_json(&app, Method::GET, "/api/v1/devices", None, None)
        .await
        .1;
    assert_eq!(devices_before_approval, before_devices);

    let session_id = session["id"].as_str().unwrap();
    let candidate_id = candidate["id"].as_str().unwrap();
    let verification = request_json(
        &app,
        Method::POST,
        &format!(
            "/api/v1/network-discovery-sessions/{session_id}/candidates/{candidate_id}/verification"
        ),
        None,
        Some("verify-atomic"),
    )
    .await;
    assert_eq!(verification.0, StatusCode::OK, "{}", verification.1);
    assert_eq!(verification.1["status"], "verified");
    assert_eq!(verification.1["suggestedDevice"]["kind"], "robot");
    assert_eq!(verification.1["sources"].as_array().map(Vec::len), Some(2));

    let workspace = request_json(
        &app,
        Method::GET,
        "/api/v1/projects/project-logistics/workspace",
        None,
        None,
    )
    .await
    .1;
    let approval = json!({
        "verificationToken": verification.1["verificationToken"],
        "projectId": "project-logistics",
        "expectedWorkspaceVersion": workspace["snapshotVersion"],
        "deviceName": "승인된 Robot-42",
        "selectedSourceIds": ["telemetry", "camera"],
        "accessMode": "observe",
        "visibility": "operator"
    });
    let linked = request_json(
        &app,
        Method::POST,
        &format!(
            "/api/v1/network-discovery-sessions/{session_id}/candidates/{candidate_id}/approval"
        ),
        Some(approval.clone()),
        Some("approve-atomic"),
    )
    .await;
    assert_eq!(linked.0, StatusCode::OK, "{}", linked.1);
    assert_eq!(linked.1["status"], "linked");
    assert_eq!(linked.1["projectId"], "project-logistics");
    assert_eq!(linked.1["dataSourceIds"].as_array().map(Vec::len), Some(2));

    // Exact approval retries return the original receipt and never duplicate catalog assets.
    let retried = request_json(
        &app,
        Method::POST,
        &format!(
            "/api/v1/network-discovery-sessions/{session_id}/candidates/{candidate_id}/approval"
        ),
        Some(approval),
        Some("approve-atomic-retry"),
    )
    .await;
    assert_eq!(retried.0, StatusCode::OK, "{}", retried.1);
    assert_eq!(retried.1, linked.1);

    let workspace = request_json(
        &app,
        Method::GET,
        "/api/v1/projects/project-logistics/workspace",
        None,
        None,
    )
    .await
    .1;
    let device_id = linked.1["deviceId"].as_str().unwrap();
    let device = workspace["devices"]
        .as_array()
        .unwrap()
        .iter()
        .find(|device| device["id"] == device_id)
        .expect("approved device is assigned");
    assert_eq!(device["status"], "offline");
    assert_eq!(device["health"], "unknown");
    let device_assignment = workspace["deviceAssignments"]
        .as_array()
        .unwrap()
        .iter()
        .find(|assignment| assignment["deviceId"] == device_id)
        .expect("approved device assignment exists");
    assert_eq!(device_assignment["accessMode"], "observe");
    for source_id in linked.1["dataSourceIds"].as_array().unwrap() {
        let source = workspace["dataSources"]
            .as_array()
            .unwrap()
            .iter()
            .find(|source| source["id"] == *source_id)
            .expect("approved source is assigned");
        assert_eq!(source["status"], "pending");
        let assignment = workspace["dataAssignments"]
            .as_array()
            .unwrap()
            .iter()
            .find(|assignment| assignment["dataSourceId"] == *source_id)
            .expect("approved source assignment exists");
        assert_eq!(assignment["visibility"], "operator");
    }
}

#[tokio::test]
async fn discovery_cancellation_and_scan_admission_are_bounded() {
    let provider =
        rms_server::FakeDiscoveryProvider::fixture().with_scan_delay(Duration::from_millis(200));
    let state = rms_server::AppState::fixture().with_discovery_provider(Arc::new(provider));
    let app = rms_server::router(state);
    let first = start_session(&app, "discovery-start-first").await;

    let second = request_json(
        &app,
        Method::POST,
        "/api/v1/network-discovery-sessions",
        Some(json!({ "organizationId": ORGANIZATION_ID })),
        Some("discovery-start-second"),
    )
    .await;
    assert_eq!(second.0, StatusCode::TOO_MANY_REQUESTS, "{}", second.1);

    let session_id = first["id"].as_str().unwrap();
    let cancelled = request(
        &app,
        Method::DELETE,
        &format!("/api/v1/network-discovery-sessions/{session_id}"),
        None,
        Some("discovery-cancel-first"),
    )
    .await;
    assert_eq!(cancelled.status(), StatusCode::NO_CONTENT);
    tokio::time::sleep(Duration::from_millis(20)).await;
    let snapshot = request_json(
        &app,
        Method::GET,
        &format!("/api/v1/network-discovery-sessions/{session_id}"),
        None,
        None,
    )
    .await;
    assert_eq!(snapshot.0, StatusCode::OK, "{}", snapshot.1);
    assert_eq!(snapshot.1["session"]["status"], "cancelled");
}

#[tokio::test]
async fn concurrent_candidate_verification_is_singleflight() {
    let provider = BlockingVerificationProvider::new(fixture_observation().await);
    let state = rms_server::AppState::fixture()
        .with_discovery_provider(Arc::new(provider.clone()))
        .with_discovery_verification_timeout(Duration::from_secs(2));
    let app = rms_server::router(state);
    let session = start_session(&app, "discovery-start-singleflight").await;
    let session_id = session["id"].as_str().unwrap();
    let snapshot = wait_until_ready(&app, session_id).await;
    let candidate_id = snapshot["candidates"][0]["id"].as_str().unwrap();
    let verification_uri = format!(
        "/api/v1/network-discovery-sessions/{session_id}/candidates/{candidate_id}/verification"
    );

    let primary_app = app.clone();
    let primary_uri = verification_uri.clone();
    let primary = tokio::spawn(async move {
        request_json(
            &primary_app,
            Method::POST,
            &primary_uri,
            None,
            Some("verify-singleflight-primary"),
        )
        .await
    });
    provider.wait_until_verification_started().await;

    let mut concurrent = tokio::task::JoinSet::new();
    for request_index in 0..8 {
        let app = app.clone();
        let uri = verification_uri.clone();
        concurrent.spawn(async move {
            let idempotency_key = format!("verify-singleflight-{request_index}");
            request_json(&app, Method::POST, &uri, None, Some(&idempotency_key)).await
        });
    }
    let responses = tokio::time::timeout(Duration::from_secs(1), async {
        let mut responses = Vec::new();
        while let Some(response) = concurrent.join_next().await {
            responses.push(response.expect("concurrent verification request should finish"));
        }
        responses
    })
    .await
    .expect("duplicate verification requests should fail without waiting for the provider");
    assert_eq!(responses.len(), 8);
    assert!(
        responses
            .iter()
            .all(|(status, _body)| *status == StatusCode::CONFLICT),
        "duplicate responses: {responses:?}"
    );
    assert_eq!(provider.verification_call_count(), 1);

    provider.release_one_verification();
    let primary = primary
        .await
        .expect("primary verification request should finish");
    assert_eq!(primary.0, StatusCode::OK, "{}", primary.1);
    let cached = request_json(
        &app,
        Method::POST,
        &verification_uri,
        None,
        Some("verify-singleflight-cached"),
    )
    .await;
    assert_eq!(cached.0, StatusCode::OK, "{}", cached.1);
    assert_eq!(cached.1, primary.1);
    assert_eq!(provider.verification_call_count(), 1);
}

#[tokio::test]
async fn verification_timeout_rolls_back_singleflight_state() {
    let provider = BlockingVerificationProvider::new(fixture_observation().await);
    let state = rms_server::AppState::fixture()
        .with_discovery_provider(Arc::new(provider.clone()))
        .with_discovery_verification_timeout(Duration::from_millis(20));
    let app = rms_server::router(state);
    let session = start_session(&app, "discovery-start-verification-timeout").await;
    let session_id = session["id"].as_str().unwrap();
    let snapshot = wait_until_ready(&app, session_id).await;
    let candidate_id = snapshot["candidates"][0]["id"].as_str().unwrap();
    let verification_uri = format!(
        "/api/v1/network-discovery-sessions/{session_id}/candidates/{candidate_id}/verification"
    );

    for attempt in 0..2 {
        let response = request_json(
            &app,
            Method::POST,
            &verification_uri,
            None,
            Some(&format!("verify-timeout-{attempt}")),
        )
        .await;
        assert_eq!(response.0, StatusCode::REQUEST_TIMEOUT, "{}", response.1);
        let snapshot = request_json(
            &app,
            Method::GET,
            &format!("/api/v1/network-discovery-sessions/{session_id}"),
            None,
            None,
        )
        .await;
        assert_eq!(snapshot.1["candidates"][0]["status"], "found");
    }
    assert_eq!(provider.verification_call_count(), 2);
}

#[tokio::test]
async fn cancelling_a_session_rolls_back_active_candidate_verification() {
    let provider = BlockingVerificationProvider::new(fixture_observation().await);
    let state = rms_server::AppState::fixture()
        .with_discovery_provider(Arc::new(provider.clone()))
        .with_discovery_verification_timeout(Duration::from_secs(2));
    let app = rms_server::router(state);
    let session = start_session(&app, "discovery-start-verification-cancel").await;
    let session_id = session["id"].as_str().unwrap().to_owned();
    let snapshot = wait_until_ready(&app, &session_id).await;
    let candidate_id = snapshot["candidates"][0]["id"].as_str().unwrap().to_owned();
    let verification_uri = format!(
        "/api/v1/network-discovery-sessions/{session_id}/candidates/{candidate_id}/verification"
    );

    let verification_app = app.clone();
    let verification = tokio::spawn(async move {
        request_json(
            &verification_app,
            Method::POST,
            &verification_uri,
            None,
            Some("verify-cancelled-session"),
        )
        .await
    });
    provider.wait_until_verification_started().await;
    let cancelled = request(
        &app,
        Method::DELETE,
        &format!("/api/v1/network-discovery-sessions/{session_id}"),
        None,
        Some("cancel-active-verification"),
    )
    .await;
    assert_eq!(cancelled.status(), StatusCode::NO_CONTENT);
    let verification = verification
        .await
        .expect("cancelled verification request should finish");
    assert_eq!(verification.0, StatusCode::CONFLICT, "{}", verification.1);
    let snapshot = request_json(
        &app,
        Method::GET,
        &format!("/api/v1/network-discovery-sessions/{session_id}"),
        None,
        None,
    )
    .await;
    assert_eq!(snapshot.1["session"]["status"], "cancelled");
    assert_eq!(snapshot.1["candidates"][0]["status"], "found");
    assert_eq!(provider.verification_call_count(), 1);
}

#[tokio::test]
async fn expired_sessions_drop_candidates_and_cannot_be_verified() {
    let state = rms_server::AppState::fixture()
        .with_discovery_ttls(Duration::from_millis(30), Duration::from_millis(10));
    let app = rms_server::router(state);
    let session = start_session(&app, "discovery-start-expiry").await;
    let session_id = session["id"].as_str().unwrap();
    let ready = wait_until_ready(&app, session_id).await;
    let candidate_id = ready["candidates"][0]["id"].as_str().unwrap().to_owned();
    tokio::time::sleep(Duration::from_millis(45)).await;

    let expired = request_json(
        &app,
        Method::GET,
        &format!("/api/v1/network-discovery-sessions/{session_id}"),
        None,
        None,
    )
    .await;
    assert_eq!(expired.0, StatusCode::OK, "{}", expired.1);
    assert_eq!(expired.1["session"]["status"], "expired");
    assert_eq!(expired.1["candidates"], json!([]));

    let verification = request_json(
        &app,
        Method::POST,
        &format!(
            "/api/v1/network-discovery-sessions/{session_id}/candidates/{candidate_id}/verification"
        ),
        None,
        Some("verify-expired"),
    )
    .await;
    assert_eq!(verification.0, StatusCode::GONE, "{}", verification.1);
}

#[tokio::test]
async fn approval_rejects_control_and_stale_workspace_versions() {
    let app = rms_server::fixture_router();
    let session = start_session(&app, "discovery-start-invalid-approval").await;
    let session_id = session["id"].as_str().unwrap();
    let snapshot = wait_until_ready(&app, session_id).await;
    let candidate_id = snapshot["candidates"][0]["id"].as_str().unwrap();
    let verification = request_json(
        &app,
        Method::POST,
        &format!(
            "/api/v1/network-discovery-sessions/{session_id}/candidates/{candidate_id}/verification"
        ),
        None,
        Some("verify-invalid-approval"),
    )
    .await
    .1;
    let workspace = request_json(
        &app,
        Method::GET,
        "/api/v1/projects/project-logistics/workspace",
        None,
        None,
    )
    .await
    .1;
    let empty_sources = request_json(
        &app,
        Method::POST,
        &format!(
            "/api/v1/network-discovery-sessions/{session_id}/candidates/{candidate_id}/approval"
        ),
        Some(json!({
            "verificationToken": verification["verificationToken"],
            "projectId": "project-logistics",
            "expectedWorkspaceVersion": workspace["snapshotVersion"],
            "deviceName": "Robot-42",
            "selectedSourceIds": [],
            "accessMode": "observe",
            "visibility": "operator"
        })),
        Some("approve-empty-sources-rejected"),
    )
    .await;
    assert_eq!(
        empty_sources.0,
        StatusCode::BAD_REQUEST,
        "{}",
        empty_sources.1
    );
    let mut workspace_before_empty = workspace.clone();
    workspace_before_empty
        .as_object_mut()
        .expect("workspace response should be an object")
        .remove("capturedAt");
    let mut workspace_after_empty = request_json(
        &app,
        Method::GET,
        "/api/v1/projects/project-logistics/workspace",
        None,
        None,
    )
    .await
    .1;
    workspace_after_empty
        .as_object_mut()
        .expect("workspace response should be an object")
        .remove("capturedAt");
    assert_eq!(workspace_after_empty, workspace_before_empty);
    let snapshot_after_empty = request_json(
        &app,
        Method::GET,
        &format!("/api/v1/network-discovery-sessions/{session_id}"),
        None,
        None,
    )
    .await
    .1;
    assert_eq!(snapshot_after_empty["candidates"][0]["status"], "verified");

    let mut approval = json!({
        "verificationToken": verification["verificationToken"],
        "projectId": "project-logistics",
        "expectedWorkspaceVersion": workspace["snapshotVersion"],
        "deviceName": "Robot-42",
        "selectedSourceIds": ["telemetry"],
        "accessMode": "control",
        "visibility": "operator"
    });
    let control = request_json(
        &app,
        Method::POST,
        &format!(
            "/api/v1/network-discovery-sessions/{session_id}/candidates/{candidate_id}/approval"
        ),
        Some(approval.clone()),
        Some("approve-control-rejected"),
    )
    .await;
    assert_eq!(control.0, StatusCode::BAD_REQUEST, "{}", control.1);

    approval["accessMode"] = json!("observe");
    approval["expectedWorkspaceVersion"] = json!(0);
    let stale = request_json(
        &app,
        Method::POST,
        &format!(
            "/api/v1/network-discovery-sessions/{session_id}/candidates/{candidate_id}/approval"
        ),
        Some(approval),
        Some("approve-stale-rejected"),
    )
    .await;
    assert_eq!(stale.0, StatusCode::CONFLICT, "{}", stale.1);
}

async fn start_session(app: &Router, idempotency_key: &str) -> Value {
    let response = request_json(
        app,
        Method::POST,
        "/api/v1/network-discovery-sessions",
        Some(json!({ "organizationId": ORGANIZATION_ID })),
        Some(idempotency_key),
    )
    .await;
    assert_eq!(response.0, StatusCode::ACCEPTED, "{}", response.1);
    assert_eq!(response.1["status"], "searching");
    response.1
}

async fn wait_until_ready(app: &Router, session_id: &str) -> Value {
    for _ in 0..100 {
        let response = request_json(
            app,
            Method::GET,
            &format!("/api/v1/network-discovery-sessions/{session_id}"),
            None,
            None,
        )
        .await;
        assert_eq!(response.0, StatusCode::OK, "{}", response.1);
        if response.1["session"]["status"] != "searching" {
            return response.1;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    panic!("network discovery did not finish");
}

async fn fixture_observation() -> DiscoveryObservation {
    rms_server::FakeDiscoveryProvider::fixture()
        .discover(DiscoveryCancellation::new())
        .await
        .expect("fixture discovery should succeed")
        .into_iter()
        .next()
        .expect("fixture discovery should contain one candidate")
}

async fn wait_until_cancelled(cancellation: DiscoveryCancellation) {
    while !cancellation.is_cancelled() {
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
}

async fn request_json(
    app: &Router,
    method: Method,
    uri: &str,
    body: Option<Value>,
    idempotency_key: Option<&str>,
) -> (StatusCode, Value) {
    let response = request(app, method, uri, body, idempotency_key).await;
    let status = response.status();
    let bytes = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("test response body should be readable");
    let body = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes)
            .unwrap_or_else(|_| panic!("response is not JSON: {}", String::from_utf8_lossy(&bytes)))
    };
    (status, body)
}

async fn request(
    app: &Router,
    method: Method,
    uri: &str,
    body: Option<Value>,
    idempotency_key: Option<&str>,
) -> axum::response::Response {
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header(header::HOST, "127.0.0.1:8080");
    if let Some(idempotency_key) = idempotency_key {
        builder = builder
            .header("Idempotency-Key", idempotency_key)
            .header("X-RMS-Request-ID", format!("request-{idempotency_key}"));
    }
    let body = if let Some(body) = body {
        builder = builder.header(header::CONTENT_TYPE, "application/json");
        Body::from(body.to_string())
    } else {
        Body::empty()
    };
    app.clone()
        .oneshot(
            builder
                .body(body)
                .expect("test request should have valid method, URI, and headers"),
        )
        .await
        .unwrap()
}
