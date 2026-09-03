use std::{
    collections::{BTreeMap, BTreeSet},
    net::{IpAddr, Ipv4Addr, Ipv6Addr},
    sync::Arc,
    time::Duration,
};

use axum::{
    Json, Router,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    routing::{get, post},
};
use base64::{Engine as _, prelude::BASE64_URL_SAFE_NO_PAD};
use sha2::{Digest as _, Sha256};
use tokio::sync::OwnedSemaphorePermit;

use crate::{
    AppState,
    domain::{
        AccessMode, ApproveNetworkCandidateInput, CandidateVerification,
        CandidateVerificationStatus, DataAssignment, DataSource, DataVisibility, Device,
        DeviceAssignment, DiscoveryCandidate, DiscoveryCandidateStatus, DiscoverySessionStatus,
        DiscoverySourceStatus, EdgeEnrollment, Integration, NetworkDiscoverySession,
        NetworkDiscoverySnapshot, NetworkLinkReceipt, StartNetworkDiscoveryRequest,
        SuggestedDevice, VerifiedDiscoverySource,
    },
    error::{ApiError, ApiResult},
    network_discovery::{
        DiscoveryCancellation, DiscoveryObservation, EXPIRED_SESSION_RETENTION,
        MAX_CANDIDATES_PER_SESSION, MAX_SOURCES_PER_CANDIDATE, StoredApproval, StoredCandidate,
        StoredDiscoverySession, StoredStartReceipt, StoredVerification, VerificationAttemptGuard,
        same_source_pin,
    },
    state::{iso_from_millis, new_id, now_iso, now_ms},
};

const MAX_ORGANIZATION_ID_LEN: usize = 128;
const MAX_DISPLAY_NAME_LEN: usize = 128;
const MAX_FINGERPRINT_LEN: usize = 256;
const MAX_SOURCE_ID_LEN: usize = 64;
const MAX_SOURCE_LABEL_LEN: usize = 128;
const MAX_ENDPOINT_PATH_LEN: usize = 256;

pub(super) fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/api/v1/network-discovery-sessions",
            post(start_discovery),
        )
        .route(
            "/api/v1/network-discovery-sessions/{session_id}",
            get(get_discovery).delete(cancel_discovery),
        )
        .route(
            "/api/v1/network-discovery-sessions/{session_id}/candidates/{candidate_id}/verification",
            post(verify_candidate),
        )
        .route(
            "/api/v1/network-discovery-sessions/{session_id}/candidates/{candidate_id}/approval",
            post(approve_candidate),
        )
}

async fn start_discovery(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<StartNetworkDiscoveryRequest>,
) -> ApiResult<(StatusCode, Json<NetworkDiscoverySession>)> {
    let idempotency_key = require_mutation_headers(&headers)?;
    validate_text(
        "organizationId",
        &request.organization_id,
        MAX_ORGANIZATION_ID_LEN,
    )?;

    let now_ms = now_ms();
    {
        let mut discovery = state.network_discovery.write().await;
        discovery.expire_and_purge(now_ms);
        if let Some(receipt) = discovery.start_receipts.get(&idempotency_key) {
            if receipt.organization_id != request.organization_id {
                return Err(ApiError::conflict(
                    "Idempotency-Key was already used for another discovery request.",
                ));
            }
            let session = discovery
                .sessions
                .get(&receipt.session_id)
                .map(|session| session.dto.clone())
                .ok_or_else(|| {
                    ApiError::internal("Network discovery idempotency state is inconsistent.")
                })?;
            return Ok((StatusCode::ACCEPTED, Json(session)));
        }
    }

    let scan_permit = state
        .discovery_slots
        .clone()
        .try_acquire_owned()
        .map_err(|_busy| {
            ApiError::too_many_requests("Another network discovery search is already running.")
        })?;

    let session_id = new_id("network-discovery");
    let cancellation = DiscoveryCancellation::new();
    let session_ttl_ms = duration_millis(state.discovery_session_ttl);
    let expires_at_ms = now_ms.saturating_add(session_ttl_ms);
    let purge_at_ms = expires_at_ms.saturating_add(duration_millis(EXPIRED_SESSION_RETENTION));
    let session = NetworkDiscoverySession {
        id: session_id.clone(),
        status: DiscoverySessionStatus::Searching,
        candidate_count: 0,
        started_at: iso_from_millis(now_ms),
        expires_at: iso_from_millis(expires_at_ms),
        resource_version: 1,
    };

    {
        let mut discovery = state.network_discovery.write().await;
        discovery.expire_and_purge(now_ms);
        if let Some(receipt) = discovery.start_receipts.get(&idempotency_key) {
            if receipt.organization_id != request.organization_id {
                return Err(ApiError::conflict(
                    "Idempotency-Key was already used for another discovery request.",
                ));
            }
            let session = discovery
                .sessions
                .get(&receipt.session_id)
                .map(|session| session.dto.clone())
                .ok_or_else(|| {
                    ApiError::internal("Network discovery idempotency state is inconsistent.")
                })?;
            return Ok((StatusCode::ACCEPTED, Json(session)));
        }
        discovery.sessions.insert(
            session_id.clone(),
            StoredDiscoverySession {
                dto: session.clone(),
                organization_id: request.organization_id.clone(),
                expires_at_ms,
                purge_at_ms,
                cancellation: cancellation.clone(),
                candidates: BTreeMap::new(),
            },
        );
        discovery.start_receipts.insert(
            idempotency_key,
            StoredStartReceipt {
                organization_id: request.organization_id,
                session_id: session_id.clone(),
            },
        );
    }

    tokio::spawn(run_discovery(state, session_id, cancellation, scan_permit));
    Ok((StatusCode::ACCEPTED, Json(session)))
}

