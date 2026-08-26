//! Bounded edge-agent supervisor, local administration API, and RMS heartbeat client.

use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    fmt,
    net::SocketAddr,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use axum::{
    BoxError, Json, Router,
    error_handling::HandleErrorLayer,
    extract::{DefaultBodyLimit, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse as _, Response},
    routing::get,
};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use parking_lot::RwLock;
use ring::hmac;
use serde::Serialize;
use sha2::{Digest as _, Sha256};
use tokio::{net::TcpListener, sync::broadcast, task::JoinSet, time::Instant};
use tokio_util::sync::CancellationToken;
use tower::ServiceBuilder;
use uuid::Uuid;

use crate::{
    adapter::{
        Adapter, AdapterError, AdapterEvent, AdapterKind, AdapterPollContext, AdapterSnapshot,
        AdapterState, mavlink::MavlinkAdapter, ros2::Ros2Adapter,
    },
    advertise::AdvertisementService,
    config::{AgentConfig, duration_ms, load_secret},
    health::{AgentHealth, AgentMetrics, aggregate_health},
    identity::{DeviceIdentity, PublicIdentity},
};

const HEARTBEAT_PATH: &str = "/api/v1/edge-agents/heartbeats";
const HEARTBEAT_CONTEXT: &str = "rms-heartbeat-v1";
const MAX_HEARTBEAT_BYTES: usize = 512 * 1024;
const MAX_HEARTBEAT_OBSERVATIONS: usize = 512;
const AUTHORIZATION_PREFIX: &str = "Bearer ";

/// Long-running RMS edge agent.
pub struct EdgeAgent {
    config: AgentConfig,
    identity: Arc<DeviceIdentity>,
    boot_id: Uuid,
    adapters: Vec<AdapterRegistration>,
    snapshots: SharedSnapshots,
    metrics: Arc<AgentMetrics>,
    events: broadcast::Sender<AdapterEvent>,
    admin_token_key: hmac::Key,
    control_plane_token: Option<String>,
    control_plane_connected: Arc<AtomicBool>,
}

impl fmt::Debug for EdgeAgent {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("EdgeAgent")
            .field("config", &self.config)
            .field("identity", &self.identity.public())
            .field("boot_id", &self.boot_id)
            .field("adapter_count", &self.adapters.len())
            .field("admin_token_key", &"<redacted>")
            .field(
                "control_plane_token",
                &self.control_plane_token.as_ref().map(|_| "<redacted>"),
            )
            .field(
                "control_plane_connected",
                &self.control_plane_connected.load(Ordering::Relaxed),
            )
            .finish_non_exhaustive()
    }
}

struct AdapterRegistration {
    adapter: Arc<dyn Adapter>,
    poll_interval: Duration,
}

type SharedSnapshots = Arc<RwLock<BTreeMap<String, AdapterSnapshot>>>;

impl EdgeAgent {
    /// Build all configured adapters and load secrets before opening any socket.
    pub fn from_config(config: AgentConfig) -> Result<Self, EdgeAgentError> {
        config.validate().map_err(EdgeAgentError::config)?;
        let mut adapters: Vec<AdapterRegistration> = Vec::new();
        for adapter_config in config.adapters.iter().filter(|adapter| adapter.enabled) {
            let adapter: Arc<dyn Adapter> = match adapter_config.kind {
                AdapterKind::Ros2Dds => Arc::new(
                    Ros2Adapter::from_config(adapter_config).map_err(EdgeAgentError::adapter)?,
                ),
                AdapterKind::Mavlink => Arc::new(
                    MavlinkAdapter::from_config(adapter_config).map_err(EdgeAgentError::adapter)?,
                ),
            };
            adapters.push(AdapterRegistration {
                adapter,
                poll_interval: duration_ms(adapter_config.poll_interval_ms),
            });
        }
        Self::new(config, adapters)
    }

