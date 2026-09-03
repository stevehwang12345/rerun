use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::OsStr,
    io,
    path::Path,
    sync::Arc,
    time::{Duration, SystemTime},
};

use rms_import::ImportCancellation;
use sha2::{Digest as _, Sha256};
use tokio::sync::{Mutex, RwLock, Semaphore, watch};
use uuid::Uuid;

use crate::domain::{
    AccessMode, CommandReceipt, ControlLease, DataAssignment, DataSource, DataVisibility, Device,
    DeviceAssignment, EdgeEnrollment, Integration, LiveSession, Project, Recording,
    RecordingImport, RecordingProjectSnapshot, ReplaySession, SendCommandRequest, Topic,
};
use crate::edge_heartbeat::{EdgeHeartbeatRuntime, fixture_token, load_token_from_environment};
use crate::error::{ApiError, ApiResult};
use crate::import_storage::{
    DEFAULT_MAX_UPLOAD_BYTES, DurableRegistry, ImportStorage, StoredImportArtifact,
    StoredImportReceipt,
};
use crate::network_discovery::{
    DEFAULT_PROVIDER_VERIFICATION_TIMEOUT, DEFAULT_SESSION_TTL, DEFAULT_VERIFICATION_TTL,
    DiscoveryProvider, FakeDiscoveryProvider, MulticastDiscoveryProvider, NetworkDiscoveryStore,
};
use crate::rrd_fixture;

pub(crate) const SAMPLE_RRD_URL: &str = "/rerun/fixture/rms-replay.rrd";

#[derive(Clone)]
pub struct AppState {
    pub(crate) catalog: Arc<RwLock<Catalog>>,
    pub(crate) import_storage: Arc<ImportStorage>,
    pub(crate) registry_persist_lock: Arc<Mutex<()>>,
    pub(crate) upload_slots: Arc<Semaphore>,
    pub(crate) import_slots: Arc<Semaphore>,
    pub(crate) network_discovery: Arc<RwLock<NetworkDiscoveryStore>>,
    pub(crate) discovery_provider: Arc<dyn DiscoveryProvider>,
    pub(crate) discovery_slots: Arc<Semaphore>,
    pub(crate) discovery_verification_slots: Arc<Semaphore>,
    pub(crate) discovery_approval_lock: Arc<Mutex<()>>,
    pub(crate) discovery_session_ttl: Duration,
    pub(crate) discovery_verification_ttl: Duration,
    pub(crate) discovery_provider_verification_timeout: Duration,
    pub(crate) edge_heartbeat_token: Option<Arc<Vec<u8>>>,
    pub(crate) edge_heartbeat_runtime: Arc<Mutex<EdgeHeartbeatRuntime>>,
    pub(crate) edge_heartbeat_apply_lock: Arc<Mutex<()>>,
    pub(crate) simulated_control_enabled: bool,
    // Keeps isolated fixture storage alive for the full cloned Router/AppState lifetime.
    _temporary_storage: Option<Arc<tempfile::TempDir>>,
}

#[derive(Clone)]
pub(crate) struct Catalog {
    pub(crate) snapshot_version: u64,
    pub(crate) workspace_version: watch::Sender<u64>,
    pub(crate) integrations: BTreeMap<String, Integration>,
    pub(crate) devices: BTreeMap<String, Device>,
    pub(crate) data_sources: BTreeMap<String, DataSource>,
    pub(crate) projects: BTreeMap<String, Project>,
    pub(crate) device_assignments: BTreeMap<String, DeviceAssignment>,
    pub(crate) data_assignments: BTreeMap<String, DataAssignment>,
    pub(crate) topics_by_data_source: BTreeMap<String, Vec<Topic>>,
    pub(crate) topics_by_recording: BTreeMap<String, Vec<Topic>>,
    pub(crate) edge_enrollments: BTreeMap<String, EdgeEnrollment>,
    pub(crate) recordings: BTreeMap<String, Recording>,
    pub(crate) recording_imports: BTreeMap<String, RecordingImport>,
    pub(crate) import_artifacts: BTreeMap<String, StoredImportArtifact>,
    pub(crate) recording_artifacts: BTreeMap<String, String>,
    pub(crate) import_receipts: BTreeMap<String, StoredImportReceipt>,
    pub(crate) import_cancellations: BTreeMap<String, ImportCancellation>,
    pub(crate) live_sessions: BTreeMap<String, LiveSession>,
    pub(crate) live_session_shutdown: BTreeMap<String, watch::Sender<bool>>,
    pub(crate) replay_sessions: BTreeMap<String, ReplaySession>,
    pub(crate) leases: BTreeMap<String, ControlLease>,
    pub(crate) lease_epochs: BTreeMap<String, u64>,
    pub(crate) command_receipts: BTreeMap<String, (SendCommandRequest, CommandReceipt)>,
}

impl AppState {
    /// Creates the deterministic local-development catalog.
    pub fn fixture() -> Self {
        let temporary_storage =
            Arc::new(tempfile::tempdir().expect("isolated RMS fixture storage must be available"));
        let storage = ImportStorage::open(temporary_storage.path(), DEFAULT_MAX_UPLOAD_BYTES)
            .expect("isolated RMS fixture storage must be available");
        Self::fixture_with_import_storage(storage, Some(temporary_storage))
            .expect("the RMS durable import registry must be readable")
    }

    /// Creates the deterministic catalog backed by the configured durable OS data directory.
    pub fn from_environment() -> io::Result<Self> {
        let mut state =
            Self::fixture_with_import_storage(ImportStorage::from_environment()?, None)?;
        state.discovery_provider = Arc::new(MulticastDiscoveryProvider::from_environment()?);
        state.edge_heartbeat_token = load_token_from_environment()?;
        state.simulated_control_enabled = simulated_control_from_environment()?;
        Ok(state)
    }

    /// Creates an empty catalog for embedding or focused tests.
    pub fn empty() -> Self {
        let temporary_storage =
            Arc::new(tempfile::tempdir().expect("isolated RMS empty storage must be available"));
        let storage = ImportStorage::open(temporary_storage.path(), DEFAULT_MAX_UPLOAD_BYTES)
            .expect("isolated RMS empty storage must be available");
        Self {
            catalog: Arc::new(RwLock::new(Catalog::empty())),
            import_storage: Arc::new(storage),
            registry_persist_lock: Arc::new(Mutex::new(())),
            upload_slots: Arc::new(Semaphore::new(configured_concurrency(
                "RMS_MAX_CONCURRENT_UPLOADS",
                2,
            ))),
            import_slots: Arc::new(Semaphore::new(configured_concurrency(
                "RMS_MAX_CONCURRENT_IMPORTS",
                2,
            ))),
            network_discovery: Arc::new(RwLock::new(NetworkDiscoveryStore::default())),
            discovery_provider: Arc::new(FakeDiscoveryProvider::default()),
            discovery_slots: Arc::new(Semaphore::new(configured_concurrency(
                "RMS_MAX_CONCURRENT_DISCOVERY_SCANS",
                1,
            ))),
            discovery_verification_slots: Arc::new(Semaphore::new(configured_concurrency(
                "RMS_MAX_CONCURRENT_DISCOVERY_VERIFICATIONS",
                4,
            ))),
            discovery_approval_lock: Arc::new(Mutex::new(())),
            discovery_session_ttl: DEFAULT_SESSION_TTL,
            discovery_verification_ttl: DEFAULT_VERIFICATION_TTL,
            discovery_provider_verification_timeout: DEFAULT_PROVIDER_VERIFICATION_TIMEOUT,
            edge_heartbeat_token: Some(fixture_token()),
            edge_heartbeat_runtime: Arc::new(Mutex::new(EdgeHeartbeatRuntime::default())),
            edge_heartbeat_apply_lock: Arc::new(Mutex::new(())),
            simulated_control_enabled: true,
            _temporary_storage: Some(temporary_storage),
        }
    }