async fn get_discovery(
    State(state): State<AppState>,
    Path(session_id): Path<String>,
) -> ApiResult<Json<NetworkDiscoverySnapshot>> {
    let mut discovery = state.network_discovery.write().await;
    discovery.expire_and_purge(now_ms());
    let session = discovery
        .sessions
        .get(&session_id)
        .ok_or_else(|| ApiError::not_found("NetworkDiscoverySession", &session_id))?;
    Ok(Json(NetworkDiscoverySnapshot {
        session: session.dto.clone(),
        candidates: session
            .candidates
            .values()
            .map(|candidate| candidate.dto.clone())
            .collect(),
    }))
}

async fn cancel_discovery(
    State(state): State<AppState>,
    Path(session_id): Path<String>,
    headers: HeaderMap,
) -> ApiResult<StatusCode> {
    require_mutation_headers(&headers)?;
    let mut discovery = state.network_discovery.write().await;
    discovery.expire_and_purge(now_ms());
    let session = discovery
        .sessions
        .get_mut(&session_id)
        .ok_or_else(|| ApiError::not_found("NetworkDiscoverySession", &session_id))?;
    session.cancellation.cancel();
    if session.dto.status != DiscoverySessionStatus::Expired {
        session.dto.status = DiscoverySessionStatus::Cancelled;
        session.dto.resource_version = session.dto.resource_version.saturating_add(1);
    }
    Ok(StatusCode::NO_CONTENT)
}

async fn verify_candidate(
    State(state): State<AppState>,
    Path((session_id, candidate_id)): Path<(String, String)>,
    headers: HeaderMap,
) -> ApiResult<Json<CandidateVerification>> {
    require_mutation_headers(&headers)?;
    let current_ms = now_ms();
    let (
        observation,
        cancellation,
        session_expires_at_ms,
        verification_attempt_id,
        previous_status,
    ) = {
        let mut discovery = state.network_discovery.write().await;
        discovery.expire_and_purge(current_ms);
        let session = active_session_mut(&mut discovery.sessions, &session_id, current_ms)?;
        let candidate = session
            .candidates
            .get_mut(&candidate_id)
            .ok_or_else(|| ApiError::not_found("DiscoveryCandidate", &candidate_id))?;
        if let Some(verification) = candidate
            .verification
            .as_ref()
            .filter(|verification| verification.expires_at_ms > current_ms)
        {
            return Ok(Json(verification.dto.clone()));
        }
        if candidate.verification_attempt.is_some() {
            return Err(ApiError::conflict(
                "This network candidate is already being verified.",
            ));
        }
        if candidate.dto.status == DiscoveryCandidateStatus::AlreadyLinked {
            return Err(ApiError::conflict(
                "This network candidate is already linked.",
            ));
        }
        if candidate.observation.expires_at_ms <= current_ms {
            candidate.dto.status = DiscoveryCandidateStatus::Unavailable;
            return Err(ApiError::gone("The network candidate has expired."));
        }
        let previous_status = candidate.dto.status;
        let verification_attempt_id = new_id("network-verification-attempt");
        candidate.verification_attempt = Some(verification_attempt_id.clone());
        candidate.dto.status = DiscoveryCandidateStatus::Verifying;
        (
            candidate.observation.clone(),
            session.cancellation.clone(),
            session.expires_at_ms,
            verification_attempt_id,
            previous_status,
        )
    };

    let mut verification_attempt = VerificationAttemptGuard::new(
        Arc::clone(&state.network_discovery),
        session_id.clone(),
        candidate_id.clone(),
        verification_attempt_id.clone(),
        previous_status,
    );
    let _verification_permit = match state
        .discovery_verification_slots
        .clone()
        .try_acquire_owned()
    {
        Ok(permit) => permit,
        Err(_busy) => {
            verification_attempt.rollback().await;
            return Err(ApiError::too_many_requests(
                "Too many network candidate verifications are running.",
            ));
        }
    };
    let provider_result = match tokio::time::timeout(
        state.discovery_provider_verification_timeout,
        state.discovery_provider.verify(&observation, cancellation),
    )
    .await
    {
        Ok(result) => result,
        Err(_elapsed) => {
            verification_attempt.rollback().await;
            return Err(ApiError::request_timeout(
                "Network candidate verification timed out.",
            ));
        }
    };
    let current_ms = now_ms();
    let mut discovery = state.network_discovery.write().await;
    discovery.expire_and_purge(current_ms);
    let session = match active_session_mut(&mut discovery.sessions, &session_id, current_ms) {
        Ok(session) => session,
        Err(err) => {
            drop(discovery);
            verification_attempt.rollback().await;
            return Err(err);
        }
    };
    let Some(candidate) = session.candidates.get_mut(&candidate_id) else {
        drop(discovery);
        verification_attempt.rollback().await;
        return Err(ApiError::not_found("DiscoveryCandidate", &candidate_id));
    };
    if candidate.verification_attempt.as_deref() != Some(&verification_attempt_id) {
        drop(discovery);
        verification_attempt.rollback().await;
        return Err(ApiError::conflict(
            "The network candidate verification attempt was superseded.",
        ));
    }
    if candidate.observation.fingerprint != observation.fingerprint
        || !same_source_pin(&candidate.observation, &observation)
    {
        candidate.verification_attempt = None;
        candidate.dto.status = DiscoveryCandidateStatus::Unavailable;
        verification_attempt.finish();
        return Err(ApiError::conflict(
            "The network candidate identity changed during verification.",
        ));
    }

    let (verification_status, fresh_observation) = match provider_result {
        Ok(result) => {
            let fresh = result.observation.filter(|fresh| {
                fresh.fingerprint == observation.fingerprint
                    && fresh.expires_at_ms > current_ms
                    && same_source_pin(fresh, &observation)
            });
            let status = if fresh.is_none() {
                CandidateVerificationStatus::Unavailable
            } else {
                result.status.into()
            };
            (status, fresh)
        }
        Err(err) => {
            eprintln!("Network candidate verification failed: {err}");
            (CandidateVerificationStatus::Unavailable, None)
        }
    };
    if let Some(fresh) = fresh_observation {
        candidate.dto.last_seen_at = iso_from_millis(fresh.last_seen_at_ms);
        candidate.observation = fresh;
    }
    candidate.dto.status = match verification_status {
        CandidateVerificationStatus::Verified => DiscoveryCandidateStatus::Verified,
        CandidateVerificationStatus::NeedsCredentials
        | CandidateVerificationStatus::Incompatible => DiscoveryCandidateStatus::NeedsAttention,
        CandidateVerificationStatus::Unavailable => DiscoveryCandidateStatus::Unavailable,
    };
    let verification_expires_at_ms = current_ms
        .saturating_add(duration_millis(state.discovery_verification_ttl))
        .min(session_expires_at_ms)
        .min(candidate.observation.expires_at_ms);
    let sources = candidate
        .observation
        .sources
        .iter()
        .map(|source| VerifiedDiscoverySource {
            id: source.id.clone(),
            label: source.label.clone(),
            category: source.category,
            status: if verification_status == CandidateVerificationStatus::Verified {
                source.status
            } else {
                DiscoverySourceStatus::Unavailable
            },
        })
        .collect();
    let verification = CandidateVerification {
        verification_token: new_id("network-verification"),
        candidate_id: candidate_id.clone(),
        status: verification_status,
        suggested_device: SuggestedDevice {
            name: candidate.dto.display_name.clone(),
            kind: candidate.observation.suggested_device_kind.clone(),
        },
        sources,
        expires_at: iso_from_millis(verification_expires_at_ms),
    };
    candidate.verification = Some(StoredVerification {
        dto: verification.clone(),
        fingerprint: candidate.observation.fingerprint.clone(),
        expires_at_ms: verification_expires_at_ms,
    });
    candidate.verification_attempt = None;
    verification_attempt.finish();
    Ok(Json(verification))
}