    /// Construct with explicit adapters. Intended for embedding and deterministic tests.
    fn new(
        config: AgentConfig,
        adapters: Vec<AdapterRegistration>,
    ) -> Result<Self, EdgeAgentError> {
        let identity = Arc::new(
            DeviceIdentity::load_or_create(&config.identity_path)
                .map_err(EdgeAgentError::identity)?,
        );
        let admin_token = load_secret(&config.admin.bearer_token_path, 32, 512)
            .map_err(EdgeAgentError::config)?;
        let admin_token_key = hmac::Key::new(hmac::HMAC_SHA256, &admin_token);
        let control_plane_token = if config.control_plane.enabled {
            let token = load_secret(&config.control_plane.bearer_token_path, 32, 512)
                .map_err(EdgeAgentError::config)?;
            Some(
                String::from_utf8(token)
                    .map_err(|_error| EdgeAgentError::new("control-plane token is not UTF-8"))?,
            )
        } else {
            None
        };
        let event_capacity = config.supervisor.event_capacity;
        let (events, _) = broadcast::channel(event_capacity);
        let snapshots = Arc::new(RwLock::new(BTreeMap::new()));
        {
            let mut guard = snapshots.write();
            for registration in &adapters {
                let descriptor = registration.adapter.descriptor();
                guard.insert(
                    descriptor.id.clone(),
                    AdapterSnapshot {
                        descriptor,
                        state: AdapterState::Starting,
                        observations: Vec::new(),
                        last_success_at_ms: None,
                        consecutive_failures: 0,
                        last_error: None,
                    },
                );
            }
        }
        Ok(Self {
            config,
            identity,
            boot_id: Uuid::new_v4(),
            adapters,
            snapshots,
            metrics: Arc::new(AgentMetrics::default()),
            events,
            admin_token_key,
            control_plane_token,
            control_plane_connected: Arc::new(AtomicBool::new(false)),
        })
    }

    /// Subscribe to bounded, lossy snapshot-change notifications.
    ///
    /// Lagging consumers receive a broadcast lag error and must resynchronize from `snapshot()`.
    pub fn subscribe(&self) -> broadcast::Receiver<AdapterEvent> {
        self.events.subscribe()
    }

    /// Return the latest bounded adapter snapshots.
    pub fn snapshot(&self) -> Vec<AdapterSnapshot> {
        snapshot_values(&self.snapshots)
    }

    /// Run all critical services until cancellation or a critical service failure.
    pub async fn run(self, cancellation: CancellationToken) -> Result<(), EdgeAgentError> {
        let advertisement = AdvertisementService::new(
            self.config.advertise.clone(),
            Arc::clone(&self.identity),
            Arc::clone(&self.metrics),
        )
        .map_err(|error| EdgeAgentError::new(error.to_string()))?;
        let mut tasks = JoinSet::new();
        for registration in self.adapters {
            tasks.spawn(run_adapter_supervisor(
                registration,
                self.config.supervisor.clone(),
                Arc::clone(&self.snapshots),
                self.events.clone(),
                Arc::clone(&self.metrics),
                cancellation.clone(),
            ));
        }

        let admin_state = AdminState {
            identity: self.identity.public().clone(),
            boot_id: self.boot_id,
            snapshots: Arc::clone(&self.snapshots),
            metrics: Arc::clone(&self.metrics),
            token_key: self.admin_token_key,
            cancellation: cancellation.clone(),
            control_plane_enabled: self.config.control_plane.enabled,
            control_plane_connected: Arc::clone(&self.control_plane_connected),
        };
        let admin_config = self.config.admin.clone();
        tasks.spawn(run_admin(admin_config, admin_state));

        let advertisement_cancellation = cancellation.clone();
        tasks.spawn(async move {
            advertisement
                .run(advertisement_cancellation)
                .await
                .map_err(|error| EdgeAgentError::new(error.to_string()))
        });

        if self.config.control_plane.enabled {
            let token = self
                .control_plane_token
                .ok_or_else(|| EdgeAgentError::new("control-plane token was not loaded"))?;
            tasks.spawn(run_heartbeat(
                self.config.control_plane.clone(),
                token,
                Arc::clone(&self.identity),
                self.boot_id,
                Arc::clone(&self.snapshots),
                Arc::clone(&self.metrics),
                Arc::clone(&self.control_plane_connected),
                cancellation.clone(),
            ));
        }

        let outcome = tokio::select! {
            () = cancellation.cancelled() => Ok(()),
            result = tasks.join_next() => match result {
                Some(Ok(result)) => result,
                Some(Err(error)) => Err(EdgeAgentError::new(format!("service task failed: {error}"))),
                None => Err(EdgeAgentError::new("all service tasks stopped unexpectedly")),
            },
        };
        cancellation.cancel();
        while let Some(result) = tasks.join_next().await {
            if let Err(error) = result {
                tracing::warn!(error = %error, "edge-agent task failed during shutdown");
            }
        }
        mark_all_stopped(&self.snapshots, &self.events);
        outcome
    }
}

