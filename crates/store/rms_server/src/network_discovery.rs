//! User-initiated, short-lived discovery observations.
//!
//! Network endpoints and protocol metadata deliberately stay behind this module boundary.
//! The public API only exposes the sanitized DTOs from [`crate::domain`].

use std::{
    collections::BTreeMap,
    error::Error,
    fmt,
    net::IpAddr,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use async_trait::async_trait;
use tokio::sync::RwLock;

use crate::domain::{
    CandidateVerification, CandidateVerificationStatus, DiscoveryCandidate,
    DiscoveryCandidateCategory, DiscoveryCandidateStatus, DiscoverySourceCategory,
    DiscoverySourceStatus, NetworkDiscoverySession, NetworkLinkReceipt,
};

pub(crate) mod edge_advertisement;
mod multicast;

pub(crate) use multicast::MulticastDiscoveryProvider;

pub(crate) const DEFAULT_SESSION_TTL: Duration = Duration::from_mins(5);
pub(crate) const DEFAULT_VERIFICATION_TTL: Duration = Duration::from_mins(1);
pub(crate) const DEFAULT_PROVIDER_VERIFICATION_TIMEOUT: Duration = Duration::from_secs(5);
pub(crate) const EXPIRED_SESSION_RETENTION: Duration = Duration::from_mins(5);
pub(crate) const MAX_CANDIDATES_PER_SESSION: usize = 128;
pub(crate) const MAX_SOURCES_PER_CANDIDATE: usize = 16;

#[derive(Clone, Debug)]
pub struct DiscoveryCancellation {
    cancelled: Arc<AtomicBool>,
}

impl DiscoveryCancellation {
    pub fn new() -> Self {
        Self {
            cancelled: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }
}

impl Default for DiscoveryCancellation {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PinnedDiscoveryEndpoint {
    pub source_address: IpAddr,
    pub port: u16,
    pub scheme: String,
    pub path: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiscoveredSource {
    pub id: String,
    pub label: String,
    pub category: DiscoverySourceCategory,
    pub status: DiscoverySourceStatus,
    pub protocol: String,
    pub endpoint: PinnedDiscoveryEndpoint,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiscoveryObservation {
    pub fingerprint: String,
    pub display_name: String,
    pub category: DiscoveryCandidateCategory,
    /// One of `robot`, `drone`, `vehicle`, `camera`, or `gateway`.
    pub suggested_device_kind: String,
    /// A server-known Integration kind such as `rerun` or `rtsp`.
    pub integration_kind: String,
    pub endpoint_label: String,
    pub last_seen_at_ms: i64,
    pub expires_at_ms: i64,
    pub sources: Vec<DiscoveredSource>,
    /// Cryptographically verified RMS Edge Agent identity metadata.
    ///
    /// This never crosses the public discovery API boundary.
    pub edge_identity: Option<EdgeObservationIdentity>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EdgeObservationIdentity {
    pub device_id: String,
    pub public_key: [u8; 32],
    pub nonce: [u8; 16],
    pub timestamp_seconds: i64,
    pub organization_id: Option<String>,
    pub organization_trusted: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProviderVerificationStatus {
    Verified,
    NeedsCredentials,
    Incompatible,
    Unavailable,
}

impl From<ProviderVerificationStatus> for CandidateVerificationStatus {
    fn from(value: ProviderVerificationStatus) -> Self {
        match value {
            ProviderVerificationStatus::Verified => Self::Verified,
            ProviderVerificationStatus::NeedsCredentials => Self::NeedsCredentials,
            ProviderVerificationStatus::Incompatible => Self::Incompatible,
            ProviderVerificationStatus::Unavailable => Self::Unavailable,
        }
    }
}

#[derive(Clone, Debug)]
pub struct ProviderVerification {
    pub status: ProviderVerificationStatus,
    /// A fresh observation of the same pinned identity, when it is still present.
    pub observation: Option<DiscoveryObservation>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiscoveryProviderError {
    message: String,
}

impl DiscoveryProviderError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for DiscoveryProviderError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl Error for DiscoveryProviderError {}

#[async_trait]
pub trait DiscoveryProvider: Send + Sync + 'static {
    /// Performs one bounded multicast discovery pass.
    async fn discover(
        &self,
        cancellation: DiscoveryCancellation,
    ) -> Result<Vec<DiscoveryObservation>, DiscoveryProviderError>;

    /// Re-observes a candidate and confirms its fingerprint and packet source pin.
    async fn verify(
        &self,
        candidate: &DiscoveryObservation,
        cancellation: DiscoveryCancellation,
    ) -> Result<ProviderVerification, DiscoveryProviderError>;
}

#[derive(Clone)]
pub struct FakeDiscoveryProvider {
    observations: Arc<Vec<DiscoveryObservation>>,
    scan_delay: Duration,
    verification_status: ProviderVerificationStatus,
}

impl FakeDiscoveryProvider {
    pub fn new(observations: Vec<DiscoveryObservation>) -> Self {
        Self {
            observations: Arc::new(observations),
            scan_delay: Duration::ZERO,
            verification_status: ProviderVerificationStatus::Verified,
        }
    }

    pub fn with_scan_delay(mut self, scan_delay: Duration) -> Self {
        self.scan_delay = scan_delay;
        self
    }

    pub fn with_verification_status(
        mut self,
        verification_status: ProviderVerificationStatus,
    ) -> Self {
        self.verification_status = verification_status;
        self
    }

    pub fn fixture() -> Self {
        let now_ms = crate::state::now_ms();
        Self::new(vec![DiscoveryObservation {
            fingerprint: "fixture:rms-edge:robot-42".to_owned(),
            display_name: "Robot-42".to_owned(),
            category: DiscoveryCandidateCategory::Robot,
            suggested_device_kind: "robot".to_owned(),
            integration_kind: "rerun".to_owned(),
            endpoint_label: "RMS Edge Agent".to_owned(),
            last_seen_at_ms: now_ms,
            expires_at_ms: now_ms.saturating_add(120_000),
            sources: vec![
                DiscoveredSource {
                    id: "telemetry".to_owned(),
                    label: "상태 데이터".to_owned(),
                    category: DiscoverySourceCategory::Telemetry,
                    status: DiscoverySourceStatus::Ready,
                    protocol: "rerun".to_owned(),
                    endpoint: PinnedDiscoveryEndpoint {
                        source_address: "192.168.10.42".parse().expect("fixture address is valid"),
                        port: 9877,
                        scheme: "rerun+http".to_owned(),
                        path: "/proxy".to_owned(),
                    },
                },
                DiscoveredSource {
                    id: "camera".to_owned(),
                    label: "전방 카메라".to_owned(),
                    category: DiscoverySourceCategory::Camera,
                    status: DiscoverySourceStatus::Ready,
                    protocol: "rtsp".to_owned(),
                    endpoint: PinnedDiscoveryEndpoint {
                        source_address: "192.168.10.42".parse().expect("fixture address is valid"),
                        port: 8554,
                        scheme: "rtsp".to_owned(),
                        path: "/front".to_owned(),
                    },
                },
            ],
            edge_identity: None,
        }])
    }
}

impl Default for FakeDiscoveryProvider {
    fn default() -> Self {
        Self::fixture()
    }
}

#[async_trait]
impl DiscoveryProvider for FakeDiscoveryProvider {
    async fn discover(
        &self,
        cancellation: DiscoveryCancellation,
    ) -> Result<Vec<DiscoveryObservation>, DiscoveryProviderError> {
        if !self.scan_delay.is_zero() {
            tokio::select! {
                () = tokio::time::sleep(self.scan_delay) => {}
                () = wait_for_cancellation(cancellation.clone()) => return Ok(Vec::new()),
            }
        }
        if cancellation.is_cancelled() {
            Ok(Vec::new())
        } else {
            Ok(self.observations.as_ref().clone())
        }
    }

    async fn verify(
        &self,
        candidate: &DiscoveryObservation,
        cancellation: DiscoveryCancellation,
    ) -> Result<ProviderVerification, DiscoveryProviderError> {
        if cancellation.is_cancelled() {
            return Ok(ProviderVerification {
                status: ProviderVerificationStatus::Unavailable,
                observation: None,
            });
        }
        let observation = self
            .observations
            .iter()
            .find(|observation| {
                observation.fingerprint == candidate.fingerprint
                    && same_source_pin(observation, candidate)
            })
            .cloned();
        Ok(ProviderVerification {
            status: if observation.is_some() {
                self.verification_status
            } else {
                ProviderVerificationStatus::Unavailable
            },
            observation,
        })
    }
}

async fn wait_for_cancellation(cancellation: DiscoveryCancellation) {
    while !cancellation.is_cancelled() {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

pub(crate) fn same_source_pin(left: &DiscoveryObservation, right: &DiscoveryObservation) -> bool {
    if left.sources.len() != right.sources.len() {
        return false;
    }
    left.sources.iter().all(|left_source| {
        right.sources.iter().any(|right_source| {
            left_source.id == right_source.id && left_source.endpoint == right_source.endpoint
        })
    })
}

pub(crate) struct StoredCandidate {
    pub dto: DiscoveryCandidate,
    pub observation: DiscoveryObservation,
    pub verification: Option<StoredVerification>,
    pub verification_attempt: Option<String>,
    pub approval: Option<StoredApproval>,
}

/// Restores candidate state if a verification request is cancelled or its task is aborted.
pub(crate) struct VerificationAttemptGuard {
    store: Arc<RwLock<NetworkDiscoveryStore>>,
    session_id: String,
    candidate_id: String,
    attempt_id: String,
    previous_status: DiscoveryCandidateStatus,
    armed: bool,
}

impl VerificationAttemptGuard {
    pub(crate) fn new(
        store: Arc<RwLock<NetworkDiscoveryStore>>,
        session_id: String,
        candidate_id: String,
        attempt_id: String,
        previous_status: DiscoveryCandidateStatus,
    ) -> Self {
        Self {
            store,
            session_id,
            candidate_id,
            attempt_id,
            previous_status,
            armed: true,
        }
    }

    pub(crate) async fn rollback(&mut self) {
        if !self.armed {
            return;
        }
        rollback_verification_attempt(
            &self.store,
            &self.session_id,
            &self.candidate_id,
            &self.attempt_id,
            self.previous_status,
        )
        .await;
        self.armed = false;
    }

    pub(crate) fn finish(&mut self) {
        self.armed = false;
    }
}

impl Drop for VerificationAttemptGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            return;
        };
        let store = Arc::clone(&self.store);
        let session_id = self.session_id.clone();
        let candidate_id = self.candidate_id.clone();
        let attempt_id = self.attempt_id.clone();
        let previous_status = self.previous_status;
        runtime.spawn(async move {
            rollback_verification_attempt(
                &store,
                &session_id,
                &candidate_id,
                &attempt_id,
                previous_status,
            )
            .await;
        });
    }
}

async fn rollback_verification_attempt(
    store: &RwLock<NetworkDiscoveryStore>,
    session_id: &str,
    candidate_id: &str,
    attempt_id: &str,
    previous_status: DiscoveryCandidateStatus,
) {
    let mut discovery = store.write().await;
    let Some(candidate) = discovery
        .sessions
        .get_mut(session_id)
        .and_then(|session| session.candidates.get_mut(candidate_id))
    else {
        return;
    };
    if candidate.verification_attempt.as_deref() != Some(attempt_id) {
        return;
    }
    candidate.verification_attempt = None;
    if candidate.dto.status == DiscoveryCandidateStatus::Verifying {
        candidate.dto.status = previous_status;
    }
}

pub(crate) struct StoredVerification {
    pub dto: CandidateVerification,
    pub fingerprint: String,
    pub expires_at_ms: i64,
}

pub(crate) struct StoredApproval {
    pub input_fingerprint: String,
    pub receipt: NetworkLinkReceipt,
}

pub(crate) struct StoredDiscoverySession {
    pub dto: NetworkDiscoverySession,
    pub organization_id: String,
    pub expires_at_ms: i64,
    pub purge_at_ms: i64,
    pub cancellation: DiscoveryCancellation,
    pub candidates: BTreeMap<String, StoredCandidate>,
}

pub(crate) struct StoredStartReceipt {
    pub organization_id: String,
    pub session_id: String,
}

#[derive(Default)]
pub(crate) struct NetworkDiscoveryStore {
    pub sessions: BTreeMap<String, StoredDiscoverySession>,
    pub start_receipts: BTreeMap<String, StoredStartReceipt>,
}

impl NetworkDiscoveryStore {
    pub fn expire_and_purge(&mut self, now_ms: i64) {
        self.sessions
            .retain(|_, session| session.purge_at_ms > now_ms);
        self.start_receipts
            .retain(|_, receipt| self.sessions.contains_key(&receipt.session_id));
        for session in self.sessions.values_mut() {
            if session.expires_at_ms <= now_ms
                && !matches!(
                    session.dto.status,
                    crate::domain::DiscoverySessionStatus::Cancelled
                        | crate::domain::DiscoverySessionStatus::Failed
                        | crate::domain::DiscoverySessionStatus::Expired
                )
            {
                session.cancellation.cancel();
                session.dto.status = crate::domain::DiscoverySessionStatus::Expired;
                session.dto.resource_version = session.dto.resource_version.saturating_add(1);
                session.candidates.clear();
                session.dto.candidate_count = 0;
            }
        }
    }
}