async fn approve_candidate(
    State(state): State<AppState>,
    Path((session_id, candidate_id)): Path<(String, String)>,
    headers: HeaderMap,
    Json(input): Json<ApproveNetworkCandidateInput>,
) -> ApiResult<Json<NetworkLinkReceipt>> {
    require_mutation_headers(&headers)?;
    validate_approval_input(&input)?;
    let input_fingerprint = approval_input_fingerprint(&input);
    let _approval_guard = state.discovery_approval_lock.lock().await;
    let current_ms = now_ms();

    let (organization_id, observation, selected_sources) = {
        let mut discovery = state.network_discovery.write().await;
        discovery.expire_and_purge(current_ms);
        let session = active_session_mut(&mut discovery.sessions, &session_id, current_ms)?;
        let candidate = session
            .candidates
            .get_mut(&candidate_id)
            .ok_or_else(|| ApiError::not_found("DiscoveryCandidate", &candidate_id))?;
        if let Some(approval) = &candidate.approval {
            if approval.input_fingerprint == input_fingerprint {
                return Ok(Json(approval.receipt.clone()));
            }
            return Err(ApiError::conflict(
                "The network candidate was already approved with different settings.",
            ));
        }
        let verification = candidate
            .verification
            .as_ref()
            .ok_or_else(|| ApiError::conflict("Verify the network candidate before approval."))?;
        if verification.expires_at_ms <= current_ms {
            candidate.dto.status = DiscoveryCandidateStatus::Unavailable;
            return Err(ApiError::gone("The candidate verification has expired."));
        }
        if verification.dto.verification_token != input.verification_token
            || verification.fingerprint != candidate.observation.fingerprint
        {
            return Err(ApiError::conflict(
                "The candidate verification token does not match this identity.",
            ));
        }
        if verification.dto.status != CandidateVerificationStatus::Verified {
            return Err(ApiError::conflict(
                "Only a verified network candidate can be approved.",
            ));
        }
        let selected_ids = input
            .selected_source_ids
            .iter()
            .cloned()
            .collect::<BTreeSet<_>>();
        let selected_sources = candidate
            .observation
            .sources
            .iter()
            .filter(|source| selected_ids.contains(&source.id))
            .cloned()
            .collect::<Vec<_>>();
        if selected_sources.len() != selected_ids.len()
            || selected_sources
                .iter()
                .any(|source| source.status != DiscoverySourceStatus::Ready)
        {
            return Err(ApiError::bad_request(
                "selectedSourceIds must contain verified, available sources only.",
            ));
        }
        (
            session.organization_id.clone(),
            candidate.observation.clone(),
            selected_sources,
        )
    };

    ensure_edge_organization_scope(&observation, &organization_id)?;

    let integration_id = stable_asset_id("integration-network", &observation.fingerprint);
    let device_id = stable_asset_id("device-network", &observation.fingerprint);
    let selected_source_ids = selected_sources
        .iter()
        .map(|source| {
            stable_asset_id(
                "data-source-network",
                &format!("{}\0{}", observation.fingerprint, source.id),
            )
        })
        .collect::<Vec<_>>();
    let project_id = input.project_id.clone();
    let device_name = input.device_name.clone();
    let observation_for_mutation = observation.clone();
    let source_pairs = selected_sources
        .into_iter()
        .zip(selected_source_ids.iter().cloned())
        .collect::<Vec<_>>();

    let receipt = state
        .durable_catalog_mutation(move |catalog| {
            if catalog.snapshot_version != input.expected_workspace_version {
                return Err(ApiError::conflict(
                    "The Project workspace changed. Refresh it before linking the candidate.",
                ));
            }
            let project = catalog
                .projects
                .get(&project_id)
                .ok_or_else(|| ApiError::not_found("Project", &project_id))?;
            if project.organization_id != organization_id {
                return Err(ApiError::conflict(
                    "The Project and discovery session must belong to the same organization.",
                ));
            }

            let next_version = catalog.snapshot_version.saturating_add(1);
            if let Some(integration) = catalog.integrations.get_mut(&integration_id) {
                if integration.organization_id != organization_id {
                    return Err(ApiError::conflict(
                        "The discovered Integration identity belongs to another organization.",
                    ));
                }
                if observation_for_mutation
                    .edge_identity
                    .as_ref()
                    .is_some_and(|identity| identity.organization_trusted)
                {
                    integration.status = "connected".to_owned();
                    integration.resource_version = next_version;
                }
            } else {
                let observed_at = iso_from_millis(observation_for_mutation.last_seen_at_ms);
                catalog.integrations.insert(
                    integration_id.clone(),
                    Integration {
                        id: integration_id.clone(),
                        organization_id: organization_id.clone(),
                        name: format!("{} 연결", observation_for_mutation.display_name),
                        kind: observation_for_mutation.integration_kind.clone(),
                        status: if observation_for_mutation
                            .edge_identity
                            .as_ref()
                            .is_some_and(|identity| identity.organization_trusted)
                        {
                            "connected"
                        } else {
                            "testing"
                        }
                        .to_owned(),
                        endpoint_label: observation_for_mutation.endpoint_label.clone(),
                        last_health_at: observed_at,
                        created_at: now_iso(),
                        resource_version: next_version,
                    },
                );
            }

            if let Some(device) = catalog.devices.get(&device_id) {
                if device.organization_id != organization_id
                    || device.integration_id != integration_id
                {
                    return Err(ApiError::conflict(
                        "The discovered Device identity conflicts with the catalog.",
                    ));
                }
            } else {
                catalog.devices.insert(
                    device_id.clone(),
                    Device {
                        id: device_id.clone(),
                        organization_id: organization_id.clone(),
                        integration_id: integration_id.clone(),
                        name: device_name,
                        kind: observation_for_mutation.suggested_device_kind.clone(),
                        status: "offline".to_owned(),
                        health: "unknown".to_owned(),
                        operation_mode: String::new(),
                        battery_percent: None,
                        task_name: String::new(),
                        task_progress: 0,
                        last_seen_at: iso_from_millis(observation_for_mutation.last_seen_at_ms),
                        state_version: 1,
                    },
                );
            }

            for (source, source_id) in &source_pairs {
                if let Some(existing) = catalog.data_sources.get(source_id) {
                    if existing.device_id != device_id || existing.integration_id != integration_id
                    {
                        return Err(ApiError::conflict(
                            "The discovered DataSource identity conflicts with the catalog.",
                        ));
                    }
                } else {
                    catalog.data_sources.insert(
                        source_id.clone(),
                        DataSource {
                            id: source_id.clone(),
                            integration_id: integration_id.clone(),
                            device_id: device_id.clone(),
                            name: source.label.clone(),
                            protocol: source.protocol.clone(),
                            status: "pending".to_owned(),
                            live_url: endpoint_url(&source.endpoint),
                            topic_ids: Vec::new(),
                            mapping_version: 1,
                            last_data_at: iso_from_millis(observation_for_mutation.last_seen_at_ms),
                        },
                    );
                    catalog
                        .topics_by_data_source
                        .insert(source_id.clone(), Vec::new());
                }
            }

            if let Some(identity) = &observation_for_mutation.edge_identity {
                let enrollment = EdgeEnrollment {
                    edge_device_id: identity.device_id.clone(),
                    public_key: BASE64_URL_SAFE_NO_PAD.encode(identity.public_key),
                    organization_id: organization_id.clone(),
                    integration_id: integration_id.clone(),
                    device_id: device_id.clone(),
                    source_ids: source_pairs
                        .iter()
                        .map(|(source, source_id)| (source.id.clone(), source_id.clone()))
                        .collect(),
                    organization_trusted: identity.organization_trusted,
                    created_at: now_iso(),
                };
                if let Some(existing) = catalog.edge_enrollments.get_mut(&identity.device_id) {
                    if existing.public_key != enrollment.public_key
                        || existing.organization_id != enrollment.organization_id
                        || existing.integration_id != enrollment.integration_id
                        || existing.device_id != enrollment.device_id
                    {
                        return Err(ApiError::conflict(
                            "The signed Edge identity conflicts with an existing enrollment.",
                        ));
                    }
                    for (source_id, catalog_source_id) in enrollment.source_ids {
                        if existing
                            .source_ids
                            .insert(source_id, catalog_source_id.clone())
                            .is_some_and(|previous| previous != catalog_source_id)
                        {
                            return Err(ApiError::conflict(
                                "The signed Edge source conflicts with an existing enrollment.",
                            ));
                        }
                    }
                    existing.organization_trusted |= enrollment.organization_trusted;
                } else {
                    catalog
                        .edge_enrollments
                        .insert(identity.device_id.clone(), enrollment);
                }
            }

            if !catalog.device_assignments.values().any(|assignment| {
                assignment.project_id == project_id
                    && assignment.device_id == device_id
                    && assignment.valid_to.is_none()
            }) {
                let assignment = DeviceAssignment {
                    id: new_id("device-assignment"),
                    project_id: project_id.clone(),
                    device_id: device_id.clone(),
                    access_mode: AccessMode::Observe,
                    valid_from: now_iso(),
                    valid_to: None,
                    resource_version: next_version,
                };
                catalog
                    .device_assignments
                    .insert(assignment.id.clone(), assignment);
            }
            for source_id in &selected_source_ids {
                if catalog.data_assignments.values().any(|assignment| {
                    assignment.project_id == project_id
                        && assignment.data_source_id == *source_id
                        && assignment.valid_to.is_none()
                }) {
                    continue;
                }
                let assignment = DataAssignment {
                    id: new_id("data-assignment"),
                    project_id: project_id.clone(),
                    data_source_id: source_id.clone(),
                    visibility: DataVisibility::Operator,
                    valid_from: now_iso(),
                    valid_to: None,
                    resource_version: next_version,
                };
                catalog
                    .data_assignments
                    .insert(assignment.id.clone(), assignment);
            }
            catalog.refresh_project_counts(&project_id);
            let workspace_version = catalog.bump_version();
            Ok(NetworkLinkReceipt {
                status: "linked".to_owned(),
                project_id,
                integration_id,
                device_id,
                data_source_ids: selected_source_ids,
                workspace_version,
            })
        })
        .await?;

    let mut discovery = state.network_discovery.write().await;
    discovery.expire_and_purge(now_ms());
    if let Some(candidate) = discovery
        .sessions
        .get_mut(&session_id)
        .and_then(|session| session.candidates.get_mut(&candidate_id))
    {
        candidate.dto.status = DiscoveryCandidateStatus::AlreadyLinked;
        candidate.approval = Some(StoredApproval {
            input_fingerprint,
            receipt: receipt.clone(),
        });
    }
    Ok(Json(receipt))
}