async fn run_adapter_supervisor(
    registration: AdapterRegistration,
    policy: crate::config::SupervisorConfig,
    snapshots: SharedSnapshots,
    events: broadcast::Sender<AdapterEvent>,
    metrics: Arc<AgentMetrics>,
    cancellation: CancellationToken,
) -> Result<(), EdgeAgentError> {
    let descriptor = registration.adapter.descriptor();
    let mut failures = VecDeque::new();
    let mut consecutive_failures = 0_u32;
    let mut backoff = duration_ms(policy.backoff_initial_ms);
    let maximum_backoff = duration_ms(policy.backoff_max_ms);
    let restart_window = duration_ms(policy.restart_window_ms);
    let stale_after = duration_ms(policy.stale_after_ms);
    let mut last_success_instant = None;
    loop {
        if cancellation.is_cancelled() {
            set_adapter_state(
                &snapshots,
                &events,
                &descriptor.id,
                AdapterState::Stopped,
                None,
            )?;
            return Ok(());
        }
        let now = Instant::now();
        while failures
            .front()
            .is_some_and(|failure| now.duration_since(*failure) >= restart_window)
        {
            failures.pop_front();
        }
        if failures.len() >= policy.restart_limit {
            set_adapter_state(
                &snapshots,
                &events,
                &descriptor.id,
                AdapterState::Stale,
                Some("restart budget exhausted".to_owned()),
            )?;
            let wake_at = failures
                .front()
                .copied()
                .map(|failure| failure + restart_window)
                .unwrap_or_else(|| Instant::now() + restart_window);
            tokio::select! {
                () = cancellation.cancelled() => continue,
                () = tokio::time::sleep_until(wake_at) => {}
            }
            continue;
        }

        let timeout = duration_ms(policy.poll_timeout_ms);
        let context = AdapterPollContext {
            cancellation: cancellation.child_token(),
            deadline: Instant::now() + timeout,
            max_observations: policy.max_observations_per_adapter,
        };
        let result = tokio::time::timeout(timeout, registration.adapter.poll(context)).await;
        match normalize_poll_result(result, policy.max_observations_per_adapter) {
            Ok(batch) => {
                let observation_count = batch.observations.len();
                consecutive_failures = 0;
                backoff = duration_ms(policy.backoff_initial_ms);
                last_success_instant = Some(Instant::now());
                update_adapter_success(&snapshots, &events, &descriptor.id, batch.observations)?;
                metrics.adapter_poll(observation_count);
                update_healthy_metric(&snapshots, &metrics);
                wait_with_freshness(
                    registration.poll_interval,
                    last_success_instant,
                    stale_after,
                    &snapshots,
                    &events,
                    &descriptor.id,
                    &cancellation,
                )
                .await?;
            }
            Err(error) => {
                consecutive_failures = consecutive_failures.saturating_add(1);
                failures.push_back(Instant::now());
                update_adapter_failure(
                    &snapshots,
                    &events,
                    &descriptor.id,
                    consecutive_failures,
                    error.to_string(),
                    stale_after,
                )?;
                metrics.adapter_failure();
                update_healthy_metric(&snapshots, &metrics);
                tracing::warn!(
                    adapter_id = %descriptor.id,
                    adapter_kind = descriptor.kind.as_str(),
                    error_kind = ?error.kind(),
                    error = %error,
                    "adapter poll failed"
                );
                let delay = if error.kind() == crate::adapter::AdapterErrorKind::Configuration {
                    restart_window
                } else {
                    backoff
                };
                wait_with_freshness(
                    delay,
                    last_success_instant,
                    stale_after,
                    &snapshots,
                    &events,
                    &descriptor.id,
                    &cancellation,
                )
                .await?;
                backoff = backoff.saturating_mul(2).min(maximum_backoff);
            }
        }
    }
}

async fn wait_with_freshness(
    delay: Duration,
    last_success: Option<Instant>,
    stale_after: Duration,
    snapshots: &SharedSnapshots,
    events: &broadcast::Sender<AdapterEvent>,
    adapter_id: &str,
    cancellation: &CancellationToken,
) -> Result<(), EdgeAgentError> {
    let Some(last_success) = last_success else {
        sleep_or_cancel(delay, cancellation).await;
        return Ok(());
    };
    let elapsed = last_success.elapsed();
    let until_stale = stale_after.saturating_sub(elapsed);
    if until_stale >= delay {
        sleep_or_cancel(delay, cancellation).await;
        return Ok(());
    }
    sleep_or_cancel(until_stale, cancellation).await;
    if cancellation.is_cancelled() {
        return Ok(());
    }
    mark_adapter_stale(snapshots, events, adapter_id)?;
    sleep_or_cancel(delay.saturating_sub(until_stale), cancellation).await;
    Ok(())
}