    /// Creates the fixture catalog backed by an explicit durable storage root.
    pub fn fixture_with_storage(root: impl AsRef<Path>) -> io::Result<Self> {
        Self::fixture_with_storage_limit(root, DEFAULT_MAX_UPLOAD_BYTES)
    }

    /// Creates the fixture catalog with an explicit storage root and upload limit.
    pub fn fixture_with_storage_limit(
        root: impl AsRef<Path>,
        max_upload_bytes: u64,
    ) -> io::Result<Self> {
        let storage = ImportStorage::open(root, max_upload_bytes)?;
        Self::fixture_with_import_storage(storage, None)
    }

    /// Creates the fixture catalog with explicit per-upload and total storage limits.
    pub fn fixture_with_storage_limits(
        root: impl AsRef<Path>,
        max_upload_bytes: u64,
        storage_quota_bytes: u64,
    ) -> io::Result<Self> {
        let storage = ImportStorage::open_with_limits(root, max_upload_bytes, storage_quota_bytes)?;
        Self::fixture_with_import_storage(storage, None)
    }

    fn fixture_with_import_storage(
        storage: ImportStorage,
        temporary_storage: Option<Arc<tempfile::TempDir>>,
    ) -> io::Result<Self> {
        let mut catalog = Catalog::fixture();
        let registry = storage.load_registry()?;
        let registry_schema_version = registry.schema_version;
        catalog.snapshot_version = catalog.snapshot_version.max(registry.snapshot_version);
        if registry.schema_version >= 1 {
            catalog.integrations = registry.integrations;
            catalog.devices = registry.devices;
            catalog.data_sources = registry.data_sources;
            catalog.projects = registry.projects;
            catalog.device_assignments = registry.device_assignments;
            catalog.data_assignments = registry.data_assignments;
            catalog.topics_by_data_source = registry.topics_by_data_source;
            catalog.topics_by_recording = registry.topics_by_recording;
            catalog.edge_enrollments = registry.edge_enrollments;
            catalog.recordings = registry.recordings;
        } else {
            catalog.data_sources.extend(registry.import_data_sources);
            catalog
                .data_assignments
                .extend(registry.import_data_assignments);
            for source in catalog
                .data_sources
                .values()
                .filter(|source| source.protocol == "file")
            {
                catalog.topics_by_data_source.insert(
                    source.id.clone(),
                    topics_for_source(&source.id, &source.device_id, &source.topic_ids),
                );
            }
            catalog.recordings.extend(registry.recordings);
        }
        catalog.recording_imports = registry.imports;
        catalog.import_artifacts = registry.import_artifacts;
        catalog.recording_artifacts = registry.recording_artifacts;
        catalog.import_receipts = registry.import_receipts;

        let repaired_recording_topics =
            repair_recording_topic_snapshots(&storage, &mut catalog, registry_schema_version < 6);

        let mut recovered_state = false;
        for import in catalog.recording_imports.values_mut() {
            if matches!(import.status.as_str(), "uploading" | "processing") {
                import.status = "failed".to_owned();
                import.progress_percent = 100;
                import.failure_reason = Some(
                    "Import was interrupted by a server restart. Upload the file again.".to_owned(),
                );
                import.updated_at = now_iso();
                import.resource_version = import.resource_version.saturating_add(1);
                recovered_state = true;
            }
        }
        recovered_state |= recover_edge_enrollments_after_restart(&mut catalog);
        recovered_state |= repaired_recording_topics;
        if recovered_state {
            catalog.snapshot_version = catalog.snapshot_version.saturating_add(1);
        }
        catalog
            .workspace_version
            .send_replace(catalog.snapshot_version);

        if recovered_state {
            storage.persist_registry_blocking(&catalog.durable_registry())?;
        }
        Ok(Self {
            catalog: Arc::new(RwLock::new(catalog)),
            import_storage: Arc::new(storage),
            registry_persist_lock: Arc::new(Mutex::new(())),
            upload_slots: Arc::new(Semaphore::new(configured_concurrency(
                "RMS_MAX_CONCURRENT_UPLOADS",
                2,
            ))),
            import_slots: Arc::new(Semaphore::new(configured_concurrency(
                "RMS_MAX_CONCURRENT_IMPORTS",
                2,
            ))),
            network_discovery: Arc::new(RwLock::new(NetworkDiscoveryStore::default())),
            discovery_provider: Arc::new(FakeDiscoveryProvider::default()),
            discovery_slots: Arc::new(Semaphore::new(configured_concurrency(
                "RMS_MAX_CONCURRENT_DISCOVERY_SCANS",
                1,
            ))),
            discovery_verification_slots: Arc::new(Semaphore::new(configured_concurrency(
                "RMS_MAX_CONCURRENT_DISCOVERY_VERIFICATIONS",
                4,
            ))),
            discovery_approval_lock: Arc::new(Mutex::new(())),
            discovery_session_ttl: DEFAULT_SESSION_TTL,
            discovery_verification_ttl: DEFAULT_VERIFICATION_TTL,
            discovery_provider_verification_timeout: DEFAULT_PROVIDER_VERIFICATION_TIMEOUT,
            edge_heartbeat_token: Some(fixture_token()),
            edge_heartbeat_runtime: Arc::new(Mutex::new(EdgeHeartbeatRuntime::default())),
            edge_heartbeat_apply_lock: Arc::new(Mutex::new(())),
            simulated_control_enabled: true,
            _temporary_storage: temporary_storage,
        })
    }

    /// Overrides the network discovery provider, primarily for deterministic embedding and tests.
    pub fn with_discovery_provider(mut self, provider: Arc<dyn DiscoveryProvider>) -> Self {
        self.discovery_provider = provider;
        self.network_discovery = Arc::new(RwLock::new(NetworkDiscoveryStore::default()));
        self
    }

    /// Overrides the short-lived discovery TTLs for deterministic tests.
    pub fn with_discovery_ttls(
        mut self,
        session_ttl: Duration,
        verification_ttl: Duration,
    ) -> Self {
        self.discovery_session_ttl = session_ttl;
        self.discovery_verification_ttl = verification_ttl;
        self
    }

    /// Overrides the provider-call deadline for deterministic discovery verification tests.
    pub fn with_discovery_verification_timeout(mut self, timeout: Duration) -> Self {
        self.discovery_provider_verification_timeout = timeout;
        self
    }