async fn run_discovery(
    state: AppState,
    session_id: String,
    cancellation: DiscoveryCancellation,
    _scan_permit: OwnedSemaphorePermit,
) {
    let result = state
        .discovery_provider
        .discover(cancellation.clone())
        .await;
    let now_ms = now_ms();
    let linked_device_ids = {
        let catalog = state.catalog.read().await;
        catalog.devices.keys().cloned().collect::<BTreeSet<_>>()
    };
    let mut discovery = state.network_discovery.write().await;
    discovery.expire_and_purge(now_ms);
    let Some(session) = discovery.sessions.get_mut(&session_id) else {
        return;
    };
    if cancellation.is_cancelled()
        || matches!(
            session.dto.status,
            DiscoverySessionStatus::Cancelled | DiscoverySessionStatus::Expired
        )
    {
        if session.dto.status != DiscoverySessionStatus::Expired {
            session.dto.status = DiscoverySessionStatus::Cancelled;
            session.dto.resource_version = session.dto.resource_version.saturating_add(1);
        }
        return;
    }

    match result {
        Ok(observations) => {
            let mut deduplicated = BTreeMap::new();
            for observation in observations {
                if deduplicated.len() >= MAX_CANDIDATES_PER_SESSION {
                    break;
                }
                if sanitize_observation(&observation, now_ms).is_err() {
                    continue;
                }
                deduplicated
                    .entry(observation.fingerprint.clone())
                    .or_insert(observation);
            }
            session.candidates = deduplicated
                .into_values()
                .map(|observation| {
                    let candidate_id = stable_asset_id(
                        "network-candidate",
                        &format!("{session_id}\0{}", observation.fingerprint),
                    );
                    let status = if linked_device_ids
                        .contains(&stable_asset_id("device-network", &observation.fingerprint))
                    {
                        DiscoveryCandidateStatus::AlreadyLinked
                    } else {
                        DiscoveryCandidateStatus::Found
                    };
                    let dto = DiscoveryCandidate {
                        id: candidate_id.clone(),
                        session_id: session_id.clone(),
                        display_name: observation.display_name.clone(),
                        category: observation.category,
                        status,
                        last_seen_at: iso_from_millis(observation.last_seen_at_ms),
                        source_count: observation.sources.len(),
                        supports_live: observation
                            .sources
                            .iter()
                            .any(|source| source.status == DiscoverySourceStatus::Ready),
                    };
                    (
                        candidate_id,
                        StoredCandidate {
                            dto,
                            observation,
                            verification: None,
                            verification_attempt: None,
                            approval: None,
                        },
                    )
                })
                .collect();
            session.dto.candidate_count = session.candidates.len();
            session.dto.status = DiscoverySessionStatus::Ready;
            session.dto.resource_version = session.dto.resource_version.saturating_add(1);
        }
        Err(err) => {
            eprintln!("Network discovery failed: {err}");
            session.dto.status = DiscoverySessionStatus::Failed;
            session.dto.resource_version = session.dto.resource_version.saturating_add(1);
        }
    }
}