fn mark_adapter_stale(
    snapshots: &SharedSnapshots,
    events: &broadcast::Sender<AdapterEvent>,
    adapter_id: &str,
) -> Result<(), EdgeAgentError> {
    let changed = {
        let mut guard = snapshots.write();
        let snapshot = guard
            .get_mut(adapter_id)
            .ok_or_else(|| EdgeAgentError::new("adapter snapshot is missing"))?;
        if snapshot.state == AdapterState::Stopped || snapshot.state == AdapterState::Stale {
            None
        } else {
            snapshot.state = AdapterState::Stale;
            if snapshot.last_error.is_none() {
                snapshot.last_error = Some("adapter data is stale".to_owned());
            }
            Some(snapshot.clone())
        }
    };
    if let Some(snapshot) = changed {
        drop(events.send(AdapterEvent::Snapshot(snapshot)));
    }
    Ok(())
}

fn normalize_poll_result(
    result: Result<Result<crate::adapter::AdapterBatch, AdapterError>, tokio::time::error::Elapsed>,
    maximum: usize,
) -> Result<crate::adapter::AdapterBatch, AdapterError> {
    let batch = result.map_err(|_error| AdapterError::transient("adapter poll timed out"))??;
    if batch.observations.len() > maximum {
        return Err(AdapterError::invalid_data(
            "adapter returned too many observations",
        ));
    }
    let mut identities = BTreeSet::new();
    for observation in &batch.observations {
        observation.validate()?;
        if !identities.insert(observation.identity.as_str()) {
            return Err(AdapterError::invalid_data(
                "adapter returned duplicate observation identities",
            ));
        }
    }
    Ok(batch)
}

fn update_adapter_success(
    snapshots: &SharedSnapshots,
    events: &broadcast::Sender<AdapterEvent>,
    adapter_id: &str,
    observations: Vec<crate::adapter::AdapterObservation>,
) -> Result<(), EdgeAgentError> {
    let now = unix_time_ms()?;
    let snapshot = {
        let mut guard = snapshots.write();
        let snapshot = guard
            .get_mut(adapter_id)
            .ok_or_else(|| EdgeAgentError::new("adapter snapshot is missing"))?;
        snapshot.state = AdapterState::Healthy;
        snapshot.observations = observations;
        snapshot.last_success_at_ms = Some(now);
        snapshot.consecutive_failures = 0;
        snapshot.last_error = None;
        snapshot.clone()
    };
    drop(events.send(AdapterEvent::Snapshot(snapshot)));
    Ok(())
}

fn update_adapter_failure(
    snapshots: &SharedSnapshots,
    events: &broadcast::Sender<AdapterEvent>,
    adapter_id: &str,
    failures: u32,
    error: String,
    stale_after: Duration,
) -> Result<(), EdgeAgentError> {
    let now = unix_time_ms()?;
    let stale_after_ms = i64::try_from(stale_after.as_millis()).unwrap_or(i64::MAX);
    let snapshot = {
        let mut guard = snapshots.write();
        let snapshot = guard
            .get_mut(adapter_id)
            .ok_or_else(|| EdgeAgentError::new("adapter snapshot is missing"))?;
        snapshot.state = if snapshot
            .last_success_at_ms
            .is_none_or(|last_success| now.saturating_sub(last_success) >= stale_after_ms)
        {
            AdapterState::Stale
        } else {
            AdapterState::Degraded
        };
        snapshot.consecutive_failures = failures;
        snapshot.last_error = Some(truncate(error, 256));
        snapshot.clone()
    };
    drop(events.send(AdapterEvent::Snapshot(snapshot)));
    Ok(())
}

fn set_adapter_state(
    snapshots: &SharedSnapshots,
    events: &broadcast::Sender<AdapterEvent>,
    adapter_id: &str,
    state: AdapterState,
    error: Option<String>,
) -> Result<(), EdgeAgentError> {
    let snapshot = {
        let mut guard = snapshots.write();
        let snapshot = guard
            .get_mut(adapter_id)
            .ok_or_else(|| EdgeAgentError::new("adapter snapshot is missing"))?;
        snapshot.state = state;
        snapshot.last_error = error;
        snapshot.clone()
    };
    drop(events.send(AdapterEvent::Snapshot(snapshot)));
    Ok(())
}

fn mark_all_stopped(snapshots: &SharedSnapshots, events: &broadcast::Sender<AdapterEvent>) {
    let updated = {
        let mut guard = snapshots.write();
        guard
            .values_mut()
            .map(|snapshot| {
                snapshot.state = AdapterState::Stopped;
                snapshot.clone()
            })
            .collect::<Vec<_>>()
    };
    for snapshot in updated {
        drop(events.send(AdapterEvent::Snapshot(snapshot)));
    }
}

fn update_healthy_metric(snapshots: &SharedSnapshots, metrics: &AgentMetrics) {
    let guard = snapshots.read();
    let healthy = guard
        .values()
        .filter(|snapshot| snapshot.state == AdapterState::Healthy)
        .count();
    metrics.set_healthy_adapters(healthy);
}