    /// Overrides the local Edge Agent bearer token for deterministic embedding and tests.
    pub fn with_edge_heartbeat_token(mut self, token: impl Into<Vec<u8>>) -> Self {
        self.edge_heartbeat_token = Some(Arc::new(token.into()));
        self.edge_heartbeat_runtime = Arc::new(Mutex::new(EdgeHeartbeatRuntime::default()));
        self.edge_heartbeat_apply_lock = Arc::new(Mutex::new(()));
        self
    }

    pub(crate) async fn persist_registry(&self) -> io::Result<()> {
        let _persist_guard = self.registry_persist_lock.lock().await;
        let registry = self.catalog.read().await.durable_registry();
        self.import_storage.persist_registry(registry).await
    }

    pub(crate) async fn durable_catalog_mutation<T>(
        &self,
        mutation: impl FnOnce(&mut Catalog) -> ApiResult<T>,
    ) -> ApiResult<T> {
        let _persist_guard = self.registry_persist_lock.lock().await;
        let mut catalog = self.catalog.write().await;
        let previous = catalog.clone();
        let value = match mutation(&mut catalog) {
            Ok(value) => value,
            Err(err) => {
                *catalog = previous;
                catalog
                    .workspace_version
                    .send_replace(catalog.snapshot_version);
                return Err(err);
            }
        };
        let registry = catalog.durable_registry();
        if self
            .import_storage
            .persist_registry(registry)
            .await
            .is_err()
        {
            *catalog = previous;
            catalog
                .workspace_version
                .send_replace(catalog.snapshot_version);
            return Err(ApiError::internal(
                "Catalog mutation could not be persisted.",
            ));
        }
        Ok(value)
    }
}

fn configured_concurrency(variable: &str, default: usize) -> usize {
    std::env::var(variable)
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(default)
}

fn simulated_control_from_environment() -> io::Result<bool> {
    parse_simulated_control(std::env::var_os("RMS_ENABLE_SIMULATED_CONTROL").as_deref())
}

fn parse_simulated_control(value: Option<&OsStr>) -> io::Result<bool> {
    match value {
        None => Ok(false),
        Some(value) if value == OsStr::new("1") => Ok(true),
        Some(_) => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "RMS_ENABLE_SIMULATED_CONTROL must be exactly 1 when enabled",
        )),
    }
}