fn active_session_mut<'a>(
    sessions: &'a mut BTreeMap<String, StoredDiscoverySession>,
    session_id: &str,
    now_ms: i64,
) -> ApiResult<&'a mut StoredDiscoverySession> {
    let session = sessions
        .get_mut(session_id)
        .ok_or_else(|| ApiError::not_found("NetworkDiscoverySession", session_id))?;
    if session.expires_at_ms <= now_ms || session.dto.status == DiscoverySessionStatus::Expired {
        return Err(ApiError::gone("The network discovery session has expired."));
    }
    if session.dto.status == DiscoverySessionStatus::Cancelled {
        return Err(ApiError::conflict(
            "The network discovery session was cancelled.",
        ));
    }
    if session.dto.status == DiscoverySessionStatus::Failed {
        return Err(ApiError::conflict("The network discovery session failed."));
    }
    if session.dto.status == DiscoverySessionStatus::Searching {
        return Err(ApiError::conflict(
            "The network discovery session is still searching.",
        ));
    }
    Ok(session)
}

fn validate_approval_input(input: &ApproveNetworkCandidateInput) -> ApiResult<()> {
    validate_text("verificationToken", &input.verification_token, 128)?;
    validate_text("projectId", &input.project_id, 128)?;
    validate_text("deviceName", &input.device_name, MAX_DISPLAY_NAME_LEN)?;
    if input.access_mode != AccessMode::Observe {
        return Err(ApiError::bad_request(
            "Network candidates can only be linked with observe access.",
        ));
    }
    if input.visibility != DataVisibility::Operator {
        return Err(ApiError::bad_request(
            "Network candidate data must initially use operator visibility.",
        ));
    }
    if input.selected_source_ids.is_empty() {
        return Err(ApiError::bad_request(
            "Select at least one verified, available data source.",
        ));
    }
    if input.selected_source_ids.len() > MAX_SOURCES_PER_CANDIDATE {
        return Err(ApiError::bad_request(
            "Too many data sources were selected.",
        ));
    }
    let unique = input.selected_source_ids.iter().collect::<BTreeSet<_>>();
    if unique.len() != input.selected_source_ids.len()
        || input
            .selected_source_ids
            .iter()
            .any(|id| validate_text("selectedSourceIds", id, MAX_SOURCE_ID_LEN).is_err())
    {
        return Err(ApiError::bad_request(
            "selectedSourceIds must contain unique, valid source IDs.",
        ));
    }
    Ok(())
}

