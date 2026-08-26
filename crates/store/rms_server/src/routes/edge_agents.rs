//! Authenticated, signed Edge Agent health and adapter inventory ingestion.

use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
    time::Duration,
};

use axum::{
    Json, Router,
    body::Bytes,
    extract::{DefaultBodyLimit, State},
    http::{HeaderMap, StatusCode, header::AUTHORIZATION},
    routing::post,
};
use base64::{Engine as _, prelude::BASE64_URL_SAFE_NO_PAD};
use ring::{hmac, signature};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use uuid::Uuid;

use crate::{
    AppState,
    domain::{EdgeEnrollment, Topic},
    error::{ApiError, ApiResult},
    network_discovery::edge_advertisement::derive_device_id,
    state::{Catalog, iso_from_millis, now_ms},
};

const MAX_BODY_BYTES: usize = 512 * 1024;
const MAX_CLOCK_SKEW_MS: u64 = 30_000;
const MAX_NONCES: usize = 4_096;
const MAX_BOOT_SEQUENCES: usize = 1_024;
const SEQUENCE_RETENTION_MS: i64 = 24 * 60 * 60 * 1_000;
#[cfg(not(test))]
const STALE_AFTER_MS: i64 = 10_000;
#[cfg(test)]
const STALE_AFTER_MS: i64 = 500;
const MAX_ADAPTERS: usize = 32;
const MAX_OBSERVATIONS: usize = 512;
const MAX_SOURCES: usize = 16;
const MAX_TOPICS: usize = 256;
const MAX_RETAINED_TOPICS_PER_SOURCE: usize = 512;
const MAX_METADATA: usize = 32;