fn recover_edge_enrollments_after_restart(catalog: &mut Catalog) -> bool {
    let enrollments = catalog
        .edge_enrollments
        .values()
        .cloned()
        .collect::<Vec<_>>();
    if enrollments.is_empty() {
        return false;
    }

    let mut changed = false;
    let mut enrolled_device_ids = BTreeSet::new();
    let mut affected_projects = BTreeSet::new();
    for enrollment in &enrollments {
        enrolled_device_ids.insert(enrollment.device_id.as_str());
        if let Some(device) = catalog.devices.get_mut(&enrollment.device_id)
            && (device.status != "offline" || device.health != "unknown")
        {
            device.status = "offline".to_owned();
            device.health = "unknown".to_owned();
            device.state_version = device.state_version.saturating_add(1);
            changed = true;
        }
        if let Some(integration) = catalog.integrations.get_mut(&enrollment.integration_id)
            && integration.status != "disconnected"
        {
            integration.status = "disconnected".to_owned();
            integration.resource_version = integration.resource_version.saturating_add(1);
            changed = true;
        }
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
    }
    let lease_count = catalog.leases.len();
    catalog
        .leases
        .retain(|_, lease| !enrolled_device_ids.contains(lease.device_id.as_str()));
    changed |= catalog.leases.len() != lease_count;

    for assignment in catalog.device_assignments.values() {
        if enrolled_device_ids.contains(assignment.device_id.as_str()) {
            affected_projects.insert(assignment.project_id.clone());
        }
    }
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

impl Default for AppState {
    fn default() -> Self {
        Self::fixture()
    }
}

fn refresh_latest_source_topic_display_metadata(
    catalog: &mut Catalog,
    verified_topics_by_recording: &BTreeMap<String, Vec<Topic>>,
) -> bool {
    let mut latest_recording_by_source = BTreeMap::new();
    for recording in catalog.recordings.values() {
        let candidate = (
            recording.captured_at.parse::<jiff::Timestamp>().ok(),
            recording.captured_at.clone(),
            recording.id.clone(),
        );
        let latest = latest_recording_by_source
            .entry(recording.data_source_id.clone())
            .or_insert_with(|| candidate.clone());
        if candidate > *latest {
            *latest = candidate;
        }
    }

    let mut changed = false;
    for (data_source_id, (_, _, recording_id)) in latest_recording_by_source {
        let Some(verified_topics) = verified_topics_by_recording.get(&recording_id) else {
            continue;
        };
        let Some(source_topics) = catalog.topics_by_data_source.get_mut(&data_source_id) else {
            continue;
        };
        let verified_by_identity = verified_topics
            .iter()
            .map(|topic| ((topic.id.as_str(), topic.path.as_str()), topic))
            .collect::<BTreeMap<_, _>>();
        for source_topic in source_topics {
            let Some(verified_topic) =
                verified_by_identity.get(&(source_topic.id.as_str(), source_topic.path.as_str()))
            else {
                continue;
            };

            // RRD inspection verifies the renderer and derives the label from the entity path.
            // Keep source identity and live/runtime fields intact during the one-way migration.
            if source_topic.label != verified_topic.label {
                source_topic.label.clone_from(&verified_topic.label);
                changed = true;
            }
            if source_topic.renderer != verified_topic.renderer {
                source_topic.renderer.clone_from(&verified_topic.renderer);
                changed = true;
            }
        }
    }
    changed
}

fn repair_recording_topic_snapshots(
    storage: &ImportStorage,
    catalog: &mut Catalog,
    refresh_existing: bool,
) -> bool {
    let missing_recordings = catalog
        .recordings
        .values()
        .filter(|recording| {
            refresh_existing || !catalog.topics_by_recording.contains_key(&recording.id)
        })
        .cloned()
        .collect::<Vec<_>>();
    let mut changed = false;
    let mut verified_topics_by_recording = BTreeMap::new();

    for recording in missing_recordings {
        let recording_topic_ids = recording
            .topic_ids
            .iter()
            .map(String::as_str)
            .collect::<BTreeSet<_>>();
        let artifact_topics = catalog
            .recording_artifacts
            .get(&recording.id)
            .and_then(|relative_path| storage.resolve_for_write(relative_path).ok())
            .and_then(
                |path| match rms_import::inspect_rrd_entity_descriptors(&path) {
                    Ok(entity_descriptors) => Some(topics_for_entity_descriptors(
                        &recording.data_source_id,
                        &recording.device_id,
                        &entity_descriptors,
                    )),
                    Err(err) => {
                        eprintln!(
                            "Failed to reconstruct an immutable Recording topic snapshot: {}\nFile path: {}",
                            err.internal_detail(),
                            path.display()
                        );
                        None
                    }
                },
            );
        let source_topics = || {
            catalog
                .topics_by_data_source
                .get(&recording.data_source_id)
                .map(|topics| {
                    topics
                        .iter()
                        .filter(|topic| recording.topic_ids.contains(&topic.id))
                        .cloned()
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default()
        };
        let artifact_verified = artifact_topics.is_some();
        let topics = artifact_topics.unwrap_or_else(source_topics);
        let snapshot_topic_ids = topics
            .iter()
            .map(|topic| topic.id.as_str())
            .collect::<BTreeSet<_>>();
        if snapshot_topic_ids == recording_topic_ids {
            if refresh_existing && artifact_verified {
                verified_topics_by_recording.insert(recording.id.clone(), topics.clone());
            }
            catalog
                .topics_by_recording
                .insert(recording.id.clone(), topics);
            changed = true;
        } else {
            eprintln!(
                "Refused to reconstruct an incomplete Recording topic snapshot: expected {} IDs, reconstructed {}\nRecording ID: {}",
                recording_topic_ids.len(),
                snapshot_topic_ids.len(),
                recording.id
            );
        }
    }

    if refresh_existing {
        changed |=
            refresh_latest_source_topic_display_metadata(catalog, &verified_topics_by_recording);
    }

    changed
}

impl Catalog {
    fn empty() -> Self {
        let (workspace_version, _receiver) = watch::channel(0);
        Self {
            snapshot_version: 0,
            workspace_version,
            integrations: BTreeMap::new(),
            devices: BTreeMap::new(),
            data_sources: BTreeMap::new(),
            projects: BTreeMap::new(),
            device_assignments: BTreeMap::new(),
            data_assignments: BTreeMap::new(),
            topics_by_data_source: BTreeMap::new(),
            topics_by_recording: BTreeMap::new(),
            edge_enrollments: BTreeMap::new(),
            recordings: BTreeMap::new(),
            recording_imports: BTreeMap::new(),
            import_artifacts: BTreeMap::new(),
            recording_artifacts: BTreeMap::new(),
            import_receipts: BTreeMap::new(),
            import_cancellations: BTreeMap::new(),
            live_sessions: BTreeMap::new(),
            live_session_shutdown: BTreeMap::new(),
            replay_sessions: BTreeMap::new(),
            leases: BTreeMap::new(),
            lease_epochs: BTreeMap::new(),
            command_receipts: BTreeMap::new(),
        }
    }

    fn fixture() -> Self {
        let mut catalog = Self::empty();
        catalog.snapshot_version = 12;

        let observed_at = "2026-08-20T09:32:08+09:00".to_owned();
        let integration = Integration {
            id: "integration-logistics".to_owned(),
            organization_id: "org-rms".to_owned(),
            name: "물류 ROS 2".to_owned(),
            kind: "ros2".to_owned(),
            status: "connected".to_owned(),
            endpoint_label: "A동 Edge Gateway".to_owned(),
            last_health_at: observed_at.clone(),
            created_at: "2026-08-01T00:00:00+09:00".to_owned(),
            resource_version: 1,
        };
        catalog
            .integrations
            .insert(integration.id.clone(), integration);

        let drone_integration = Integration {
            id: "integration-inspection".to_owned(),
            organization_id: "org-rms".to_owned(),
            name: "점검 MAVLink".to_owned(),
            kind: "mavlink".to_owned(),
            status: "connected".to_owned(),
            endpoint_label: "야외 Drone Gateway".to_owned(),
            last_health_at: observed_at.clone(),
            created_at: "2026-08-02T00:00:00+09:00".to_owned(),
            resource_version: 1,
        };
        catalog
            .integrations
            .insert(drone_integration.id.clone(), drone_integration);

        for device in [
            Device {
                id: "robot-07".to_owned(),
                organization_id: "org-rms".to_owned(),
                integration_id: "integration-logistics".to_owned(),
                name: "Robot-07".to_owned(),
                kind: "robot".to_owned(),
                status: "online".to_owned(),
                health: "normal".to_owned(),
                operation_mode: "자율 운행".to_owned(),
                battery_percent: Some(78),
                task_name: "Bay 3 이동".to_owned(),
                task_progress: 62,
                last_seen_at: observed_at.clone(),
                state_version: 142,
            },
            Device {
                id: "robot-12".to_owned(),
                organization_id: "org-rms".to_owned(),
                integration_id: "integration-logistics".to_owned(),
                name: "Robot-12".to_owned(),
                kind: "robot".to_owned(),
                status: "degraded".to_owned(),
                health: "attention".to_owned(),
                operation_mode: "대기".to_owned(),
                battery_percent: Some(41),
                task_name: "충전 위치 이동".to_owned(),
                task_progress: 18,
                last_seen_at: "2026-08-20T09:32:05+09:00".to_owned(),
                state_version: 87,
            },
            Device {
                id: "robot-21".to_owned(),
                organization_id: "org-rms".to_owned(),
                integration_id: "integration-logistics".to_owned(),
                name: "Robot-21".to_owned(),
                kind: "robot".to_owned(),
                status: "offline".to_owned(),
                health: "restricted".to_owned(),
                operation_mode: "점검".to_owned(),
                battery_percent: Some(0),
                task_name: "정비 중".to_owned(),
                task_progress: 0,
                last_seen_at: "2026-08-20T08:51:00+09:00".to_owned(),
                state_version: 31,
            },
            Device {
                id: "drone-03".to_owned(),
                organization_id: "org-rms".to_owned(),
                integration_id: "integration-inspection".to_owned(),
                name: "Drone-03".to_owned(),
                kind: "drone".to_owned(),
                status: "online".to_owned(),
                health: "normal".to_owned(),
                operation_mode: "임무 대기".to_owned(),
                battery_percent: Some(86),
                task_name: "동측 패널 점검".to_owned(),
                task_progress: 0,
                last_seen_at: "2026-08-20T09:31:59+09:00".to_owned(),
                state_version: 55,
            },
        ] {
            catalog.devices.insert(device.id.clone(), device);
        }

        for mut source in [
            fixture_source(
                "robot-07-source",
                "integration-logistics",
                "robot-07",
                "Robot-07 실시간",
                &["pose", "front-camera", "velocity", "battery", "planner"],
            ),
            fixture_source(
                "robot-12-source",
                "integration-logistics",
                "robot-12",
                "Robot-12 실시간",
                &["pose", "front-camera", "velocity", "battery", "planner"],
            ),
            fixture_source(
                "robot-21-source",
                "integration-logistics",
                "robot-21",
                "Robot-21 실시간",
                &["pose", "front-camera", "velocity", "battery"],
            ),
            fixture_source(
                "drone-03-source",
                "integration-inspection",
                "drone-03",
                "Drone-03 실시간",
                &["pose", "front-camera", "altitude", "battery", "planner"],
            ),
        ] {
            let topics = topics_for_source(&source.id, &source.device_id, &source.topic_ids);
            source.topic_ids = topics.iter().map(|topic| topic.id.clone()).collect();
            catalog
                .topics_by_data_source
                .insert(source.id.clone(), topics);
            catalog.data_sources.insert(source.id.clone(), source);
        }

        for project in [
            Project {
                id: "project-logistics".to_owned(),
                organization_id: "org-rms".to_owned(),
                name: "물류 자동화".to_owned(),
                description: "A동 물류 로봇 운영".to_owned(),
                status: "active".to_owned(),
                device_count: 3,
                online_device_count: 1,
                created_at: "2026-08-01T00:00:00+09:00".to_owned(),
                resource_version: 1,
            },
            Project {
                id: "project-inspection".to_owned(),
                organization_id: "org-rms".to_owned(),
                name: "시설 점검".to_owned(),
                description: "야외 설비 드론 점검".to_owned(),
                status: "standby".to_owned(),
                device_count: 1,
                online_device_count: 1,
                created_at: "2026-08-01T00:00:00+09:00".to_owned(),
                resource_version: 1,
            },
        ] {
            catalog.projects.insert(project.id.clone(), project);
        }

        for (project_id, device_id, access_mode) in [
            ("project-logistics", "robot-07", AccessMode::Control),
            ("project-logistics", "robot-12", AccessMode::Observe),
            ("project-logistics", "robot-21", AccessMode::Observe),
            ("project-inspection", "drone-03", AccessMode::Control),
        ] {
            let assignment = fixture_device_assignment(project_id, device_id, access_mode);
            catalog
                .device_assignments
                .insert(assignment.id.clone(), assignment);
        }
        for (project_id, source_id) in [
            ("project-logistics", "robot-07-source"),
            ("project-logistics", "robot-12-source"),
            ("project-logistics", "robot-21-source"),
            ("project-inspection", "drone-03-source"),
        ] {
            let assignment = fixture_data_assignment(project_id, source_id);
            catalog
                .data_assignments
                .insert(assignment.id.clone(), assignment);
        }

        for recording in [
            fixture_recording(
                "recording-robot-07-incident",
                "project-logistics",
                "물류 자동화",
                "robot-07",
                "robot-07-source",
                "08:42 경로 이탈",
                10,
            ),
            fixture_recording(
                "recording-robot-21-maintenance",
                "project-logistics",
                "물류 자동화",
                "robot-21",
                "robot-21-source",
                "정비 전 운행",
                10,
            ),
            fixture_recording(
                "recording-drone-03-inspection",
                "project-inspection",
                "시설 점검",
                "drone-03",
                "drone-03-source",
                "서측 패널 점검",
                11,
            ),
        ] {
            let topics = catalog
                .topics_by_data_source
                .get(&recording.data_source_id)
                .map(|source_topics| {
                    source_topics
                        .iter()
                        .filter(|topic| recording.topic_ids.contains(&topic.id))
                        .cloned()
                        .collect()
                })
                .unwrap_or_default();
            catalog
                .topics_by_recording
                .insert(recording.id.clone(), topics);
            catalog.recordings.insert(recording.id.clone(), recording);
        }

        catalog
            .workspace_version
            .send_replace(catalog.snapshot_version);

        catalog
    }

    pub(crate) fn bump_version(&mut self) -> u64 {
        self.snapshot_version = self.snapshot_version.saturating_add(1);
        self.workspace_version.send_replace(self.snapshot_version);
        self.snapshot_version
    }

    pub(crate) fn durable_registry(&self) -> DurableRegistry {
        DurableRegistry {
            schema_version: 6,
            snapshot_version: self.snapshot_version,
            integrations: self.integrations.clone(),
            devices: self.devices.clone(),
            data_sources: self.data_sources.clone(),
            projects: self.projects.clone(),
            device_assignments: self.device_assignments.clone(),
            data_assignments: self.data_assignments.clone(),
            topics_by_data_source: self.topics_by_data_source.clone(),
            topics_by_recording: self.topics_by_recording.clone(),
            edge_enrollments: self.edge_enrollments.clone(),
            imports: self.recording_imports.clone(),
            recordings: self.recordings.clone(),
            import_data_sources: BTreeMap::new(),
            import_data_assignments: BTreeMap::new(),
            import_artifacts: self.import_artifacts.clone(),
            recording_artifacts: self.recording_artifacts.clone(),
            import_receipts: self.import_receipts.clone(),
        }
    }

    pub(crate) fn refresh_project_counts(&mut self, project_id: &str) {
        let assigned_device_ids = self
            .device_assignments
            .values()
            .filter(|assignment| assignment.project_id == project_id)
            .map(|assignment| assignment.device_id.as_str())
            .collect::<Vec<_>>();
        let online_device_count = assigned_device_ids
            .iter()
            .filter(|device_id| {
                self.devices
                    .get(**device_id)
                    .is_some_and(|device| device.status == "online")
            })
            .count();
        if let Some(project) = self.projects.get_mut(project_id) {
            project.device_count = assigned_device_ids.len();
            project.online_device_count = online_device_count;
        }
    }
}

pub(crate) fn new_id(prefix: &str) -> String {
    format!("{prefix}-{}", Uuid::new_v4())
}

pub(crate) fn now_iso() -> String {
    jiff::Timestamp::now().to_string()
}

pub(crate) fn now_ms() -> i64 {
    let millis = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_millis());
    i64::try_from(millis).unwrap_or(i64::MAX)
}

pub(crate) fn iso_from_millis(millis: i64) -> String {
    jiff::Timestamp::from_millisecond(millis)
        .map_or_else(|_| now_iso(), |timestamp| timestamp.to_string())
}

fn fixture_source(
    id: &str,
    integration_id: &str,
    device_id: &str,
    name: &str,
    topic_ids: &[&str],
) -> DataSource {
    DataSource {
        id: id.to_owned(),
        integration_id: integration_id.to_owned(),
        device_id: device_id.to_owned(),
        name: name.to_owned(),
        protocol: if integration_id.contains("mavlink") {
            "mavlink"
        } else {
            "ros2"
        }
        .to_owned(),
        status: "recording".to_owned(),
        live_url: SAMPLE_RRD_URL.to_owned(),
        topic_ids: topic_ids.iter().map(|topic| (*topic).to_owned()).collect(),
        mapping_version: 1,
        last_data_at: "2026-08-20T09:32:08+09:00".to_owned(),
    }
}

fn fixture_device_assignment(
    project_id: &str,
    device_id: &str,
    access_mode: AccessMode,
) -> DeviceAssignment {
    DeviceAssignment {
        id: format!("device-assignment-{project_id}-{device_id}"),
        project_id: project_id.to_owned(),
        device_id: device_id.to_owned(),
        access_mode,
        valid_from: "2026-08-20T00:00:00Z".to_owned(),
        valid_to: None,
        resource_version: 1,
    }
}

fn fixture_data_assignment(project_id: &str, data_source_id: &str) -> DataAssignment {
    DataAssignment {
        id: format!("data-assignment-{project_id}-{data_source_id}"),
        project_id: project_id.to_owned(),
        data_source_id: data_source_id.to_owned(),
        visibility: DataVisibility::Operator,
        valid_from: "2026-08-20T00:00:00Z".to_owned(),
        valid_to: None,
        resource_version: 1,
    }
}

fn fixture_recording(
    id: &str,
    project_id: &str,
    project_name: &str,
    device_id: &str,
    data_source_id: &str,
    name: &str,
    snapshot_version: u64,
) -> Recording {
    Recording {
        id: id.to_owned(),
        organization_id: "org-rms".to_owned(),
        project_id: project_id.to_owned(),
        device_id: device_id.to_owned(),
        data_source_id: data_source_id.to_owned(),
        name: name.to_owned(),
        status: "ready".to_owned(),
        rrd_url: SAMPLE_RRD_URL.to_owned(),
        captured_at: "2026-08-20T08:42:10+09:00".to_owned(),
        duration_label: rrd_fixture::DURATION_LABEL.to_owned(),
        timelines: rrd_fixture::timelines(),
        default_timeline: rrd_fixture::DEFAULT_TIMELINE.to_owned(),
        duration_seconds: rrd_fixture::DURATION_SECONDS,
        rrd_version: rrd_fixture::RRD_VERSION.to_owned(),
        footer_verified: true,
        content_sha256: rrd_fixture::content_sha256().to_owned(),
        topic_ids: vec![
            "pose".to_owned(),
            "front-camera".to_owned(),
            "velocity".to_owned(),
            "battery".to_owned(),
        ],
        mapping_version: 1,
        project_snapshot: RecordingProjectSnapshot {
            project_id: project_id.to_owned(),
            project_name: project_name.to_owned(),
            captured_at: "2026-08-20T08:46:28+09:00".to_owned(),
            device_assignment_id: format!("device-assignment-{project_id}-{device_id}"),
            data_assignment_id: format!("data-assignment-{project_id}-{data_source_id}"),
        },
        resource_version: snapshot_version,
        manifest_hash: format!("sha256:{}", rrd_fixture::content_sha256()),
    }
}

pub(crate) fn topics_for_source(
    data_source_id: &str,
    device_id: &str,
    topic_ids: &[String],
) -> Vec<Topic> {
    let mut topics_by_path = BTreeMap::new();
    for raw_topic_id in topic_ids {
        let raw_topic_id = raw_topic_id.trim();
        if raw_topic_id.is_empty() {
            continue;
        }
        let (id, path, label, renderer, value, unit, message) = match raw_topic_id {
            "pose" => (
                "pose".to_owned(),
                "/localization/pose".to_owned(),
                "위치와 주변".to_owned(),
                "spatial".to_owned(),
                Some("Bay 2 → Bay 3"),
                None,
                None,
            ),
            "front-camera" => (
                "front-camera".to_owned(),
                "/camera/front/image".to_owned(),
                "전방 카메라".to_owned(),
                "camera".to_owned(),
                Some("30 fps"),
                None,
                None,
            ),
            "velocity" => (
                "velocity".to_owned(),
                "/vehicle/velocity".to_owned(),
                "속도".to_owned(),
                "timeseries".to_owned(),
                Some("1.2"),
                Some("m/s"),
                None,
            ),
            "altitude" => (
                "altitude".to_owned(),
                "/flight/altitude".to_owned(),
                "고도".to_owned(),
                "timeseries".to_owned(),
                Some("0"),
                Some("m"),
                None,
            ),
            "battery" => (
                "battery".to_owned(),
                "/power/battery".to_owned(),
                "배터리".to_owned(),
                "state".to_owned(),
                Some("78"),
                Some("%"),
                None,
            ),
            "planner" => (
                "planner".to_owned(),
                "/planning/status".to_owned(),
                "경로 계획".to_owned(),
                "log".to_owned(),
                None,
                None,
                Some("전방 통로를 확인했습니다."),
            ),
            dynamic => {
                let path = normalize_topic_path(dynamic);
                let label = path
                    .rsplit('/')
                    .find(|segment| !segment.is_empty())
                    .unwrap_or("data")
                    .replace(['_', '-'], " ");
                let renderer = inferred_renderer(&path).to_owned();
                let id = stable_topic_id(data_source_id, &path);
                (id, path, label, renderer, None, None, None)
            }
        };
        topics_by_path.entry(path.clone()).or_insert_with(|| Topic {
            id,
            data_source_id: data_source_id.to_owned(),
            device_id: device_id.to_owned(),
            path,
            label,
            renderer,
            quality: "fresh".to_owned(),
            value: value.map(ToOwned::to_owned),
            unit: unit.map(ToOwned::to_owned),
            message: message.map(ToOwned::to_owned),
            samples: None,
            updated_at: "2026-08-20T09:32:08+09:00".to_owned(),
        });
    }
    topics_by_path.into_values().collect()
}

/// Materializes immutable imported Recording topics from verified RRD archetype metadata.
///
/// Every descriptor came from a verified RRD manifest, so it overrides path guessing. In
/// particular, unknown/raw components must remain `raw`: routing `/scan` or `/velocity` by name to
/// an incompatible spatial/time-series visualizer merely creates another empty view.
/// Stable source-scoped Topic identities are never changed.
pub(crate) fn topics_for_entity_descriptors(
    data_source_id: &str,
    device_id: &str,
    descriptors: &[rms_import::EntityDescriptor],
) -> Vec<Topic> {
    let paths = descriptors
        .iter()
        .map(|descriptor| descriptor.path.clone())
        .collect::<Vec<_>>();
    let renderer_by_path = descriptors
        .iter()
        .map(|descriptor| {
            (
                normalize_topic_path(&descriptor.path),
                descriptor.visualization.renderer(),
            )
        })
        .collect::<BTreeMap<_, _>>();
    let mut topics = topics_for_source(data_source_id, device_id, &paths);
    for topic in &mut topics {
        if let Some(renderer) = renderer_by_path.get(&topic.path) {
            topic.renderer = (*renderer).to_owned();
        }
    }
    topics
}

fn normalize_topic_path(raw: &str) -> String {
    let normalized = raw.replace('\\', "/");
    let segments = normalized
        .split('/')
        .filter_map(|segment| {
            let segment = segment.trim();
            if segment.is_empty() || segment == "." {
                None
            } else if segment == ".." {
                Some("_")
            } else {
                Some(segment)
            }
        })
        .collect::<Vec<_>>();
    if segments.is_empty() {
        "/data".to_owned()
    } else {
        format!("/{}", segments.join("/"))
    }
}

fn inferred_renderer(path: &str) -> &'static str {
    let path = path.to_ascii_lowercase();
    if ["camera", "image", "video"]
        .iter()
        .any(|hint| path.contains(hint))
    {
        "camera"
    } else if ["point", "cloud", "lidar", "mesh", "pose", "transform"]
        .iter()
        .any(|hint| path.contains(hint))
    {
        "spatial"
    } else if ["velocity", "speed", "battery", "temperature", "altitude"]
        .iter()
        .any(|hint| path.contains(hint))
    {
        "timeseries"
    } else {
        "log"
    }
}

fn stable_topic_id(data_source_id: &str, path: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data_source_id.as_bytes());
    hasher.update([0]);
    hasher.update(path.as_bytes());
    let digest = format!("{:x}", hasher.finalize());
    format!("topic-{}", &digest[..16])
}

