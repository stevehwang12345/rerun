use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Integration {
    pub id: String,
    pub organization_id: String,
    pub name: String,
    pub kind: String,
    pub status: String,
    pub endpoint_label: String,
    pub last_health_at: String,
    pub created_at: String,
    pub resource_version: u64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateIntegrationRequest {
    pub organization_id: String,
    pub name: String,
    pub kind: String,
    pub endpoint_label: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Device {
    pub id: String,
    pub organization_id: String,
    pub integration_id: String,
    pub name: String,
    pub kind: String,
    pub status: String,
    pub health: String,
    pub operation_mode: String,
    pub battery_percent: Option<u8>,
    pub task_name: String,
    pub task_progress: u8,
    pub last_seen_at: String,
    pub state_version: u64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateDeviceRequest {
    #[serde(default)]
    pub id: Option<String>,
    pub organization_id: String,
    pub integration_id: String,
    pub name: String,
    pub kind: String,
    #[serde(default = "default_online")]
    pub status: String,
    #[serde(default = "default_normal")]
    pub health: String,
    #[serde(default)]
    pub operation_mode: String,
    #[serde(default)]
    pub battery_percent: Option<u8>,
    #[serde(default)]
    pub task_name: String,
    #[serde(default)]
    pub task_progress: u8,
    #[serde(default)]
    pub last_seen_at: Option<String>,
}

fn default_online() -> String {
    "online".to_owned()
}

fn default_normal() -> String {
    "normal".to_owned()
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DataSource {
    pub id: String,
    pub integration_id: String,
    pub device_id: String,
    pub name: String,
    pub protocol: String,
    pub status: String,
    pub live_url: String,
    pub topic_ids: Vec<String>,
    pub mapping_version: u64,
    pub last_data_at: String,
}

/// Durable trust binding created only after an operator approves a signed RMS Edge candidate.
///
/// This record is never returned by the catalog APIs. It binds signed heartbeats to the exact
/// Integration, Device, and `DataSources` that were created by the approval transaction.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EdgeEnrollment {
    pub edge_device_id: String,
    pub public_key: String,
    pub organization_id: String,
    pub integration_id: String,
    pub device_id: String,
    pub source_ids: BTreeMap<String, String>,
    pub organization_trusted: bool,
    pub created_at: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateDataSourceRequest {
    #[serde(default)]
    pub id: Option<String>,
    pub integration_id: String,
    pub device_id: String,
    pub name: String,
    pub protocol: String,
    pub status: String,
    pub live_url: String,
    #[serde(default)]
    pub topic_ids: Vec<String>,
    #[serde(default)]
    pub last_data_at: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Project {
    pub id: String,
    pub organization_id: String,
    pub name: String,
    pub description: String,
    pub status: String,
    pub device_count: usize,
    pub online_device_count: usize,
    pub created_at: String,
    pub resource_version: u64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateProjectRequest {
    pub organization_id: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default = "default_active")]
    pub status: String,
    #[serde(default)]
    pub device_ids: Vec<String>,
    #[serde(default)]
    pub data_source_ids: Vec<String>,
}

fn default_active() -> String {
    "active".to_owned()
}

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum AccessMode {
    #[default]
    Control,
    Observe,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceAssignment {
    pub id: String,
    pub project_id: String,
    pub device_id: String,
    pub access_mode: AccessMode,
    pub valid_from: String,
    pub valid_to: Option<String>,
    pub resource_version: u64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateDeviceAssignmentRequest {
    pub device_id: String,
    #[serde(default)]
    pub access_mode: AccessMode,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum DataVisibility {
    #[default]
    Operator,
    Analyst,
    Restricted,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DataAssignment {
    pub id: String,
    pub project_id: String,
    pub data_source_id: String,
    pub visibility: DataVisibility,
    pub valid_from: String,
    pub valid_to: Option<String>,
    pub resource_version: u64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateDataAssignmentRequest {
    pub data_source_id: String,
    #[serde(default)]
    pub visibility: DataVisibility,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Topic {
    pub id: String,
    pub data_source_id: String,
    pub device_id: String,
    pub path: String,
    pub label: String,
    pub renderer: String,
    pub quality: String,
    pub value: Option<String>,
    pub unit: Option<String>,
    pub message: Option<String>,
    pub samples: Option<Vec<f64>>,
    pub updated_at: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordingProjectSnapshot {
    pub project_id: String,
    pub project_name: String,
    pub captured_at: String,
    pub device_assignment_id: String,
    pub data_assignment_id: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TimelineDescriptor {
    pub name: String,
    pub kind: String,
    /// Lossless native Rerun time value: sequence tick, duration ns, or Unix timestamp ns.
    pub start: String,
    /// Lossless native Rerun time value: sequence tick, duration ns, or Unix timestamp ns.
    pub end: String,
    pub duration_seconds: Option<f64>,
    pub fps: Option<f64>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Recording {
    pub id: String,
    pub organization_id: String,
    pub project_id: String,
    pub device_id: String,
    pub data_source_id: String,
    pub name: String,
    pub status: String,
    pub rrd_url: String,
    pub captured_at: String,
    pub duration_label: String,
    pub timelines: Vec<TimelineDescriptor>,
    pub default_timeline: String,
    pub duration_seconds: f64,
    pub rrd_version: String,
    pub footer_verified: bool,
    pub content_sha256: String,
    pub topic_ids: Vec<String>,
    pub mapping_version: u64,
    pub project_snapshot: RecordingProjectSnapshot,
    pub resource_version: u64,
    /// Kept by the backend to make fixture immutability observable in tests.
    pub manifest_hash: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectWorkspace {
    pub snapshot_version: u64,
    pub captured_at: String,
    pub project: Project,
    pub device_assignments: Vec<DeviceAssignment>,
    pub data_assignments: Vec<DataAssignment>,
    pub devices: Vec<Device>,
    pub data_sources: Vec<DataSource>,
    pub recordings: Vec<Recording>,
    /// Immutable topic descriptor snapshots keyed by Recording ID.
    ///
    /// Replay clients must use this map instead of the mutable live `topicsByDataSource`
    /// projection so a later import cannot change an older recording's layout.
    pub topics_by_recording: BTreeMap<String, Vec<Topic>>,
    pub topics_by_data_source: BTreeMap<String, Vec<Topic>>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LiveSession {
    pub id: String,
    pub project_id: String,
    pub device_id: String,
    pub data_source_id: String,
    pub opened_by: String,
    pub status: String,
    pub play_state: String,
    pub source_health: String,
    pub stream_url: String,
    pub started_at: String,
    pub closed_at: Option<String>,
    pub resource_version: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlaybackCursor {
    pub kind: String,
    /// Lossless native Rerun time value: sequence tick, duration ns, or Unix timestamp ns.
    pub value: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReplayLoop {
    pub mode: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub start: Option<PlaybackCursor>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub end: Option<PlaybackCursor>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateLiveSessionRequest {
    pub project_id: String,
    pub device_id: String,
    pub data_source_id: String,
    pub opened_by: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReplaySession {
    pub id: String,
    pub project_id: String,
    pub recording_id: String,
    pub device_id: String,
    pub opened_by: String,
    pub status: String,
    pub stream_url: String,
    /// Deprecated compatibility projection. Use `initialCursor` for lossless seek state.
    pub cursor_seconds: f64,
    pub initial_timeline: String,
    pub initial_cursor: PlaybackCursor,
    pub initial_play_state: String,
    pub initial_speed: f64,
    pub initial_loop: ReplayLoop,
    pub opened_at: String,
    pub closed_at: Option<String>,
    pub resource_version: u64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateReplaySessionRequest {
    pub project_id: String,
    pub recording_id: String,
    pub opened_by: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordingImport {
    pub id: String,
    pub project_id: String,
    pub device_id: String,
    pub data_source_id: String,
    pub file_name: String,
    pub format: String,
    pub status: String,
    pub progress_percent: u8,
    pub size_bytes: u64,
    pub source_sha256: Option<String>,
    pub artifact_url: Option<String>,
    pub recording_id: Option<String>,
    pub failure_reason: Option<String>,
    pub warnings: Vec<String>,
    pub created_at: String,
    pub updated_at: String,
    pub resource_version: u64,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DiscoverySessionStatus {
    Searching,
    Ready,
    Cancelled,
    Failed,
    Expired,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NetworkDiscoverySession {
    pub id: String,
    pub status: DiscoverySessionStatus,
    pub candidate_count: usize,
    pub started_at: String,
    pub expires_at: String,
    pub resource_version: u64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StartNetworkDiscoveryRequest {
    pub organization_id: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DiscoveryCandidateStatus {
    Found,
    Verifying,
    Verified,
    NeedsAttention,
    Unavailable,
    AlreadyLinked,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DiscoveryCandidateCategory {
    Robot,
    Drone,
    Vehicle,
    Camera,
    Gateway,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiscoveryCandidate {
    pub id: String,
    pub session_id: String,
    pub display_name: String,
    pub category: DiscoveryCandidateCategory,
    pub status: DiscoveryCandidateStatus,
    pub last_seen_at: String,
    pub source_count: usize,
    pub supports_live: bool,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NetworkDiscoverySnapshot {
    pub session: NetworkDiscoverySession,
    pub candidates: Vec<DiscoveryCandidate>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CandidateVerificationStatus {
    Verified,
    NeedsCredentials,
    Incompatible,
    Unavailable,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DiscoverySourceCategory {
    Camera,
    Spatial,
    Telemetry,
    State,
    Log,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DiscoverySourceStatus {
    Ready,
    Unavailable,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SuggestedDevice {
    pub name: String,
    pub kind: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VerifiedDiscoverySource {
    pub id: String,
    pub label: String,
    pub category: DiscoverySourceCategory,
    pub status: DiscoverySourceStatus,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CandidateVerification {
    pub verification_token: String,
    pub candidate_id: String,
    pub status: CandidateVerificationStatus,
    pub suggested_device: SuggestedDevice,
    pub sources: Vec<VerifiedDiscoverySource>,
    pub expires_at: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApproveNetworkCandidateInput {
    pub verification_token: String,
    pub project_id: String,
    pub expected_workspace_version: u64,
    pub device_name: String,
    pub selected_source_ids: Vec<String>,
    pub access_mode: AccessMode,
    pub visibility: DataVisibility,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NetworkLinkReceipt {
    pub status: String,
    pub project_id: String,
    pub integration_id: String,
    pub device_id: String,
    pub data_source_ids: Vec<String>,
    pub workspace_version: u64,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ControlLease {
    pub id: String,
    pub live_session_id: String,
    pub device_id: String,
    pub holder_id: String,
    pub holder_name: String,
    pub expires_at: String,
    pub epoch: u64,
    #[serde(skip)]
    pub expires_at_ms: i64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RequestControlLease {
    #[serde(default)]
    pub scope: Option<String>,
    pub expected_device_version: u64,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CommandReceipt {
    pub command_id: String,
    pub live_session_id: String,
    pub command_type: String,
    pub status: String,
    pub message: String,
    pub created_at: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SendCommandRequest {
    #[serde(default)]
    pub live_session_id: Option<String>,
    pub device_id: String,
    pub command_type: String,
    pub expected_device_version: u64,
    pub idempotency_key: String,
    pub lease_id: String,
    pub lease_epoch: u64,
    #[serde(default)]
    pub session_mode: Option<String>,
    pub issued_at: String,
    pub expires_at: String,
}