pub(super) fn router() -> Router<AppState> {
    Router::new()
        .route("/api/v1/edge-agents/heartbeats", post(accept_heartbeat))
        .layer(DefaultBodyLimit::max(MAX_BODY_BYTES))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct EdgeHeartbeat {
    schema_version: u32,
    device_id: String,
    public_key: String,
    boot_id: String,
    sent_at_ms: i64,
    sequence: u64,
    agent_health: String,
    adapters: Vec<EdgeAdapterHeartbeat>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct EdgeAdapterHeartbeat {
    id: String,
    kind: String,
    state: String,
    last_success_at_ms: Option<i64>,
    observations: Vec<EdgeObservation>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct EdgeObservation {
    identity: String,
    display_name: String,
    device_kind: String,
    trust: String,
    observed_at_ms: i64,
    expires_at_ms: i64,
    sources: Vec<EdgeSource>,
    metadata: BTreeMap<String, String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct EdgeSource {
    id: String,
    label: String,
    category: String,
    protocol: String,
    status: String,
    renderer_hint: Option<String>,
    topics: Vec<EdgeTopic>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct EdgeTopic {
    path: String,
    label: String,
    message_type: Option<String>,
    qos: Option<EdgeQos>,
    renderer_hint: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct EdgeQos {
    reliability: String,
    durability: String,
    history_depth: Option<u32>,
}

async fn accept_heartbeat(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> ApiResult<(StatusCode, Json<Value>)> {
    let received_at_ms = now_ms();
    let token = state.edge_heartbeat_token.as_ref().ok_or_else(|| {
        ApiError::unavailable("Edge Agent heartbeat enrollment is not configured.")
    })?;
    verify_bearer(&headers, token)?;
    let heartbeat: EdgeHeartbeat = serde_json::from_slice(&body)
        .map_err(|_error| ApiError::bad_request("The Edge Agent heartbeat is invalid."))?;
    validate_heartbeat(&heartbeat)?;

    let public_key = decode_exact::<32>(&heartbeat.public_key, "publicKey")?;
    if derive_device_id(&public_key) != heartbeat.device_id {
        return Err(ApiError::forbidden(
            "The Edge Agent identity is not enrolled.",
        ));
    }
    let timestamp = header_text(&headers, "x-rms-timestamp")?
        .parse::<i64>()
        .map_err(|_error| ApiError::unauthorized("The Edge Agent signature is invalid."))?;
    if timestamp != heartbeat.sent_at_ms || timestamp.abs_diff(received_at_ms) > MAX_CLOCK_SKEW_MS {
        return Err(ApiError::unauthorized(
            "The Edge Agent heartbeat timestamp is stale.",
        ));
    }
    let nonce_text = header_text(&headers, "x-rms-nonce")?;
    let _nonce = decode_exact::<16>(nonce_text, "X-RMS-Nonce")?;
    let signature_bytes =
        decode_exact::<64>(header_text(&headers, "x-rms-signature")?, "X-RMS-Signature")?;
    let body_hash = hex_sha256(&body);
    let canonical = format!("rms-heartbeat-v1\n{timestamp}\n{nonce_text}\n{body_hash}");
    signature::UnparsedPublicKey::new(&signature::ED25519, public_key)
        .verify(canonical.as_bytes(), &signature_bytes)
        .map_err(|_error| ApiError::unauthorized("The Edge Agent signature is invalid."))?;

    // Serialize the replay check, durable catalog transaction, and deadline publication so a
    // later sequence can never be overwritten by a slower earlier request.
    let _apply_guard = state.edge_heartbeat_apply_lock.lock().await;
    let enrollment = {
        let catalog = state.catalog.read().await;
        catalog
            .edge_enrollments
            .get(&heartbeat.device_id)
            .filter(|enrollment| enrollment.public_key == heartbeat.public_key)
            .cloned()
            .ok_or_else(|| ApiError::forbidden("The Edge Agent identity is not enrolled."))?
    };

    let deadline = received_at_ms.saturating_add(STALE_AFTER_MS);
    let sequence_key = (heartbeat.device_id.clone(), heartbeat.boot_id.clone());
    let previous_sequence;
    let previous_deadline;
    {
        let mut runtime = state.edge_heartbeat_runtime.lock().await;
        let oldest_nonce = received_at_ms.saturating_sub(MAX_CLOCK_SKEW_MS.cast_signed());
        runtime.nonces.retain(|_, seen_at| *seen_at >= oldest_nonce);
        if runtime.nonces.contains_key(nonce_text) {
            return Err(ApiError::conflict(
                "The Edge Agent heartbeat was already accepted.",
            ));
        }
        runtime.sequences.retain(|_, (_, seen_at)| {
            *seen_at >= received_at_ms.saturating_sub(SEQUENCE_RETENTION_MS)
        });
        previous_sequence = runtime.sequences.get(&sequence_key).copied();
        if runtime
            .sequences
            .get(&sequence_key)
            .is_some_and(|(sequence, _)| heartbeat.sequence <= *sequence)
        {
            return Err(ApiError::conflict(
                "The Edge Agent heartbeat sequence is stale.",
            ));
        }
        if runtime.sequences.len() >= MAX_BOOT_SEQUENCES
            && !runtime.sequences.contains_key(&sequence_key)
        {
            return Err(ApiError::too_many_requests(
                "Too many Edge Agent boots are active.",
            ));
        }
        runtime.nonces.insert(nonce_text.to_owned(), received_at_ms);
        while runtime.nonces.len() > MAX_NONCES {
            let Some(first) = runtime.nonces.keys().next().cloned() else {
                break;
            };
            runtime.nonces.remove(&first);
        }
        runtime
            .sequences
            .insert(sequence_key.clone(), (heartbeat.sequence, received_at_ms));
        previous_deadline = runtime
            .deadlines
            .insert(heartbeat.device_id.clone(), deadline);
    }

    let enrollment = match apply_heartbeat(&state, &enrollment, &heartbeat, received_at_ms).await {
        Ok(enrollment) => enrollment,
        Err(error) => {
            let mut runtime = state.edge_heartbeat_runtime.lock().await;
            runtime.nonces.remove(nonce_text);
            if let Some(previous) = previous_sequence {
                runtime.sequences.insert(sequence_key, previous);
            } else {
                runtime.sequences.remove(&sequence_key);
            }
            if runtime.deadlines.get(&heartbeat.device_id) == Some(&deadline) {
                if let Some(previous) = previous_deadline {
                    runtime
                        .deadlines
                        .insert(heartbeat.device_id.clone(), previous);
                } else {
                    runtime.deadlines.remove(&heartbeat.device_id);
                }
            }
            return Err(error);
        }
    };
    schedule_stale_transition(state.clone(), enrollment, deadline).await;
    Ok((
        StatusCode::ACCEPTED,
        Json(json!({
            "status": "accepted",
            "deviceId": heartbeat.device_id,
            "sequence": heartbeat.sequence,
        })),
    ))
}

async fn apply_heartbeat(
    state: &AppState,
    enrollment: &EdgeEnrollment,
    heartbeat: &EdgeHeartbeat,
    received_at_ms: i64,
) -> ApiResult<EdgeEnrollment> {
    state
        .durable_catalog_mutation(|catalog| {
            let current_enrollment = catalog
                .edge_enrollments
                .get(&enrollment.edge_device_id)
                .filter(|current| current.public_key == enrollment.public_key)
                .cloned()
                .ok_or_else(|| ApiError::forbidden("The Edge Agent identity is not enrolled."))?;
            apply_heartbeat_to_catalog(catalog, &current_enrollment, heartbeat, received_at_ms)?;
            Ok(current_enrollment)
        })
        .await
}

fn apply_heartbeat_to_catalog(
    catalog: &mut Catalog,
    enrollment: &EdgeEnrollment,
    heartbeat: &EdgeHeartbeat,
    received_at_ms: i64,
) -> ApiResult<()> {
    let now = iso_from_millis(received_at_ms);
    let integration = catalog
        .integrations
        .get_mut(&enrollment.integration_id)
        .ok_or_else(|| ApiError::forbidden("The Edge Agent identity is not enrolled."))?;
    integration.last_health_at.clone_from(&now);
    integration.status = if enrollment.organization_trusted {
        "connected"
    } else {
        "testing"
    }
    .to_owned();
    integration.resource_version = integration.resource_version.saturating_add(1);

    let device = catalog
        .devices
        .get_mut(&enrollment.device_id)
        .ok_or_else(|| ApiError::forbidden("The Edge Agent identity is not enrolled."))?;
    let next_health = match heartbeat.agent_health.as_str() {
        "healthy" => "normal",
        "degraded" => "attention",
        _ => "unknown",
    };
    let state_changed = device.status != "online" || device.health != next_health;
    device.status = "online".to_owned();
    device.health = next_health.to_owned();
    device.last_seen_at.clone_from(&now);
    if state_changed {
        device.state_version = device.state_version.saturating_add(1);
    }

    let approved_source_ids = enrollment
        .source_ids
        .values()
        .cloned()
        .collect::<BTreeSet<_>>();
    let previous_topic_schemas = approved_source_ids
        .iter()
        .map(|source_id| {
            (
                source_id.clone(),
                topic_schema(
                    catalog
                        .topics_by_data_source
                        .get(source_id)
                        .map_or(&[], Vec::as_slice),
                ),
            )
        })
        .collect::<BTreeMap<_, _>>();
    for source_id in &approved_source_ids {
        if let Some(source) = catalog.data_sources.get_mut(source_id) {
            source.status = "pending".to_owned();
        }
        if let Some(topics) = catalog.topics_by_data_source.get_mut(source_id) {
            for topic in topics {
                topic.quality = "stale".to_owned();
            }
        }
    }

    let mut fresh_topics_by_source: BTreeMap<String, BTreeMap<String, Topic>> = BTreeMap::new();
    for adapter in &heartbeat.adapters {
        if heartbeat.agent_health == "stale"
            || !matches!(adapter.state.as_str(), "healthy" | "degraded")
        {
            continue;
        }
        let (expected_protocol, canonical_source_id) = match adapter.kind.as_str() {
            "ros2_dds" => ("ros2", "ros2-graph"),
            "mavlink" => ("mavlink", "mavlink-telemetry"),
            _ => continue,
        };
        for observation in &adapter.observations {
            if observation.expires_at_ms <= received_at_ms {
                continue;
            }
            for source in &observation.sources {
                if source.protocol != expected_protocol {
                    return Err(ApiError::forbidden(
                        "The Edge Agent source protocol binding is invalid.",
                    ));
                }
                let source_binding = enrollment
                    .source_ids
                    .get(&source.id)
                    .or_else(|| enrollment.source_ids.get(canonical_source_id));
                let Some(source_id) = source_binding else {
                    continue;
                };
                let data_source = catalog.data_sources.get(source_id).ok_or_else(|| {
                    ApiError::forbidden("The Edge Agent source binding is invalid.")
                })?;
                if data_source.device_id != enrollment.device_id
                    || data_source.integration_id != enrollment.integration_id
                    || data_source.protocol != expected_protocol
                {
                    return Err(ApiError::forbidden(
                        "The Edge Agent source binding is invalid.",
                    ));
                }
                let fresh_topics = fresh_topics_by_source.entry(source_id.clone()).or_default();
                for topic in &source.topics {
                    fresh_topics.insert(
                        topic.path.clone(),
                        materialize_topic(source_id, &enrollment.device_id, topic, &now),
                    );
                    if fresh_topics.len() > MAX_TOPICS {
                        return Err(ApiError::bad_request(
                            "The Edge Agent source topic inventory is too large.",
                        ));
                    }
                }
            }
        }
    }

    for (source_id, fresh_topics) in fresh_topics_by_source {
        let data_source = catalog
            .data_sources
            .get_mut(&source_id)
            .ok_or_else(|| ApiError::forbidden("The Edge Agent source binding is invalid."))?;
        // Adapter discovery alone never proves that a Rerun live stream exists.
        data_source.status = "pending".to_owned();
        data_source.last_data_at.clone_from(&now);
        let topics = catalog.topics_by_data_source.entry(source_id).or_default();
        for (path, materialized) in fresh_topics {
            if let Some(existing) = topics.iter_mut().find(|existing| existing.path == path) {
                *existing = materialized;
            } else {
                topics.push(materialized);
            }
        }
    }
    for source_id in approved_source_ids {
        if let Some(topics) = catalog.topics_by_data_source.get_mut(&source_id) {
            prune_retained_topics(topics);
        }
        let schema_changed = previous_topic_schemas.get(&source_id)
            != Some(&topic_schema(
                catalog
                    .topics_by_data_source
                    .get(&source_id)
                    .map_or(&[], Vec::as_slice),
            ));
        let topic_ids = catalog
            .topics_by_data_source
            .get(&source_id)
            .map(|topics| topics.iter().map(|topic| topic.id.clone()).collect())
            .unwrap_or_default();
        if let Some(source) = catalog.data_sources.get_mut(&source_id) {
            source.topic_ids = topic_ids;
            if schema_changed {
                source.mapping_version = source.mapping_version.saturating_add(1);
            }
        }
    }
    let affected_projects = catalog
        .device_assignments
        .values()
        .filter(|assignment| assignment.device_id == enrollment.device_id)
        .map(|assignment| assignment.project_id.clone())
        .collect::<BTreeSet<_>>();
    for project_id in affected_projects {
        catalog.refresh_project_counts(&project_id);
    }
    catalog.bump_version();
    Ok(())
}

fn prune_retained_topics(topics: &mut Vec<Topic>) {
    topics.sort_by(|left, right| {
        let left_fresh = left.quality == "fresh";
        let right_fresh = right.quality == "fresh";
        let left_updated = left.updated_at.parse::<jiff::Timestamp>().ok();
        let right_updated = right.updated_at.parse::<jiff::Timestamp>().ok();
        right_fresh
            .cmp(&left_fresh)
            .then_with(|| right_updated.cmp(&left_updated))
            .then_with(|| left.path.cmp(&right.path))
            .then_with(|| left.id.cmp(&right.id))
    });
    let mut retained_paths = BTreeSet::new();
    topics.retain(|topic| retained_paths.insert(topic.path.clone()));
    topics.truncate(MAX_RETAINED_TOPICS_PER_SOURCE);
}

fn topic_schema(topics: &[Topic]) -> BTreeSet<(String, String, String)> {
    topics
        .iter()
        .map(|topic| (topic.id.clone(), topic.path.clone(), topic.renderer.clone()))
        .collect()
}

async fn schedule_stale_transition(state: AppState, enrollment: EdgeEnrollment, deadline: i64) {
    let edge_device_id = enrollment.edge_device_id.clone();
    let task_edge_device_id = edge_device_id.clone();
    let task_state = state.clone();
    let task = tokio::spawn(async move {
        let delay_ms = deadline.saturating_sub(now_ms()).max(0) as u64;
        tokio::time::sleep(Duration::from_millis(delay_ms)).await;
        let _apply_guard = task_state.edge_heartbeat_apply_lock.lock().await;
        {
            let mut runtime = task_state.edge_heartbeat_runtime.lock().await;
            if runtime.deadlines.get(&task_edge_device_id) != Some(&deadline) {
                return;
            }
            runtime.deadlines.remove(&task_edge_device_id);
            runtime.stale_tasks.remove(&task_edge_device_id);
        }
        let durable_result = task_state
            .durable_catalog_mutation(|catalog| {
                let Some(current_enrollment) = catalog
                    .edge_enrollments
                    .get(&enrollment.edge_device_id)
                    .filter(|current| current.public_key == enrollment.public_key)
                    .cloned()
                else {
                    return Ok(());
                };
                if mark_enrollment_stale(catalog, &current_enrollment) {
                    catalog.bump_version();
                }
                Ok(())
            })
            .await;
        if durable_result.is_err() {
            // Availability state fails closed in memory even when durable storage is unavailable.
            // Startup recovery independently forces every enrolled Edge device offline.
            let mut catalog = task_state.catalog.write().await;
            if mark_enrollment_stale(&mut catalog, &enrollment) {
                catalog.bump_version();
            }
            tracing::error!("Failed to persist the stale Edge Agent transition");
        }
    });
    let mut runtime = state.edge_heartbeat_runtime.lock().await;
    if runtime.deadlines.get(&edge_device_id) != Some(&deadline) {
        task.abort();
        return;
    }
    if let Some(previous) = runtime
        .stale_tasks
        .insert(edge_device_id, task.abort_handle())
    {
        previous.abort();
    }
    drop(task);
}

fn mark_enrollment_stale(catalog: &mut Catalog, enrollment: &EdgeEnrollment) -> bool {
    let device_changed = if let Some(device) = catalog.devices.get_mut(&enrollment.device_id)
        && (device.status != "offline" || device.health != "unknown")
    {
        device.status = "offline".to_owned();
        device.health = "unknown".to_owned();
        device.state_version = device.state_version.saturating_add(1);
        true
    } else {
        false
    };
    let integration_changed = if let Some(integration) =
        catalog.integrations.get_mut(&enrollment.integration_id)
        && integration.status != "disconnected"
    {
        integration.status = "disconnected".to_owned();
        integration.resource_version = integration.resource_version.saturating_add(1);
        true
    } else {
        false
    };
    let mut changed = device_changed || integration_changed;
    for source_id in enrollment.source_ids.values() {
        if let Some(source) = catalog.data_sources.get_mut(source_id)
            && source.status != "pending"
        {
            source.status = "pending".to_owned();
            changed = true;
        }
        if let Some(topics) = catalog.topics_by_data_source.get_mut(source_id) {
            for topic in topics {
                if topic.quality != "stale" {
                    topic.quality = "stale".to_owned();
                    changed = true;
                }
            }
        }
    }
    let lease_count = catalog.leases.len();
    catalog
        .leases
        .retain(|_, lease| lease.device_id != enrollment.device_id);
    changed |= lease_count != catalog.leases.len();

    let affected_projects = catalog
        .device_assignments
        .values()
        .filter(|assignment| assignment.device_id == enrollment.device_id)
        .map(|assignment| assignment.project_id.clone())
        .collect::<BTreeSet<_>>();
    for project_id in affected_projects {
        let previous = catalog
            .projects
            .get(&project_id)
            .map(|project| project.online_device_count);
        catalog.refresh_project_counts(&project_id);
        changed |= catalog
            .projects
            .get(&project_id)
            .map(|project| project.online_device_count)
            != previous;
    }
    changed
}

fn materialize_topic(
    source_id: &str,
    device_id: &str,
    topic: &EdgeTopic,
    updated_at: &str,
) -> Topic {
    let mut hasher = Sha256::new();
    hasher.update(source_id.as_bytes());
    hasher.update([0]);
    hasher.update(topic.path.as_bytes());
    let digest = hasher.finalize();
    Topic {
        id: format!("topic-{}", hex_prefix(&digest, 12)),
        data_source_id: source_id.to_owned(),
        device_id: device_id.to_owned(),
        path: topic.path.clone(),
        label: topic.label.clone(),
        renderer: renderer(topic.renderer_hint.as_deref(), &topic.path).to_owned(),
        quality: "fresh".to_owned(),
        value: None,
        unit: None,
        message: None,
        samples: None,
        updated_at: updated_at.to_owned(),
    }
}

fn validate_heartbeat(heartbeat: &EdgeHeartbeat) -> ApiResult<()> {
    if heartbeat.schema_version != 1
        || heartbeat.sequence == 0
        || Uuid::parse_str(&heartbeat.boot_id).is_err()
        || !valid_token(&heartbeat.device_id, 64)
        || !matches!(
            heartbeat.agent_health.as_str(),
            "healthy" | "degraded" | "stale"
        )
        || heartbeat.adapters.len() > MAX_ADAPTERS
    {
        return Err(ApiError::bad_request(
            "The Edge Agent heartbeat is invalid.",
        ));
    }
    let mut total_observations = 0;
    for adapter in &heartbeat.adapters {
        let active = matches!(adapter.state.as_str(), "healthy" | "degraded");
        if !valid_token(&adapter.id, 64)
            || !matches!(adapter.kind.as_str(), "ros2_dds" | "mavlink")
            || !matches!(
                adapter.state.as_str(),
                "starting" | "healthy" | "degraded" | "stale" | "stopped"
            )
            || adapter
                .last_success_at_ms
                .is_some_and(|value| value > heartbeat.sent_at_ms)
        {
            return Err(ApiError::bad_request(
                "The Edge Agent adapter status is invalid.",
            ));
        }
        if active {
            let Some(last_success_at_ms) = adapter.last_success_at_ms else {
                return Err(ApiError::bad_request(
                    "The Edge Agent adapter status is invalid.",
                ));
            };
            let freshest_observation = adapter
                .observations
                .iter()
                .map(|observation| observation.observed_at_ms)
                .max();
            if heartbeat.sent_at_ms.saturating_sub(last_success_at_ms) > STALE_AFTER_MS
                || freshest_observation.is_some_and(|observed_at| observed_at > last_success_at_ms)
            {
                return Err(ApiError::bad_request(
                    "The Edge Agent adapter status is invalid.",
                ));
            }
        }
        total_observations += adapter.observations.len();
        if total_observations > MAX_OBSERVATIONS {
            return Err(ApiError::bad_request(
                "The Edge Agent inventory is too large.",
            ));
        }
        for observation in &adapter.observations {
            validate_observation(adapter, observation, heartbeat.sent_at_ms)?;
        }
    }
    Ok(())
}

fn validate_observation(
    adapter: &EdgeAdapterHeartbeat,
    observation: &EdgeObservation,
    sent_at_ms: i64,
) -> ApiResult<()> {
    if !valid_text(&observation.identity, 192)
        || !valid_text(&observation.display_name, 96)
        || !matches!(
            observation.device_kind.as_str(),
            "robot" | "drone" | "vehicle" | "camera" | "gateway"
        )
        || !matches!(observation.trust.as_str(), "observed" | "authenticated")
        || observation.observed_at_ms > sent_at_ms
        || observation.expires_at_ms <= observation.observed_at_ms
        || observation.sources.is_empty()
        || observation.sources.len() > MAX_SOURCES
        || observation.metadata.len() > MAX_METADATA
        || observation
            .metadata
            .iter()
            .any(|(key, value)| !valid_token(key, 48) || !valid_text(value, 256))
    {
        return Err(ApiError::bad_request(
            "The Edge Agent observation is invalid.",
        ));
    }
    for source in &observation.sources {
        let expected_protocol = if adapter.kind == "ros2_dds" {
            "ros2"
        } else {
            "mavlink"
        };
        if !valid_token(&source.id, 96)
            || !valid_text(&source.label, 96)
            || source.protocol != expected_protocol
            || !matches!(
                source.category.as_str(),
                "camera" | "spatial" | "telemetry" | "state" | "log"
            )
            || !matches!(source.status.as_str(), "metadata_only" | "ready")
            || source.topics.len() > MAX_TOPICS
            || source
                .renderer_hint
                .as_deref()
                .is_some_and(|hint| !valid_renderer(hint))
        {
            return Err(ApiError::bad_request("The Edge Agent source is invalid."));
        }
        for topic in &source.topics {
            if !valid_path(&topic.path)
                || !valid_text(&topic.label, 96)
                || topic
                    .message_type
                    .as_ref()
                    .is_some_and(|value| !valid_text(value, 192))
                || topic
                    .renderer_hint
                    .as_deref()
                    .is_some_and(|hint| !valid_renderer(hint))
            {
                return Err(ApiError::bad_request("The Edge Agent topic is invalid."));
            }
            if let Some(qos) = &topic.qos
                && (!matches!(
                    qos.reliability.as_str(),
                    "reliable" | "best_effort" | "unknown"
                ) || !matches!(
                    qos.durability.as_str(),
                    "volatile" | "transient_local" | "unknown"
                ) || qos.history_depth.is_some_and(|depth| depth > 100_000))
            {
                return Err(ApiError::bad_request(
                    "The Edge Agent topic QoS is invalid.",
                ));
            }
        }
    }
    Ok(())
}

fn verify_bearer(headers: &HeaderMap, expected: &Arc<Vec<u8>>) -> ApiResult<()> {
    let authorization = headers
        .get(AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .ok_or_else(|| ApiError::unauthorized("Edge Agent authentication failed."))?;
    let expected_key = hmac::Key::new(hmac::HMAC_SHA256, expected);
    let provided_key = hmac::Key::new(hmac::HMAC_SHA256, authorization.as_bytes());
    let provided_tag = hmac::sign(&provided_key, b"rms-edge-bearer-v1");
    hmac::verify(&expected_key, b"rms-edge-bearer-v1", provided_tag.as_ref())
        .map_err(|_error| ApiError::unauthorized("Edge Agent authentication failed."))
}

fn header_text<'a>(headers: &'a HeaderMap, name: &'static str) -> ApiResult<&'a str> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .filter(|value| !value.is_empty() && value.len() <= 256)
        .ok_or_else(|| ApiError::unauthorized("The Edge Agent signature is incomplete."))
}

fn decode_exact<const N: usize>(value: &str, _field: &str) -> ApiResult<[u8; N]> {
    BASE64_URL_SAFE_NO_PAD
        .decode(value)
        .ok()
        .and_then(|bytes| bytes.try_into().ok())
        .ok_or_else(|| ApiError::unauthorized("The Edge Agent signature is invalid."))
}

fn hex_sha256(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    hex_prefix(&digest, digest.len())
}

fn hex_prefix(bytes: &[u8], count: usize) -> String {
    bytes
        .iter()
        .take(count)
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn valid_token(value: &str, maximum: usize) -> bool {
    !value.is_empty()
        && value.len() <= maximum
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':'))
}

fn valid_text(value: &str, maximum: usize) -> bool {
    !value.is_empty() && value.len() <= maximum && !value.chars().any(char::is_control)
}

fn valid_path(value: &str) -> bool {
    value.starts_with('/')
        && value.len() <= 256
        && !value.contains("..")
        && !value.chars().any(char::is_control)
}

fn valid_renderer(value: &str) -> bool {
    matches!(
        value,
        "image" | "point_cloud" | "spatial" | "transform3d" | "plot" | "state" | "log" | "raw"
    )
}

fn renderer(hint: Option<&str>, _path: &str) -> &'static str {
    match hint {
        Some("image") => "camera",
        Some("point_cloud" | "spatial") => "spatial",
        Some("transform3d") => "transform3d",
        Some("plot") => "timeseries",
        Some("state") => "state",
        Some("log") => "log",
        _ => "raw",
    }
}

#[cfg(test)]
mod tests {
    use axum::{
        body::Body,
        http::{Request, StatusCode},
    };
    use ring::signature::{Ed25519KeyPair, KeyPair as _};
    use tower::ServiceExt as _;

    use super::*;
    use crate::domain::{ControlLease, EdgeEnrollment};

    const TOKEN: &[u8] = b"edge-heartbeat-test-token-32-bytes";

    #[test]
    fn renderer_contract_preserves_raw_and_transform3d_hints() {
        for hint in [
            "image",
            "point_cloud",
            "spatial",
            "transform3d",
            "plot",
            "state",
            "log",
            "raw",
        ] {
            assert!(valid_renderer(hint), "expected `{hint}` to be valid");
        }
        assert!(!valid_renderer("map"));
        assert!(!valid_renderer("text_log"));

        assert_eq!(
            renderer(Some("transform3d"), "/custom/frames"),
            "transform3d"
        );
        assert_eq!(renderer(Some("raw"), "/camera/info"), "raw");
        assert_eq!(renderer(None, "/camera/info"), "raw");
        assert_eq!(renderer(Some("point_cloud"), "/lidar/points"), "spatial");
        assert_eq!(renderer(Some("image"), "/front/image"), "camera");
    }

    async fn enrolled_state(seed: [u8; 32]) -> (AppState, Ed25519KeyPair, String) {
        let key_pair = Ed25519KeyPair::from_seed_unchecked(&seed).expect("test seed is valid");
        let public_key: [u8; 32] = key_pair
            .public_key()
            .as_ref()
            .try_into()
            .expect("Ed25519 public key has 32 bytes");
        let edge_device_id = derive_device_id(&public_key);
        let state = AppState::fixture().with_edge_heartbeat_token(TOKEN.to_vec());
        {
            let mut catalog = state.catalog.write().await;
            catalog.edge_enrollments.insert(
                edge_device_id.clone(),
                EdgeEnrollment {
                    edge_device_id: edge_device_id.clone(),
                    public_key: BASE64_URL_SAFE_NO_PAD.encode(public_key),
                    organization_id: "org-rms".to_owned(),
                    integration_id: "integration-logistics".to_owned(),
                    device_id: "robot-07".to_owned(),
                    source_ids: BTreeMap::from([(
                        "ros2-graph".to_owned(),
                        "robot-07-source".to_owned(),
                    )]),
                    organization_trusted: true,
                    created_at: iso_from_millis(now_ms()),
                },
            );
            catalog
                .topics_by_data_source
                .insert("robot-07-source".to_owned(), Vec::new());
            catalog
                .data_sources
                .get_mut("robot-07-source")
                .unwrap()
                .topic_ids
                .clear();
        }
        (state, key_pair, edge_device_id)
    }

    fn heartbeat_body(
        key_pair: &Ed25519KeyPair,
        device_id: &str,
        sent_at_ms: i64,
        sequence: u64,
    ) -> Vec<u8> {
        serde_json::to_vec(&heartbeat_value(key_pair, device_id, sent_at_ms, sequence))
            .expect("heartbeat serializes")
    }

    fn heartbeat_value(
        key_pair: &Ed25519KeyPair,
        device_id: &str,
        sent_at_ms: i64,
        sequence: u64,
    ) -> Value {
        json!({
            "schemaVersion": 1,
            "deviceId": device_id,
            "publicKey": BASE64_URL_SAFE_NO_PAD.encode(key_pair.public_key().as_ref()),
            "bootId": "c873a70b-0b4c-49d2-a402-8f1f21ad85cb",
            "sentAtMs": sent_at_ms,
            "sequence": sequence,
            "agentHealth": "healthy",
            "adapters": [{
                "id": "ros-main",
                "kind": "ros2_dds",
                "state": "healthy",
                "lastSuccessAtMs": sent_at_ms,
                "observations": [{
                    "identity": "robot-07",
                    "displayName": "Robot 07",
                    "deviceKind": "robot",
                    "trust": "authenticated",
                    "observedAtMs": sent_at_ms,
                    "expiresAtMs": sent_at_ms + 5_000,
                    "sources": [{
                        "id": "ros2:telemetry",
                        "label": "Telemetry",
                        "category": "telemetry",
                        "protocol": "ros2",
                        "status": "metadata_only",
                        "rendererHint": "plot",
                        "topics": [{
                            "path": "/vehicle/speed",
                            "label": "Speed",
                            "messageType": "std_msgs/msg/Float64",
                            "qos": {
                                "reliability": "reliable",
                                "durability": "volatile",
                                "historyDepth": 10
                            },
                            "rendererHint": "plot"
                        }]
                    }],
                    "metadata": {"domain_id": "0"}
                }]
            }]
        })
    }

    fn request(
        key_pair: &Ed25519KeyPair,
        body: Vec<u8>,
        timestamp: i64,
        nonce_byte: u8,
    ) -> Request<Body> {
        let nonce = BASE64_URL_SAFE_NO_PAD.encode([nonce_byte; 16]);
        let canonical = format!(
            "rms-heartbeat-v1\n{timestamp}\n{nonce}\n{}",
            hex_sha256(&body)
        );
        let signature = BASE64_URL_SAFE_NO_PAD.encode(key_pair.sign(canonical.as_bytes()).as_ref());
        Request::post("/api/v1/edge-agents/heartbeats")
            .header("content-type", "application/json")
            .header(
                "authorization",
                format!("Bearer {}", String::from_utf8_lossy(TOKEN)),
            )
            .header("x-rms-timestamp", timestamp.to_string())
            .header("x-rms-nonce", nonce)
            .header("x-rms-signature", signature)
            .body(Body::from(body))
            .expect("request is valid")
    }

    fn generated_topics(prefix: &str, count: usize) -> Value {
        Value::Array(
            (0..count)
                .map(|index| {
                    json!({
                        "path": format!("/{prefix}/{index:04}"),
                        "label": format!("Topic {index}"),
                        "messageType": null,
                        "qos": null,
                        "rendererHint": "log"
                    })
                })
                .collect(),
        )
    }

    #[tokio::test]
    async fn signed_enrolled_heartbeat_updates_topics_then_expires() {
        let (state, key_pair, device_id) = enrolled_state([3; 32]).await;
        let timestamp = now_ms();
        let body = heartbeat_body(&key_pair, &device_id, timestamp, 1);
        let response = super::router()
            .with_state(state.clone())
            .oneshot(request(&key_pair, body, timestamp, 1))
            .await
            .expect("router responds");
        assert_eq!(response.status(), StatusCode::ACCEPTED);
        {
            let catalog = state.catalog.read().await;
            assert_eq!(catalog.devices["robot-07"].status, "online");
            assert_eq!(
                catalog.integrations["integration-logistics"].status,
                "connected"
            );
            assert_eq!(catalog.data_sources["robot-07-source"].status, "pending");
            assert_eq!(
                catalog.topics_by_data_source["robot-07-source"][0].path,
                "/vehicle/speed"
            );
            assert!(catalog.live_sessions.is_empty());
            assert!(catalog.leases.is_empty());
            assert!(catalog.command_receipts.is_empty());
        }
        state.catalog.write().await.leases.insert(
            "edge-test-lease".to_owned(),
            ControlLease {
                id: "edge-test-lease".to_owned(),
                live_session_id: "edge-test-session".to_owned(),
                device_id: "robot-07".to_owned(),
                holder_id: "operator".to_owned(),
                holder_name: "Operator".to_owned(),
                expires_at: "2099-01-01T00:00:00Z".to_owned(),
                epoch: 1,
                expires_at_ms: i64::MAX,
            },
        );
        tokio::time::sleep(Duration::from_millis(700)).await;
        let catalog = state.catalog.read().await;
        assert_eq!(catalog.devices["robot-07"].status, "offline");
        assert_eq!(
            catalog.integrations["integration-logistics"].status,
            "disconnected"
        );
        assert_eq!(catalog.data_sources["robot-07-source"].status, "pending");
        assert_eq!(
            catalog.topics_by_data_source["robot-07-source"][0].quality,
            "stale"
        );
        assert!(catalog.leases.is_empty());
    }

    #[tokio::test]
    async fn mapping_version_changes_only_when_topic_schema_changes() {
        let (state, key_pair, device_id) = enrolled_state([14; 32]).await;
        let router = super::router().with_state(state.clone());
        let initial_mapping_version =
            state.catalog.read().await.data_sources["robot-07-source"].mapping_version;

        let first_timestamp = now_ms();
        let first = router
            .clone()
            .oneshot(request(
                &key_pair,
                heartbeat_body(&key_pair, &device_id, first_timestamp, 1),
                first_timestamp,
                61,
            ))
            .await
            .unwrap();
        assert_eq!(first.status(), StatusCode::ACCEPTED);
        let first_mapping_version =
            state.catalog.read().await.data_sources["robot-07-source"].mapping_version;
        assert_eq!(
            first_mapping_version,
            initial_mapping_version.saturating_add(1)
        );

        let repeated_timestamp = now_ms();
        let repeated = router
            .clone()
            .oneshot(request(
                &key_pair,
                heartbeat_body(&key_pair, &device_id, repeated_timestamp, 2),
                repeated_timestamp,
                62,
            ))
            .await
            .unwrap();
        assert_eq!(repeated.status(), StatusCode::ACCEPTED);
        assert_eq!(
            state.catalog.read().await.data_sources["robot-07-source"].mapping_version,
            first_mapping_version
        );

        let changed_timestamp = now_ms();
        let mut changed = heartbeat_value(&key_pair, &device_id, changed_timestamp, 3);
        changed["adapters"][0]["observations"][0]["sources"][0]["topics"][0]["rendererHint"] =
            json!("image");
        let changed = router
            .clone()
            .oneshot(request(
                &key_pair,
                serde_json::to_vec(&changed).unwrap(),
                changed_timestamp,
                63,
            ))
            .await
            .unwrap();
        assert_eq!(changed.status(), StatusCode::ACCEPTED);
        let changed_mapping_version =
            state.catalog.read().await.data_sources["robot-07-source"].mapping_version;
        assert_eq!(
            changed_mapping_version,
            first_mapping_version.saturating_add(1)
        );

        let added_timestamp = now_ms();
        let mut added = heartbeat_value(&key_pair, &device_id, added_timestamp, 4);
        added["adapters"][0]["observations"][0]["sources"][0]["topics"]
            .as_array_mut()
            .unwrap()
            .push(json!({
                "path": "/vehicle/heading",
                "label": "Heading",
                "messageType": "std_msgs/msg/Float64",
                "qos": null,
                "rendererHint": "plot"
            }));
        let added = router
            .oneshot(request(
                &key_pair,
                serde_json::to_vec(&added).unwrap(),
                added_timestamp,
                64,
            ))
            .await
            .unwrap();
        assert_eq!(added.status(), StatusCode::ACCEPTED);
        assert_eq!(
            state.catalog.read().await.data_sources["robot-07-source"].mapping_version,
            changed_mapping_version.saturating_add(1)
        );
    }

    #[tokio::test]
    async fn nonce_and_sequence_replays_fail_closed() {
        let (state, key_pair, device_id) = enrolled_state([4; 32]).await;
        let timestamp = now_ms();
        let router = super::router().with_state(state);
        let first = router
            .clone()
            .oneshot(request(
                &key_pair,
                heartbeat_body(&key_pair, &device_id, timestamp, 1),
                timestamp,
                1,
            ))
            .await
            .unwrap();
        assert_eq!(first.status(), StatusCode::ACCEPTED);
        let replay = router
            .oneshot(request(
                &key_pair,
                heartbeat_body(&key_pair, &device_id, timestamp, 1),
                timestamp,
                2,
            ))
            .await
            .unwrap();
        assert_eq!(replay.status(), StatusCode::CONFLICT);
    }

    #[tokio::test]
    async fn unknown_key_and_stale_timestamp_are_rejected() {
        let (state, key_pair, device_id) = enrolled_state([5; 32]).await;
        let other = Ed25519KeyPair::from_seed_unchecked(&[6; 32]).unwrap();
        let timestamp = now_ms();
        let wrong_key = super::router()
            .with_state(state.clone())
            .oneshot(request(
                &other,
                heartbeat_body(
                    &other,
                    &derive_device_id(other.public_key().as_ref().try_into().unwrap()),
                    timestamp,
                    1,
                ),
                timestamp,
                1,
            ))
            .await
            .unwrap();
        assert_eq!(wrong_key.status(), StatusCode::FORBIDDEN);

        let stale = timestamp - 31_000;
        let stale_response = super::router()
            .with_state(state)
            .oneshot(request(
                &key_pair,
                heartbeat_body(&key_pair, &device_id, stale, 1),
                stale,
                2,
            ))
            .await
            .unwrap();
        assert_eq!(stale_response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn active_adapter_requires_a_fresh_success_after_its_observations() {
        let (state, key_pair, device_id) = enrolled_state([11; 32]).await;
        let timestamp = now_ms();
        let mut missing_success = heartbeat_value(&key_pair, &device_id, timestamp, 1);
        missing_success["adapters"][0]["lastSuccessAtMs"] = Value::Null;
        let router = super::router().with_state(state);
        let missing_response = router
            .clone()
            .oneshot(request(
                &key_pair,
                serde_json::to_vec(&missing_success).unwrap(),
                timestamp,
                15,
            ))
            .await
            .unwrap();
        assert_eq!(missing_response.status(), StatusCode::BAD_REQUEST);

        let mut older_success = heartbeat_value(&key_pair, &device_id, timestamp, 1);
        older_success["adapters"][0]["lastSuccessAtMs"] = json!(timestamp - 1);
        let older_response = router
            .oneshot(request(
                &key_pair,
                serde_json::to_vec(&older_success).unwrap(),
                timestamp,
                16,
            ))
            .await
            .unwrap();
        assert_eq!(older_response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn stale_deadline_and_last_seen_use_server_receive_time() {
        let (state, key_pair, device_id) = enrolled_state([7; 32]).await;
        let sent_at_ms = now_ms() - 20_000;
        let before_receive = now_ms();
        let response = super::router()
            .with_state(state.clone())
            .oneshot(request(
                &key_pair,
                heartbeat_body(&key_pair, &device_id, sent_at_ms, 1),
                sent_at_ms,
                7,
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::ACCEPTED);
        {
            let catalog = state.catalog.read().await;
            assert_eq!(catalog.devices["robot-07"].status, "online");
            assert_ne!(
                catalog.devices["robot-07"].last_seen_at,
                iso_from_millis(sent_at_ms)
            );
        }
        {
            let runtime = state.edge_heartbeat_runtime.lock().await;
            assert!(runtime.deadlines[&device_id] >= before_receive + STALE_AFTER_MS);
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(
            state.catalog.read().await.devices["robot-07"].status,
            "online"
        );
        tokio::time::sleep(Duration::from_millis(600)).await;
        assert_eq!(
            state.catalog.read().await.devices["robot-07"].status,
            "offline"
        );
    }

    #[tokio::test]
    async fn disappeared_and_inactive_adapter_topics_stay_stale() {
        let (state, key_pair, device_id) = enrolled_state([8; 32]).await;
        let timestamp = now_ms();
        let mut first = heartbeat_value(&key_pair, &device_id, timestamp, 1);
        first["adapters"][0]["observations"][0]["sources"][0]["topics"]
            .as_array_mut()
            .unwrap()
            .push(json!({
                "path": "/vehicle/temperature",
                "label": "Temperature",
                "messageType": "std_msgs/msg/Float64",
                "qos": null,
                "rendererHint": "plot"
            }));
        let router = super::router().with_state(state.clone());
        let first_response = router
            .clone()
            .oneshot(request(
                &key_pair,
                serde_json::to_vec(&first).unwrap(),
                timestamp,
                8,
            ))
            .await
            .unwrap();
        assert_eq!(first_response.status(), StatusCode::ACCEPTED);

        let second_timestamp = now_ms();
        let second_response = router
            .clone()
            .oneshot(request(
                &key_pair,
                heartbeat_body(&key_pair, &device_id, second_timestamp, 2),
                second_timestamp,
                9,
            ))
            .await
            .unwrap();
        assert_eq!(second_response.status(), StatusCode::ACCEPTED);
        {
            let catalog = state.catalog.read().await;
            let topics = &catalog.topics_by_data_source["robot-07-source"];
            assert_eq!(topics.len(), 2);
            assert_eq!(
                topics
                    .iter()
                    .find(|topic| topic.path == "/vehicle/speed")
                    .unwrap()
                    .quality,
                "fresh"
            );
            assert_eq!(
                topics
                    .iter()
                    .find(|topic| topic.path == "/vehicle/temperature")
                    .unwrap()
                    .quality,
                "stale"
            );
        }

        let third_timestamp = now_ms();
        let mut inactive = heartbeat_value(&key_pair, &device_id, third_timestamp, 3);
        inactive["adapters"][0]["state"] = json!("stale");
        inactive["adapters"][0]["lastSuccessAtMs"] = Value::Null;
        inactive["adapters"][0]["observations"][0]["sources"][0]["topics"] = json!([{
            "path": "/should/not/appear",
            "label": "Ignored",
            "messageType": null,
            "qos": null,
            "rendererHint": "log"
        }]);
        let inactive_response = router
            .oneshot(request(
                &key_pair,
                serde_json::to_vec(&inactive).unwrap(),
                third_timestamp,
                10,
            ))
            .await
            .unwrap();
        assert_eq!(inactive_response.status(), StatusCode::ACCEPTED);
        let catalog = state.catalog.read().await;
        let topics = &catalog.topics_by_data_source["robot-07-source"];
        assert!(topics.iter().all(|topic| topic.quality == "stale"));
        assert!(
            topics
                .iter()
                .all(|topic| topic.path != "/should/not/appear")
        );
    }

    #[tokio::test]
    async fn catalog_protocol_mismatch_rolls_back_catalog_and_replay_state() {
        let (state, key_pair, device_id) = enrolled_state([9; 32]).await;
        let previous_version;
        {
            let mut catalog = state.catalog.write().await;
            catalog.devices.get_mut("robot-07").unwrap().status = "offline".to_owned();
            catalog.devices.get_mut("robot-07").unwrap().health = "unknown".to_owned();
            catalog
                .integrations
                .get_mut("integration-logistics")
                .unwrap()
                .status = "disconnected".to_owned();
            catalog
                .data_sources
                .get_mut("robot-07-source")
                .unwrap()
                .protocol = "mavlink".to_owned();
            previous_version = catalog.snapshot_version;
        }
        let timestamp = now_ms();
        let body = heartbeat_body(&key_pair, &device_id, timestamp, 1);
        let rejected = super::router()
            .with_state(state.clone())
            .oneshot(request(&key_pair, body, timestamp, 11))
            .await
            .unwrap();
        assert_eq!(rejected.status(), StatusCode::FORBIDDEN);
        {
            let catalog = state.catalog.read().await;
            assert_eq!(catalog.snapshot_version, previous_version);
            assert_eq!(catalog.devices["robot-07"].status, "offline");
            assert_eq!(
                catalog.integrations["integration-logistics"].status,
                "disconnected"
            );
        }
        {
            let runtime = state.edge_heartbeat_runtime.lock().await;
            assert!(runtime.sequences.is_empty());
            assert!(runtime.deadlines.is_empty());
        }

        state
            .catalog
            .write()
            .await
            .data_sources
            .get_mut("robot-07-source")
            .unwrap()
            .protocol = "ros2".to_owned();
        let accepted_timestamp = now_ms();
        let accepted = super::router()
            .with_state(state.clone())
            .oneshot(request(
                &key_pair,
                heartbeat_body(&key_pair, &device_id, accepted_timestamp, 1),
                accepted_timestamp,
                12,
            ))
            .await
            .unwrap();
        assert_eq!(accepted.status(), StatusCode::ACCEPTED);
        assert_eq!(
            state.catalog.read().await.devices["robot-07"].status,
            "online"
        );
    }

    #[tokio::test]
    async fn concurrent_sequences_cannot_commit_out_of_order() {
        let (state, key_pair, device_id) = enrolled_state([10; 32]).await;
        let timestamp = now_ms();
        let mut older = heartbeat_value(&key_pair, &device_id, timestamp, 1);
        older["adapters"][0]["observations"][0]["sources"][0]["topics"][0]["label"] =
            json!("Older");
        let mut newer = heartbeat_value(&key_pair, &device_id, timestamp, 2);
        newer["adapters"][0]["observations"][0]["sources"][0]["topics"][0]["label"] =
            json!("Newer");
        let older_request = request(
            &key_pair,
            serde_json::to_vec(&older).unwrap(),
            timestamp,
            13,
        );
        let newer_request = request(
            &key_pair,
            serde_json::to_vec(&newer).unwrap(),
            timestamp,
            14,
        );
        let router = super::router().with_state(state.clone());
        let (older_response, newer_response) = tokio::join!(
            router.clone().oneshot(older_request),
            router.oneshot(newer_request)
        );
        let older_status = older_response.unwrap().status();
        let newer_status = newer_response.unwrap().status();
        assert!(matches!(
            older_status,
            StatusCode::ACCEPTED | StatusCode::CONFLICT
        ));
        assert_eq!(newer_status, StatusCode::ACCEPTED);
        assert_eq!(
            state.catalog.read().await.topics_by_data_source["robot-07-source"][0].label,
            "Newer"
        );
    }

    #[tokio::test]
    async fn aggregate_topic_limit_cannot_be_bypassed_and_rolls_back() {
        let (state, key_pair, device_id) = enrolled_state([12; 32]).await;
        let timestamp = now_ms();
        let (previous_version, previous_mapping_version, previous_source_status) = {
            let catalog = state.catalog.read().await;
            (
                catalog.snapshot_version,
                catalog.data_sources["robot-07-source"].mapping_version,
                catalog.data_sources["robot-07-source"].status.clone(),
            )
        };
        let mut oversized = heartbeat_value(&key_pair, &device_id, timestamp, 1);
        oversized["adapters"][0]["observations"][0]["sources"][0]["topics"] =
            generated_topics("aggregate/a", 200);
        let mut second_observation = oversized["adapters"][0]["observations"][0].clone();
        second_observation["identity"] = json!("robot-07-secondary-observation");
        second_observation["sources"][0]["id"] = json!("ros2:secondary");
        second_observation["sources"][0]["topics"] = generated_topics("aggregate/b", 200);
        oversized["adapters"][0]["observations"]
            .as_array_mut()
            .unwrap()
            .push(second_observation);

        let router = super::router().with_state(state.clone());
        let rejected = router
            .clone()
            .oneshot(request(
                &key_pair,
                serde_json::to_vec(&oversized).unwrap(),
                timestamp,
                17,
            ))
            .await
            .unwrap();
        assert_eq!(rejected.status(), StatusCode::BAD_REQUEST);
        {
            let catalog = state.catalog.read().await;
            assert_eq!(catalog.snapshot_version, previous_version);
            assert!(catalog.topics_by_data_source["robot-07-source"].is_empty());
            assert!(catalog.data_sources["robot-07-source"].topic_ids.is_empty());
            assert_eq!(
                catalog.data_sources["robot-07-source"].mapping_version,
                previous_mapping_version
            );
            assert_eq!(
                catalog.data_sources["robot-07-source"].status,
                previous_source_status
            );
        }
        {
            let runtime = state.edge_heartbeat_runtime.lock().await;
            assert!(runtime.sequences.is_empty());
            assert!(runtime.deadlines.is_empty());
        }

        let retry_timestamp = now_ms();
        let accepted = router
            .oneshot(request(
                &key_pair,
                heartbeat_body(&key_pair, &device_id, retry_timestamp, 1),
                retry_timestamp,
                18,
            ))
            .await
            .unwrap();
        assert_eq!(accepted.status(), StatusCode::ACCEPTED);
    }

    #[tokio::test]
    async fn topic_churn_is_deterministically_bounded_with_fresh_topics_preserved() {
        let (state, key_pair, device_id) = enrolled_state([13; 32]).await;
        let router = super::router().with_state(state.clone());
        let mut previous_response_at = now_ms();
        for generation in 0..8_u64 {
            while now_ms() <= previous_response_at {
                tokio::task::yield_now().await;
            }
            let timestamp = now_ms();
            let mut heartbeat = heartbeat_value(&key_pair, &device_id, timestamp, generation + 1);
            heartbeat["adapters"][0]["observations"][0]["sources"][0]["topics"] =
                generated_topics(&format!("churn/{generation}"), MAX_TOPICS);
            let response = router
                .clone()
                .oneshot(request(
                    &key_pair,
                    serde_json::to_vec(&heartbeat).unwrap(),
                    timestamp,
                    32 + u8::try_from(generation).unwrap(),
                ))
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::ACCEPTED);
            previous_response_at = now_ms();
        }

        let catalog = state.catalog.read().await;
        let topics = &catalog.topics_by_data_source["robot-07-source"];
        assert_eq!(topics.len(), MAX_RETAINED_TOPICS_PER_SOURCE);
        assert_eq!(
            catalog.data_sources["robot-07-source"].topic_ids.len(),
            MAX_RETAINED_TOPICS_PER_SOURCE
        );
        let fresh = topics
            .iter()
            .filter(|topic| topic.quality == "fresh")
            .collect::<Vec<_>>();
        assert_eq!(fresh.len(), MAX_TOPICS);
        assert!(
            fresh
                .iter()
                .all(|topic| topic.path.starts_with("/churn/7/"))
        );
        let stale = topics
            .iter()
            .filter(|topic| topic.quality == "stale")
            .collect::<Vec<_>>();
        assert_eq!(stale.len(), MAX_TOPICS);
        assert!(
            stale
                .iter()
                .all(|topic| topic.path.starts_with("/churn/6/"))
        );
    }
}