fn sanitize_observation(observation: &DiscoveryObservation, now_ms: i64) -> ApiResult<()> {
    validate_text(
        "candidate fingerprint",
        &observation.fingerprint,
        MAX_FINGERPRINT_LEN,
    )?;
    validate_text(
        "candidate display name",
        &observation.display_name,
        MAX_DISPLAY_NAME_LEN,
    )?;
    validate_text(
        "candidate endpoint label",
        &observation.endpoint_label,
        MAX_DISPLAY_NAME_LEN,
    )?;
    if !matches!(
        observation.suggested_device_kind.as_str(),
        "robot" | "drone" | "vehicle" | "camera" | "gateway"
    ) || !matches!(
        observation.integration_kind.as_str(),
        "ros2" | "rtsp" | "mavlink" | "autoware" | "rerun" | "rms_edge"
    ) || observation.expires_at_ms <= now_ms
        || observation.last_seen_at_ms > now_ms.saturating_add(5_000)
        || observation.sources.len() > MAX_SOURCES_PER_CANDIDATE
        || (observation.integration_kind == "rms_edge" && observation.edge_identity.is_none())
    {
        return Err(ApiError::bad_request(
            "The discovery observation is not eligible for registration.",
        ));
    }
    let mut source_ids = BTreeSet::new();
    for source in &observation.sources {
        validate_text("source ID", &source.id, MAX_SOURCE_ID_LEN)?;
        validate_text("source label", &source.label, MAX_SOURCE_LABEL_LEN)?;
        if !source_ids.insert(source.id.as_str())
            || !matches!(
                source.protocol.as_str(),
                "rerun" | "redap" | "rtsp" | "ros2" | "mavlink" | "onvif" | "ssdp"
            )
            || !matches!(
                source.endpoint.scheme.as_str(),
                "http" | "https" | "rtsp" | "rerun+http" | "rerun+https" | "redap"
            )
            || source.endpoint.port == 0
            || !address_is_lan_eligible(source.endpoint.source_address)
            || source.endpoint.path.len() > MAX_ENDPOINT_PATH_LEN
            || !source.endpoint.path.starts_with('/')
            || source.endpoint.path.contains("..")
            || source.endpoint.path.chars().any(char::is_control)
        {
            return Err(ApiError::bad_request(
                "The discovery source endpoint is not eligible for registration.",
            ));
        }
    }
    Ok(())
}

fn address_is_lan_eligible(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(address) => ipv4_is_lan_eligible(address),
        IpAddr::V6(address) => ipv6_is_lan_eligible(address),
    }
}

fn ipv4_is_lan_eligible(address: Ipv4Addr) -> bool {
    (address.is_private() || address.is_link_local())
        && !address.is_loopback()
        && !address.is_unspecified()
        && !address.is_multicast()
        && !address.is_broadcast()
}

fn ipv6_is_lan_eligible(address: Ipv6Addr) -> bool {
    (address.is_unique_local() || address.is_unicast_link_local())
        && !address.is_loopback()
        && !address.is_unspecified()
        && !address.is_multicast()
}

fn endpoint_url(endpoint: &crate::network_discovery::PinnedDiscoveryEndpoint) -> String {
    let host = match endpoint.source_address {
        IpAddr::V4(address) => address.to_string(),
        IpAddr::V6(address) => format!("[{address}]"),
    };
    format!(
        "{}://{}:{}{}",
        endpoint.scheme, host, endpoint.port, endpoint.path
    )
}