#[cfg(test)]
mod tests {
    use std::{collections::BTreeMap, ffi::OsStr};

    use crate::domain::{ControlLease, EdgeEnrollment};

    use super::{
        AppState, Catalog, fixture_recording, parse_simulated_control,
        recover_edge_enrollments_after_restart, refresh_latest_source_topic_display_metadata,
        topics_for_entity_descriptors, topics_for_source,
    };

    #[test]
    fn dynamic_topics_keep_real_paths_and_source_scoped_ids() {
        let topics = topics_for_source(
            "file-source-a",
            "robot-a",
            &[
                "csv/temperature".to_owned(),
                "/csv/temperature".to_owned(),
                "camera/rear/image".to_owned(),
            ],
        );
        assert_eq!(topics.len(), 2);
        let temperature = topics
            .iter()
            .find(|topic| topic.path == "/csv/temperature")
            .expect("temperature topic exists");
        assert_eq!(temperature.label, "temperature");
        assert_eq!(temperature.renderer, "timeseries");
        assert!(temperature.value.is_none());
        assert!(temperature.message.is_none());
        assert!(temperature.id.starts_with("topic-"));

        let other_source =
            topics_for_source("file-source-b", "robot-a", &["csv/temperature".to_owned()]);
        assert_ne!(temperature.id, other_source[0].id);
    }