fn snapshot_values(snapshots: &SharedSnapshots) -> Vec<AdapterSnapshot> {
    let guard = snapshots.read();
    guard.values().cloned().collect()
}

#[derive(Clone)]
struct AdminState {
    identity: PublicIdentity,
    boot_id: Uuid,
    snapshots: SharedSnapshots,
    metrics: Arc<AgentMetrics>,
    token_key: hmac::Key,
    cancellation: CancellationToken,
    control_plane_enabled: bool,
    control_plane_connected: Arc<AtomicBool>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct AgentStatus {
    schema_version: u32,
    identity: PublicIdentity,
    boot_id: Uuid,
    health: AgentHealth,
    adapters: Vec<AdapterSnapshot>,
    control_plane: ControlPlaneStatus,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ControlPlaneStatus {
    enabled: bool,
    connected: bool,
}

async fn run_admin(
    config: crate::config::AdminConfig,
    state: AdminState,
) -> Result<(), EdgeAgentError> {
    let listener = TcpListener::bind(SocketAddr::new(config.bind_ip, config.port))
        .await
        .map_err(|error| EdgeAgentError::new(format!("admin bind failed: {error}")))?;
    let cancellation = state.cancellation.clone();
    let router = Router::new()
        .route("/v1/status", get(admin_status))
        .route("/v1/health", get(admin_health))
        .route("/metrics", get(admin_metrics))
        .with_state(state)
        .layer(DefaultBodyLimit::disable())
        .layer(
            ServiceBuilder::new()
                .layer(HandleErrorLayer::new(handle_service_error))
                .load_shed()
                .concurrency_limit(config.max_connections)
                .timeout(duration_ms(config.request_timeout_ms)),
        );
    axum::serve(listener, router)
        .with_graceful_shutdown(cancellation.cancelled_owned())
        .await
        .map_err(|error| EdgeAgentError::new(format!("admin server failed: {error}")))
}

async fn admin_status(State(state): State<AdminState>, headers: HeaderMap) -> Response {
    if !authorized(&headers, &state.token_key) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let adapters = snapshot_values(&state.snapshots);
    let control_plane_connected = state.control_plane_connected.load(Ordering::Relaxed);
    let control_plane = ControlPlaneStatus {
        enabled: state.control_plane_enabled,
        connected: control_plane_connected,
    };
    Json(AgentStatus {
        schema_version: 1,
        identity: state.identity,
        boot_id: state.boot_id,
        health: effective_health(&adapters, state.cancellation.is_cancelled(), &control_plane),
        adapters,
        control_plane,
    })
    .into_response()
}

async fn admin_health(State(state): State<AdminState>, headers: HeaderMap) -> Response {
    if !authorized(&headers, &state.token_key) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let adapters = snapshot_values(&state.snapshots);
    let control_plane = ControlPlaneStatus {
        enabled: state.control_plane_enabled,
        connected: state.control_plane_connected.load(Ordering::Relaxed),
    };
    let health = effective_health(&adapters, state.cancellation.is_cancelled(), &control_plane);
    let status = if health == AgentHealth::Stale {
        StatusCode::SERVICE_UNAVAILABLE
    } else {
        StatusCode::OK
    };
    (status, Json(serde_json::json!({ "status": health }))).into_response()
}

fn effective_health(
    adapters: &[AdapterSnapshot],
    shutting_down: bool,
    control_plane: &ControlPlaneStatus,
) -> AgentHealth {
    let adapter_health = aggregate_health(adapters, shutting_down);
    if control_plane.enabled && !control_plane.connected && adapter_health == AgentHealth::Healthy {
        AgentHealth::Degraded
    } else {
        adapter_health
    }
}

async fn admin_metrics(State(state): State<AdminState>, headers: HeaderMap) -> Response {
    if !authorized(&headers, &state.token_key) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    match state.metrics.encode() {
        Ok(metrics) => (
            [(
                header::CONTENT_TYPE,
                "application/openmetrics-text; version=1.0.0",
            )],
            metrics,
        )
            .into_response(),
        Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

fn authorized(headers: &HeaderMap, expected_key: &hmac::Key) -> bool {
    let Some(value) = headers.get(header::AUTHORIZATION) else {
        return false;
    };
    let Ok(value) = value.to_str() else {
        return false;
    };
    let Some(token) = value.strip_prefix(AUTHORIZATION_PREFIX) else {
        return false;
    };
    if token.is_empty() || token.len() > 512 || token.contains(char::is_whitespace) {
        return false;
    }
    let supplied_key = hmac::Key::new(hmac::HMAC_SHA256, token.as_bytes());
    let supplied_tag = hmac::sign(&supplied_key, b"rms-admin-token-v1");
    hmac::verify(expected_key, b"rms-admin-token-v1", supplied_tag.as_ref()).is_ok()
}

async fn handle_service_error(_error: BoxError) -> Response {
    StatusCode::SERVICE_UNAVAILABLE.into_response()
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct HeartbeatPayload {
    schema_version: u32,
    device_id: Uuid,
    public_key: String,
    boot_id: Uuid,
    sequence: u64,
    sent_at_ms: i64,
    agent_health: AgentHealth,
    adapters: Vec<HeartbeatAdapter>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct HeartbeatAdapter {
    id: String,
    kind: AdapterKind,
    state: AdapterState,
    last_success_at_ms: Option<i64>,
    observations: Vec<crate::adapter::AdapterObservation>,
}

impl From<AdapterSnapshot> for HeartbeatAdapter {
    fn from(snapshot: AdapterSnapshot) -> Self {
        Self {
            id: snapshot.descriptor.id,
            kind: snapshot.descriptor.kind,
            state: snapshot.state,
            last_success_at_ms: snapshot.last_success_at_ms,
            observations: snapshot.observations,
        }
    }
}

async fn run_heartbeat(
    config: crate::config::ControlPlaneConfig,
    token: String,
    identity: Arc<DeviceIdentity>,
    boot_id: Uuid,
    snapshots: SharedSnapshots,
    metrics: Arc<AgentMetrics>,
    control_plane_connected: Arc<AtomicBool>,
    cancellation: CancellationToken,
) -> Result<(), EdgeAgentError> {
    let base_url = reqwest::Url::parse(&config.server_url)
        .map_err(|_error| EdgeAgentError::new("control-plane URL is invalid"))?;
    let endpoint = base_url
        .join(HEARTBEAT_PATH)
        .map_err(|_error| EdgeAgentError::new("heartbeat endpoint URL is invalid"))?;
    let mut client_builder = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(duration_ms(config.timeout_ms));
    if let Some(path) = &config.ca_certificate_path {
        let certificate_bytes = read_bounded_file(path, 64 * 1024, "CA certificate")?;
        let certificate = reqwest::Certificate::from_pem(&certificate_bytes)
            .map_err(|_error| EdgeAgentError::new("control-plane CA certificate is invalid"))?;
        client_builder = client_builder.add_root_certificate(certificate);
    }
    let client = client_builder
        .build()
        .map_err(|error| EdgeAgentError::new(format!("heartbeat client failed: {error}")))?;
    let mut sequence = 0_u64;
    let mut retry = Duration::from_secs(1);
    loop {
        if cancellation.is_cancelled() {
            return Ok(());
        }
        sequence = sequence.saturating_add(1);
        let mut snapshots = snapshot_values(&snapshots);
        let mut remaining_observations = MAX_HEARTBEAT_OBSERVATIONS;
        for adapter in &mut snapshots {
            let adapter_limit = config
                .max_observations_per_adapter
                .min(remaining_observations);
            adapter.observations.truncate(adapter_limit);
            remaining_observations =
                remaining_observations.saturating_sub(adapter.observations.len());
        }
        let agent_health = aggregate_health(&snapshots, false);
        let adapters = snapshots.into_iter().map(HeartbeatAdapter::from).collect();
        let sent_at_ms = unix_time_ms()?;
        let payload = HeartbeatPayload {
            schema_version: 1,
            device_id: identity.public().device_id,
            public_key: identity.public().public_key.clone(),
            boot_id,
            sequence,
            sent_at_ms,
            agent_health,
            adapters,
        };
        let body = serde_json::to_vec(&payload)
            .map_err(|error| EdgeAgentError::new(format!("heartbeat encoding failed: {error}")))?;
        if body.len() > MAX_HEARTBEAT_BYTES {
            metrics.heartbeat_failure();
            control_plane_connected.store(false, Ordering::Relaxed);
            tracing::error!(body_bytes = body.len(), "heartbeat exceeds payload limit");
            sleep_or_cancel(Duration::from_secs(30), &cancellation).await;
            continue;
        }
        let nonce = random_nonce()?;
        let body_hash = hex_sha256(&body);
        let canonical = format!("{HEARTBEAT_CONTEXT}\n{sent_at_ms}\n{nonce}\n{body_hash}");
        let signature = identity.sign_base64(canonical.as_bytes());
        let response = client
            .post(endpoint.clone())
            .bearer_auth(&token)
            .header("X-RMS-Timestamp", sent_at_ms.to_string())
            .header("X-RMS-Nonce", &nonce)
            .header("X-RMS-Signature", signature)
            .header(header::CONTENT_TYPE, "application/json")
            .body(body)
            .send()
            .await;
        match response {
            Ok(response) if response.status() == StatusCode::ACCEPTED => {
                metrics.heartbeat_success();
                control_plane_connected.store(true, Ordering::Relaxed);
                retry = Duration::from_secs(1);
                sleep_or_cancel(duration_ms(config.interval_ms), &cancellation).await;
            }
            Ok(response)
                if matches!(
                    response.status(),
                    StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN
                ) =>
            {
                metrics.heartbeat_failure();
                control_plane_connected.store(false, Ordering::Relaxed);
                tracing::error!(status = %response.status(), "heartbeat authorization rejected; circuit open");
                sleep_or_cancel(Duration::from_mins(5), &cancellation).await;
            }
            Ok(response) => {
                metrics.heartbeat_failure();
                control_plane_connected.store(false, Ordering::Relaxed);
                tracing::warn!(status = %response.status(), "heartbeat rejected");
                sleep_or_cancel(with_jitter(retry)?, &cancellation).await;
                retry = retry.saturating_mul(2).min(Duration::from_secs(30));
            }
            Err(error) => {
                metrics.heartbeat_failure();
                control_plane_connected.store(false, Ordering::Relaxed);
                tracing::warn!(error = %error, "heartbeat delivery failed");
                sleep_or_cancel(with_jitter(retry)?, &cancellation).await;
                retry = retry.saturating_mul(2).min(Duration::from_secs(30));
            }
        }
    }
}

async fn sleep_or_cancel(duration: Duration, cancellation: &CancellationToken) {
    tokio::select! {
        () = cancellation.cancelled() => {},
        () = tokio::time::sleep(duration) => {}
    }
}

fn with_jitter(duration: Duration) -> Result<Duration, EdgeAgentError> {
    let mut random = [0_u8; 2];
    getrandom::fill(&mut random)
        .map_err(|error| EdgeAgentError::new(format!("random generation failed: {error}")))?;
    let jitter = Duration::from_millis(u64::from(u16::from_be_bytes(random)) % 251);
    Ok(duration.saturating_add(jitter))
}

fn random_nonce() -> Result<String, EdgeAgentError> {
    let mut nonce = [0_u8; 16];
    getrandom::fill(&mut nonce)
        .map_err(|error| EdgeAgentError::new(format!("random generation failed: {error}")))?;
    Ok(URL_SAFE_NO_PAD.encode(nonce))
}

fn read_bounded_file(
    path: &std::path::Path,
    maximum_bytes: u64,
    label: &str,
) -> Result<Vec<u8>, EdgeAgentError> {
    let metadata = std::fs::metadata(path)
        .map_err(|error| EdgeAgentError::new(format!("{label} metadata failed: {error}")))?;
    if metadata.len() == 0 || metadata.len() > maximum_bytes {
        return Err(EdgeAgentError::new(format!(
            "{label} size is outside configured limits"
        )));
    }
    std::fs::read(path)
        .map_err(|error| EdgeAgentError::new(format!("{label} read failed: {error}")))
}

fn hex_sha256(value: &[u8]) -> String {
    use std::fmt::Write as _;
    let digest = Sha256::digest(value);
    let mut encoded = String::with_capacity(64);
    for byte in digest {
        write!(&mut encoded, "{byte:02x}").expect("writing to a String cannot fail");
    }
    encoded
}

fn unix_time_ms() -> Result<i64, EdgeAgentError> {
    let milliseconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_error| EdgeAgentError::new("system clock precedes Unix epoch"))?
        .as_millis();
    i64::try_from(milliseconds).map_err(|_error| EdgeAgentError::new("system clock overflow"))
}

fn truncate(mut value: String, maximum: usize) -> String {
    if value.len() <= maximum {
        return value;
    }
    while !value.is_char_boundary(maximum.min(value.len())) {
        value.pop();
    }
    value.truncate(maximum);
    value
}

/// Edge-agent startup or critical runtime failure.
#[derive(Debug)]
pub struct EdgeAgentError {
    message: String,
}

impl EdgeAgentError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }

    fn config(error: impl fmt::Display) -> Self {
        Self::new(error.to_string())
    }

    fn identity(error: impl fmt::Display) -> Self {
        Self::new(error.to_string())
    }

    fn adapter(error: impl fmt::Display) -> Self {
        Self::new(error.to_string())
    }
}

impl fmt::Display for EdgeAgentError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for EdgeAgentError {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::AdapterDescriptor;
    use crate::config::{
        AdminConfig, AdvertiseConfig, ControlPlaneConfig, Environment, SupervisorConfig,
    };

    #[test]
    fn bearer_auth_is_strict() {
        let expected = hmac::Key::new(hmac::HMAC_SHA256, b"abcdefghijklmnopqrstuvwxyz-123456");
        let mut headers = HeaderMap::new();
        headers.insert(
            header::AUTHORIZATION,
            "Bearer abcdefghijklmnopqrstuvwxyz-123456"
                .parse()
                .expect("header parses"),
        );
        assert!(authorized(&headers, &expected));
        headers.insert(
            header::AUTHORIZATION,
            "bearer abcdefghijklmnopqrstuvwxyz-123456"
                .parse()
                .expect("header parses"),
        );
        assert!(!authorized(&headers, &expected));
    }

    #[test]
    fn heartbeat_hash_is_stable() {
        assert_eq!(
            hex_sha256(b"{}"),
            "44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a"
        );
    }

    #[test]
    fn heartbeat_adapter_uses_server_wire_contract_only() {
        let adapter = HeartbeatAdapter::from(AdapterSnapshot {
            descriptor: AdapterDescriptor {
                id: "ros-main".to_owned(),
                kind: AdapterKind::Ros2Dds,
                display_name: "not on heartbeat wire".to_owned(),
            },
            state: AdapterState::Healthy,
            observations: Vec::new(),
            last_success_at_ms: Some(42),
            consecutive_failures: 7,
            last_error: Some("not on heartbeat wire".to_owned()),
        });
        let value = serde_json::to_value(adapter).expect("heartbeat adapter serializes");
        let keys = value
            .as_object()
            .expect("heartbeat adapter is an object")
            .keys()
            .map(String::as_str)
            .collect::<BTreeSet<_>>();
        assert_eq!(
            keys,
            BTreeSet::from(["id", "kind", "state", "lastSuccessAtMs", "observations",])
        );
        assert_eq!(value["kind"], "ros2_dds");
    }

    #[tokio::test]
    async fn admin_is_loopback_authenticated_and_shuts_down() {
        let root = tempfile::tempdir().expect("temporary directory");
        let token_path = root.path().join("admin.token");
        std::fs::write(&token_path, b"abcdefghijklmnopqrstuvwxyz-123456\n")
            .expect("token is written");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&token_path, std::fs::Permissions::from_mode(0o600))
                .expect("token permissions are private");
        }
        let probe = std::net::TcpListener::bind("127.0.0.1:0").expect("test port binds");
        let port = probe.local_addr().expect("test address exists").port();
        drop(probe);
        let config = AgentConfig {
            schema_version: 1,
            environment: Environment::Development,
            identity_path: root.path().join("identity.json"),
            admin: AdminConfig {
                bind_ip: "127.0.0.1".parse().expect("test address"),
                port,
                bearer_token_path: token_path,
                max_connections: 4,
                request_timeout_ms: 1_000,
            },
            advertise: AdvertiseConfig {
                enabled: false,
                display_name: "Test Edge".to_owned(),
                device_kind: "gateway".to_owned(),
                bind_ip: "192.168.1.2".parse().expect("test address"),
                service_port: 9_879,
                path: "/rms/v1/manifest".to_owned(),
                ttl_seconds: 30,
                capabilities: Vec::new(),
                organization_id: None,
                organization_key_id: None,
                organization_psk_path: None,
            },
            supervisor: SupervisorConfig::default(),
            control_plane: ControlPlaneConfig::default(),
            adapters: Vec::new(),
        };
        let agent = EdgeAgent::from_config(config).expect("agent is configured");
        let cancellation = CancellationToken::new();
        let task_cancellation = cancellation.clone();
        let task = tokio::spawn(agent.run(task_cancellation));
        let client = reqwest::Client::new();
        let url = format!("http://127.0.0.1:{port}/v1/status");
        let mut ready = false;
        for _ in 0..40 {
            if client.get(&url).send().await.is_ok() {
                ready = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        assert!(ready, "admin server did not start");
        let unauthorized = client
            .get(&url)
            .send()
            .await
            .expect("unauthenticated request completes");
        assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);
        let authorized = client
            .get(&url)
            .bearer_auth("abcdefghijklmnopqrstuvwxyz-123456")
            .send()
            .await
            .expect("authenticated request completes");
        assert_eq!(authorized.status(), StatusCode::OK);
        cancellation.cancel();
        tokio::time::timeout(Duration::from_secs(2), task)
            .await
            .expect("agent shuts down before deadline")
            .expect("agent task joins")
            .expect("agent exits successfully");
    }
}