fn require_mutation_headers(headers: &HeaderMap) -> ApiResult<String> {
    let idempotency_key = required_printable_header(headers, "idempotency-key", "Idempotency-Key")?;
    required_printable_header(headers, "x-rms-request-id", "X-RMS-Request-ID")?;
    Ok(idempotency_key)
}

fn required_printable_header(
    headers: &HeaderMap,
    header_name: &'static str,
    display_name: &str,
) -> ApiResult<String> {
    let value = headers
        .get(header_name)
        .ok_or_else(|| ApiError::bad_request(format!("{display_name} is required.")))?
        .to_str()
        .map_err(|_invalid| ApiError::bad_request(format!("{display_name} must be valid ASCII.")))?
        .trim();
    if value.is_empty() || value.len() > 128 || value.chars().any(char::is_control) {
        return Err(ApiError::bad_request(format!(
            "{display_name} must contain 1 to 128 printable characters."
        )));
    }
    Ok(value.to_owned())
}

fn validate_text(field: &str, value: &str, max_len: usize) -> ApiResult<()> {
    let value = value.trim();
    if value.is_empty()
        || value.len() > max_len
        || value.chars().any(|character| character.is_control())
    {
        Err(ApiError::bad_request(format!(
            "{field} must contain 1 to {max_len} safe characters."
        )))
    } else {
        Ok(())
    }
}

fn duration_millis(duration: Duration) -> i64 {
    i64::try_from(duration.as_millis()).unwrap_or(i64::MAX)
}

fn stable_asset_id(prefix: &str, identity: &str) -> String {
    let digest = format!("{:x}", Sha256::digest(identity.as_bytes()));
    format!("{prefix}-{}", &digest[..20])
}

fn approval_input_fingerprint(input: &ApproveNetworkCandidateInput) -> String {
    let encoded = serde_json::to_vec(input).expect("approval input is always JSON serializable");
    format!("{:x}", Sha256::digest(encoded))
}

