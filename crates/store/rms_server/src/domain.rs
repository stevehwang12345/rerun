use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize)]
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

#[derive(Clone, Debug, Serialize)]
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

#[derive(Clone, Debug, Serialize)]
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

#[derive(Clone, Debug, Serialize)]
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

#[derive(Clone, Debug, Serialize)]
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

#[derive(Clone, Debug, Serialize)]
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

#[derive(Clone, Debug, Serialize)]
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

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordingProjectSnapshot {
    pub project_id: String,
    pub project_name: String,
    pub captured_at: String,
    pub device_assignment_id: String,
    pub data_assignment_id: String,
}

#[derive(Clone, Debug, Serialize)]
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
    pub cursor_seconds: f64,
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