    #[test]
    fn verified_archetypes_override_path_guessing_without_changing_topic_identity() {
        use rms_import::{EntityDescriptor, EntityVisualization};

        let topics = topics_for_entity_descriptors(
            "file-source-a",
            "robot-a",
            &[
                EntityDescriptor {
                    path: "/map".to_owned(),
                    visualization: EntityVisualization::Spatial3d,
                },
                EntityDescriptor {
                    path: "/tf_drop".to_owned(),
                    visualization: EntityVisualization::Transform3d,
                },
                EntityDescriptor {
                    path: "/raw/velocity".to_owned(),
                    visualization: EntityVisualization::Raw,
                },
            ],
        );
        assert_eq!(topics.len(), 3);
        assert_eq!(
            topics
                .iter()
                .find(|topic| topic.path == "/map")
                .unwrap()
                .renderer,
            "spatial3d"
        );
        assert_eq!(
            topics
                .iter()
                .find(|topic| topic.path == "/tf_drop")
                .unwrap()
                .renderer,
            "transform3d"
        );
        assert_eq!(
            topics
                .iter()
                .find(|topic| topic.path == "/raw/velocity")
                .unwrap()
                .renderer,
            "raw"
        );
        assert!(topics.iter().all(|topic| topic.id.starts_with("topic-")));
    }