fn ensure_edge_organization_scope(
    observation: &DiscoveryObservation,
    organization_id: &str,
) -> ApiResult<()> {
    if observation.edge_identity.as_ref().is_some_and(|identity| {
        identity.organization_trusted
            && identity.organization_id.as_deref() != Some(organization_id)
    }) {
        Err(ApiError::conflict(
            "The signed Edge identity belongs to another organization.",
        ))
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{DiscoveryCandidateCategory, DiscoverySourceCategory};
    use crate::network_discovery::{
        DiscoveredSource, EdgeObservationIdentity, FakeDiscoveryProvider, PinnedDiscoveryEndpoint,
    };

    fn observation(address: &str) -> DiscoveryObservation {
        let now_ms = now_ms();
        DiscoveryObservation {
            fingerprint: "test-candidate".to_owned(),
            display_name: "Test robot".to_owned(),
            category: DiscoveryCandidateCategory::Robot,
            suggested_device_kind: "robot".to_owned(),
            integration_kind: "rerun".to_owned(),
            endpoint_label: "Test edge".to_owned(),
            last_seen_at_ms: now_ms,
            expires_at_ms: now_ms + 1_000,
            sources: vec![DiscoveredSource {
                id: "telemetry".to_owned(),
                label: "Telemetry".to_owned(),
                category: DiscoverySourceCategory::Telemetry,
                status: DiscoverySourceStatus::Ready,
                protocol: "rerun".to_owned(),
                endpoint: PinnedDiscoveryEndpoint {
                    source_address: address.parse().unwrap(),
                    port: 9877,
                    scheme: "rerun+http".to_owned(),
                    path: "/proxy".to_owned(),
                },
            }],
            edge_identity: None,
        }
    }

    #[test]
    fn observation_boundary_rejects_non_lan_and_unsafe_paths() {
        assert!(sanitize_observation(&observation("192.168.1.10"), now_ms()).is_ok());
        assert!(sanitize_observation(&observation("8.8.8.8"), now_ms()).is_err());
        assert!(sanitize_observation(&observation("127.0.0.1"), now_ms()).is_err());

        let mut unsafe_path = observation("10.0.0.2");
        unsafe_path.sources[0].endpoint.path = "/../admin".to_owned();
        assert!(sanitize_observation(&unsafe_path, now_ms()).is_err());
    }

    #[test]
    fn signed_edge_observation_is_eligible_for_registration() {
        let mut edge = observation("10.0.0.2");
        edge.integration_kind = "rms_edge".to_owned();
        edge.sources[0].id = "mavlink-telemetry".to_owned();
        edge.sources[0].protocol = "mavlink".to_owned();
        edge.sources[0].endpoint.scheme = "http".to_owned();
        edge.edge_identity = Some(EdgeObservationIdentity {
            device_id: "edge-device".to_owned(),
            public_key: [1; 32],
            nonce: [2; 16],
            timestamp_seconds: 1,
            organization_id: None,
            organization_trusted: false,
        });

        assert!(sanitize_observation(&edge, now_ms()).is_ok());

        edge.edge_identity = None;
        assert!(sanitize_observation(&edge, now_ms()).is_err());
    }

    #[test]
    fn organization_authenticated_edge_cannot_cross_project_tenants() {
        let mut observation = observation("10.0.0.2");
        observation.edge_identity = Some(EdgeObservationIdentity {
            device_id: "edge-device".to_owned(),
            public_key: [1; 32],
            nonce: [2; 16],
            timestamp_seconds: 1,
            organization_id: Some("org-a".to_owned()),
            organization_trusted: true,
        });
        assert!(ensure_edge_organization_scope(&observation, "org-a").is_ok());
        assert!(ensure_edge_organization_scope(&observation, "org-b").is_err());
    }

    #[tokio::test]
    async fn signed_edge_candidate_completes_scan_verify_and_approval() {
        let mut edge = observation("10.0.0.2");
        edge.integration_kind = "rms_edge".to_owned();
        edge.sources[0].id = "mavlink-telemetry".to_owned();
        edge.sources[0].protocol = "mavlink".to_owned();
        edge.sources[0].endpoint.scheme = "http".to_owned();
        edge.edge_identity = Some(EdgeObservationIdentity {
            device_id: "edge-device".to_owned(),
            public_key: [1; 32],
            nonce: [2; 16],
            timestamp_seconds: 1,
            organization_id: None,
            organization_trusted: false,
        });
        let state = AppState::fixture()
            .with_discovery_provider(Arc::new(FakeDiscoveryProvider::new(vec![edge])));
        let session = start_discovery(
            State(state.clone()),
            mutation_headers("start-edge-e2e"),
            Json(StartNetworkDiscoveryRequest {
                organization_id: "org-rms".to_owned(),
            }),
        )
        .await
        .expect("Edge discovery starts")
        .1
        .0;
        let snapshot = loop {
            let snapshot = get_discovery(State(state.clone()), Path(session.id.clone()))
                .await
                .expect("Edge snapshot is available")
                .0;
            if snapshot.session.status != DiscoverySessionStatus::Searching {
                break snapshot;
            }
            tokio::task::yield_now().await;
        };
        let candidate = snapshot
            .candidates
            .first()
            .expect("Edge candidate is retained");
        let verification = verify_candidate(
            State(state.clone()),
            Path((session.id.clone(), candidate.id.clone())),
            mutation_headers("verify-edge-e2e"),
        )
        .await
        .expect("Edge candidate verifies")
        .0;
        let workspace_version = state.catalog.read().await.snapshot_version;
        let receipt = approve_candidate(
            State(state.clone()),
            Path((session.id, candidate.id.clone())),
            mutation_headers("approve-edge-e2e"),
            Json(ApproveNetworkCandidateInput {
                verification_token: verification.verification_token,
                project_id: "project-logistics".to_owned(),
                expected_workspace_version: workspace_version,
                device_name: "Edge vehicle".to_owned(),
                selected_source_ids: vec!["mavlink-telemetry".to_owned()],
                access_mode: AccessMode::Observe,
                visibility: DataVisibility::Operator,
            }),
        )
        .await
        .expect("Edge candidate approval succeeds")
        .0;

        let catalog = state.catalog.read().await;
        assert_eq!(
            catalog
                .integrations
                .get(&receipt.integration_id)
                .expect("approved Edge integration exists")
                .kind,
            "rms_edge"
        );
        assert!(catalog.edge_enrollments.contains_key("edge-device"));
        assert!(catalog.live_sessions.is_empty());
        assert!(catalog.leases.is_empty());
    }

    #[tokio::test]
    async fn approval_never_creates_live_or_control_state() {
        let state = AppState::fixture();
        let session = start_discovery(
            State(state.clone()),
            mutation_headers("start-no-control"),
            Json(StartNetworkDiscoveryRequest {
                organization_id: "org-rms".to_owned(),
            }),
        )
        .await
        .expect("discovery starts")
        .1
        .0;
        let snapshot = loop {
            let snapshot = get_discovery(State(state.clone()), Path(session.id.clone()))
                .await
                .expect("snapshot is available")
                .0;
            if snapshot.session.status != DiscoverySessionStatus::Searching {
                break snapshot;
            }
            tokio::task::yield_now().await;
        };
        let candidate = snapshot.candidates.first().expect("fixture candidate");
        let verification = verify_candidate(
            State(state.clone()),
            Path((session.id.clone(), candidate.id.clone())),
            mutation_headers("verify-no-control"),
        )
        .await
        .expect("candidate verifies")
        .0;
        let workspace_version = state.catalog.read().await.snapshot_version;
        let receipt = approve_candidate(
            State(state.clone()),
            Path((session.id, candidate.id.clone())),
            mutation_headers("approve-no-control"),
            Json(ApproveNetworkCandidateInput {
                verification_token: verification.verification_token,
                project_id: "project-logistics".to_owned(),
                expected_workspace_version: workspace_version,
                device_name: "Robot-42".to_owned(),
                selected_source_ids: vec!["telemetry".to_owned()],
                access_mode: AccessMode::Observe,
                visibility: DataVisibility::Operator,
            }),
        )
        .await
        .expect("candidate approval succeeds")
        .0;

        let catalog = state.catalog.read().await;
        assert!(catalog.live_sessions.is_empty());
        assert!(catalog.live_session_shutdown.is_empty());
        assert!(catalog.replay_sessions.is_empty());
        assert!(catalog.leases.is_empty());
        assert!(catalog.lease_epochs.is_empty());
        assert!(catalog.command_receipts.is_empty());
        assert!(catalog.device_assignments.values().any(|assignment| {
            assignment.project_id == receipt.project_id
                && assignment.device_id == receipt.device_id
                && assignment.access_mode == AccessMode::Observe
        }));
    }

    fn mutation_headers(id: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert("idempotency-key", id.parse().unwrap());
        headers.insert("x-rms-request-id", format!("request-{id}").parse().unwrap());
        headers
    }
}