    #[test]
    fn schema_six_repair_refreshes_only_latest_source_display_metadata() {
        use rms_import::{EntityDescriptor, EntityVisualization};

        let source_id = "file-source-a";
        let mut catalog = Catalog::empty();
        let mut source_topics = topics_for_source(source_id, "live-robot", &["/map".to_owned()]);
        let source_topic = source_topics.first_mut().unwrap();
        source_topic.label = "stale map label".to_owned();
        source_topic.renderer = "log".to_owned();
        source_topic.quality = "stale".to_owned();
        source_topic.value = Some("live value".to_owned());
        source_topic.unit = Some("cells".to_owned());
        source_topic.message = Some("live message".to_owned());
        source_topic.samples = Some(vec![1.0, 2.0]);
        source_topic.updated_at = "2026-08-22T00:00:00Z".to_owned();
        let source_topic_id = source_topic.id.clone();
        catalog
            .topics_by_data_source
            .insert(source_id.to_owned(), source_topics);

        // IDs intentionally sort in the opposite order to capture time-based, deterministic
        // selection rather than relying on BTreeMap iteration order.
        let mut older = fixture_recording(
            "recording-z-older",
            "project-logistics",
            "Logistics",
            "recorded-robot",
            source_id,
            "Older",
            1,
        );
        older.captured_at = "2026-08-20T00:00:00Z".to_owned();
        older.topic_ids = vec![source_topic_id.clone()];
        let mut latest = fixture_recording(
            "recording-a-latest",
            "project-logistics",
            "Logistics",
            "recorded-robot",
            source_id,
            "Latest",
            2,
        );
        latest.captured_at = "2026-08-20T01:00:00Z".to_owned();
        latest.topic_ids = vec![source_topic_id.clone()];
        catalog.recordings.insert(older.id.clone(), older.clone());
        catalog.recordings.insert(latest.id.clone(), latest.clone());

        let mut older_topics = topics_for_entity_descriptors(
            source_id,
            &older.device_id,
            &[EntityDescriptor {
                path: "/map".to_owned(),
                visualization: EntityVisualization::Spatial2d,
            }],
        );
        older_topics[0].label = "older verified label".to_owned();
        let only_older = BTreeMap::from([(older.id.clone(), older_topics.clone())]);
        assert!(!refresh_latest_source_topic_display_metadata(
            &mut catalog,
            &only_older,
        ));
        assert_eq!(
            catalog.topics_by_data_source[source_id][0].renderer, "log",
            "an older verified snapshot must not overwrite the mutable latest source projection"
        );

        let mut latest_topics = topics_for_entity_descriptors(
            source_id,
            &latest.device_id,
            &[EntityDescriptor {
                path: "/map".to_owned(),
                visualization: EntityVisualization::Spatial3d,
            }],
        );
        latest_topics[0].label = "latest verified label".to_owned();
        let verified_topics =
            BTreeMap::from([(older.id, older_topics), (latest.id, latest_topics)]);
        assert!(refresh_latest_source_topic_display_metadata(
            &mut catalog,
            &verified_topics,
        ));

        let repaired = &catalog.topics_by_data_source[source_id][0];
        assert_eq!(repaired.label, "latest verified label");
        assert_eq!(repaired.renderer, "spatial3d");
        assert_eq!(repaired.id, source_topic_id);
        assert_eq!(repaired.data_source_id, source_id);
        assert_eq!(repaired.device_id, "live-robot");
        assert_eq!(repaired.path, "/map");
        assert_eq!(repaired.quality, "stale");
        assert_eq!(repaired.value.as_deref(), Some("live value"));
        assert_eq!(repaired.unit.as_deref(), Some("cells"));
        assert_eq!(repaired.message.as_deref(), Some("live message"));
        assert_eq!(repaired.samples, Some(vec![1.0, 2.0]));
        assert_eq!(repaired.updated_at, "2026-08-22T00:00:00Z");
        assert!(!refresh_latest_source_topic_display_metadata(
            &mut catalog,
            &verified_topics,
        ));
    }

    #[test]
    fn schema_five_registry_migrates_source_metadata_and_recording_snapshots_to_six() {
        use crate::import_storage::{DEFAULT_MAX_UPLOAD_BYTES, ImportStorage};
        use crate::rrd_fixture;

        let root = tempfile::tempdir().unwrap();
        let storage = ImportStorage::open(root.path(), DEFAULT_MAX_UPLOAD_BYTES).unwrap();
        let relative_artifact_path = "recordings/verified.rrd";
        let artifact_path = storage.resolve_for_write(relative_artifact_path).unwrap();
        std::fs::create_dir_all(artifact_path.parent().unwrap()).unwrap();
        std::fs::write(&artifact_path, rrd_fixture::BYTES).unwrap();

        let source_id = "migrated-source";
        let descriptors = rms_import::inspect_rrd_entity_descriptors(&artifact_path).unwrap();
        let verified_topics =
            topics_for_entity_descriptors(source_id, "recorded-device", &descriptors);
        assert!(!verified_topics.is_empty());

        let mut stale_source_topics = verified_topics.clone();
        for topic in &mut stale_source_topics {
            topic.device_id = "live-device".to_owned();
            topic.label = "stale label".to_owned();
            topic.renderer = "log".to_owned();
            topic.quality = "stale".to_owned();
            topic.value = Some("live value".to_owned());
            topic.unit = Some("live unit".to_owned());
            topic.message = Some("live message".to_owned());
            topic.samples = Some(vec![3.0, 5.0]);
            topic.updated_at = "2026-08-22T00:00:00Z".to_owned();
        }

        let mut recording = fixture_recording(
            "recording-schema-five",
            "project-logistics",
            "Logistics",
            "recorded-device",
            source_id,
            "Schema five recording",
            1,
        );
        recording.topic_ids = verified_topics
            .iter()
            .map(|topic| topic.id.clone())
            .collect();

        let mut catalog = Catalog::empty();
        catalog
            .topics_by_data_source
            .insert(source_id.to_owned(), stale_source_topics.clone());
        catalog
            .topics_by_recording
            .insert(recording.id.clone(), stale_source_topics);
        catalog
            .recording_artifacts
            .insert(recording.id.clone(), relative_artifact_path.to_owned());
        catalog
            .recordings
            .insert(recording.id.clone(), recording.clone());

        let mut schema_five_registry = catalog.durable_registry();
        schema_five_registry.schema_version = 5;
        storage
            .persist_registry_blocking(&schema_five_registry)
            .unwrap();
        drop(storage);

        let restarted = AppState::fixture_with_storage(root.path()).unwrap();
        let migrated_registry = restarted.import_storage.load_registry().unwrap();
        assert_eq!(migrated_registry.schema_version, 6);

        let catalog = restarted.catalog.try_read().unwrap();
        let migrated_source_topics = &catalog.topics_by_data_source[source_id];
        let migrated_recording_topics = &catalog.topics_by_recording[&recording.id];
        for verified in &verified_topics {
            let source_topic = migrated_source_topics
                .iter()
                .find(|topic| topic.id == verified.id && topic.path == verified.path)
                .unwrap();
            assert_eq!(source_topic.label, verified.label);
            assert_eq!(source_topic.renderer, verified.renderer);
            assert_eq!(source_topic.data_source_id, source_id);
            assert_eq!(source_topic.device_id, "live-device");
            assert_eq!(source_topic.quality, "stale");
            assert_eq!(source_topic.value.as_deref(), Some("live value"));
            assert_eq!(source_topic.unit.as_deref(), Some("live unit"));
            assert_eq!(source_topic.message.as_deref(), Some("live message"));
            assert_eq!(source_topic.samples, Some(vec![3.0, 5.0]));
            assert_eq!(source_topic.updated_at, "2026-08-22T00:00:00Z");

            let recording_topic = migrated_recording_topics
                .iter()
                .find(|topic| topic.id == verified.id && topic.path == verified.path)
                .unwrap();
            assert_eq!(recording_topic.renderer, verified.renderer);
            assert_eq!(recording_topic.device_id, "recorded-device");
        }
    }

    fn enroll_fixture_robot(catalog: &mut Catalog) {
        catalog.edge_enrollments.insert(
            "edge-fixture".to_owned(),
            EdgeEnrollment {
                edge_device_id: "edge-fixture".to_owned(),
                public_key: "fixture-public-key".to_owned(),
                organization_id: "org-rms".to_owned(),
                integration_id: "integration-logistics".to_owned(),
                device_id: "robot-07".to_owned(),
                source_ids: BTreeMap::from([(
                    "ros2-graph".to_owned(),
                    "robot-07-source".to_owned(),
                )]),
                organization_trusted: true,
                created_at: "2026-08-21T00:00:00Z".to_owned(),
            },
        );
    }

    #[test]
    fn startup_recovery_fails_enrolled_devices_closed_and_revokes_leases() {
        let mut catalog = Catalog::fixture();
        enroll_fixture_robot(&mut catalog);
        catalog.leases.insert(
            "lease-edge".to_owned(),
            ControlLease {
                id: "lease-edge".to_owned(),
                live_session_id: "live-edge".to_owned(),
                device_id: "robot-07".to_owned(),
                holder_id: "operator".to_owned(),
                holder_name: "Operator".to_owned(),
                expires_at: "2099-01-01T00:00:00Z".to_owned(),
                epoch: 1,
                expires_at_ms: i64::MAX,
            },
        );

        assert!(recover_edge_enrollments_after_restart(&mut catalog));
        assert_eq!(catalog.devices["robot-07"].status, "offline");
        assert_eq!(catalog.devices["robot-07"].health, "unknown");
        assert_eq!(
            catalog.integrations["integration-logistics"].status,
            "disconnected"
        );
        assert_eq!(catalog.data_sources["robot-07-source"].status, "pending");
        assert!(
            catalog.topics_by_data_source["robot-07-source"]
                .iter()
                .all(|topic| topic.quality == "stale")
        );
        assert!(catalog.leases.is_empty());
        assert!(!recover_edge_enrollments_after_restart(&mut catalog));
    }

    #[tokio::test]
    async fn startup_recovery_is_persisted_before_serving() {
        let root = tempfile::tempdir().unwrap();
        let state = AppState::fixture_with_storage(root.path()).unwrap();
        let stored_version;
        {
            let mut catalog = state.catalog.write().await;
            enroll_fixture_robot(&mut catalog);
            catalog.devices.get_mut("robot-07").unwrap().status = "online".to_owned();
            catalog.devices.get_mut("robot-07").unwrap().health = "normal".to_owned();
            catalog
                .integrations
                .get_mut("integration-logistics")
                .unwrap()
                .status = "connected".to_owned();
            catalog
                .data_sources
                .get_mut("robot-07-source")
                .unwrap()
                .status = "recording".to_owned();
            for topic in catalog
                .topics_by_data_source
                .get_mut("robot-07-source")
                .unwrap()
            {
                topic.quality = "fresh".to_owned();
            }
            catalog.bump_version();
            stored_version = catalog.snapshot_version;
        }
        state.persist_registry().await.unwrap();
        drop(state);

        let restarted = AppState::fixture_with_storage(root.path()).unwrap();
        let recovered_version = restarted.catalog.read().await.snapshot_version;
        {
            let catalog = restarted.catalog.read().await;
            assert_eq!(catalog.devices["robot-07"].status, "offline");
            assert_eq!(
                catalog.integrations["integration-logistics"].status,
                "disconnected"
            );
            assert_eq!(catalog.data_sources["robot-07-source"].status, "pending");
            assert!(
                catalog.topics_by_data_source["robot-07-source"]
                    .iter()
                    .all(|topic| topic.quality == "stale")
            );
        }
        assert_eq!(recovered_version, stored_version.saturating_add(1));
        drop(restarted);

        let second_restart = AppState::fixture_with_storage(root.path()).unwrap();
        assert_eq!(
            second_restart.catalog.read().await.snapshot_version,
            recovered_version
        );
    }

    #[test]
    fn production_simulated_control_is_opt_in_with_a_strict_value() {
        assert!(!parse_simulated_control(None).unwrap());
        assert!(parse_simulated_control(Some(OsStr::new("1"))).unwrap());
        assert!(parse_simulated_control(Some(OsStr::new("true"))).is_err());
        assert!(parse_simulated_control(Some(OsStr::new("0"))).is_err());
    }
}
