//! Protocol adapter contract and bounded supervisor-facing data types.

use std::{collections::BTreeMap, fmt, sync::Arc, time::Duration};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

#[path = "adapters/mavlink.rs"]
pub mod mavlink;
#[path = "adapters/ros2.rs"]
pub mod ros2;

/// A protocol family implemented by an edge adapter.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AdapterKind {
    /// ROS 2 graph discovery backed by DDS.
    Ros2Dds,
    /// Passive `MAVLink` heartbeat discovery.
    Mavlink,
}

impl AdapterKind {
    /// Stable wire name used in configuration and discovery capabilities.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ros2Dds => "ros2_dds",
            Self::Mavlink => "mavlink",
        }
    }
}

/// Static, non-secret adapter metadata.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AdapterDescriptor {
    /// Configuration-stable adapter identifier.
    pub id: String,
    /// Protocol family.
    pub kind: AdapterKind,
    /// Short operator-facing name.
    pub display_name: String,
}

/// Whether an observation was merely seen or cryptographically authenticated.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ObservationTrust {
    /// Network metadata was observed but did not carry verifiable authentication.
    Observed,
    /// Protocol-native authentication was successfully verified.
    Authenticated,
}

/// One bounded, sanitized device observation produced by an adapter.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AdapterObservation {
    /// Protocol-scoped stable identity, such as a ROS enclave/GID or `MAVLink` sysid/compid pair.
    pub identity: String,
    /// Safe operator-facing label.
    pub display_name: String,
    /// RMS device category (`robot`, `drone`, `vehicle`, `camera`, or `gateway`).
    pub device_kind: String,
    /// Trust established by the protocol adapter.
    pub trust: ObservationTrust,
    /// Milliseconds since the Unix epoch at which the observation was received.
    pub observed_at_ms: i64,
    /// Milliseconds since the Unix epoch after which the observation is stale.
    pub expires_at_ms: i64,
    /// Bounded data-source and topic inventory suitable for RMS materialization.
    pub sources: Vec<ObservedSource>,
    /// Bounded, sanitized metadata. Values must not contain credentials or raw payloads.
    pub metadata: BTreeMap<String, String>,
}

impl AdapterObservation {
    /// Validate caps shared by every protocol adapter.
    pub fn validate(&self) -> Result<(), AdapterError> {
        if !valid_wire_token(&self.identity, 192) {
            return Err(AdapterError::invalid_data("invalid observation identity"));
        }
        if self.display_name.is_empty()
            || self.display_name.len() > 96
            || self.display_name.chars().any(char::is_control)
        {
            return Err(AdapterError::invalid_data(
                "invalid observation display name",
            ));
        }
        if !matches!(
            self.device_kind.as_str(),
            "robot" | "drone" | "vehicle" | "camera" | "gateway"
        ) {
            return Err(AdapterError::invalid_data("unsupported device kind"));
        }
        if self.expires_at_ms <= self.observed_at_ms {
            return Err(AdapterError::invalid_data("invalid observation freshness"));
        }
        if self.metadata.len() > 32
            || self.metadata.iter().any(|(key, value)| {
                !valid_wire_token(key, 48)
                    || value.len() > 256
                    || value.chars().any(char::is_control)
            })
        {
            return Err(AdapterError::invalid_data(
                "observation metadata exceeds limits",
            ));
        }
        if self.sources.is_empty() || self.sources.len() > 16 {
            return Err(AdapterError::invalid_data(
                "observation source count is invalid",
            ));
        }
        for source in &self.sources {
            source.validate()?;
        }
        Ok(())
    }
}

/// Data source found on a protocol-native device.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ObservedSource {
    /// Device-local stable source identifier.
    pub id: String,
    /// Operator-facing label.
    pub label: String,
    /// Semantic source category.
    pub category: SourceCategory,
    /// Wire protocol (`ros2` or `mavlink`).
    pub protocol: String,
    /// Whether this edge agent can provide a consumable stream or only metadata.
    pub status: SourceStatus,
    /// Optional default viewer hint.
    pub renderer_hint: Option<RendererHint>,
    /// Bounded topic inventory.
    pub topics: Vec<ObservedTopic>,
}

impl ObservedSource {
    fn validate(&self) -> Result<(), AdapterError> {
        if !valid_wire_token(&self.id, 96)
            || self.label.is_empty()
            || self.label.len() > 96
            || self.label.chars().any(char::is_control)
            || !matches!(self.protocol.as_str(), "ros2" | "mavlink")
            || self.topics.len() > 256
        {
            return Err(AdapterError::invalid_data("invalid observed source"));
        }
        for topic in &self.topics {
            topic.validate()?;
        }
        Ok(())
    }
}

/// RMS-compatible source category.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceCategory {
    /// Camera frames.
    Camera,
    /// Point clouds, transforms, and other spatial data.
    Spatial,
    /// Numeric telemetry.
    Telemetry,
    /// Discrete system state.
    State,
    /// Text or structured logs.
    Log,
}

/// Source readiness exposed to the control plane.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceStatus {
    /// Inventory only; RMS must not claim a live stream exists.
    MetadataOnly,
    /// A separately configured and verified data-plane stream is available.
    Ready,
}

/// Minimal renderer choice suitable for the topic-first viewer.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RendererHint {
    /// Image/camera view.
    Image,
    /// Point-cloud view.
    PointCloud,
    /// General 2D/3D spatial view.
    Spatial,
    /// Three-dimensional transform-tree view.
    Transform3d,
    /// Numeric time-series plot.
    Plot,
    /// Compact state panel.
    State,
    /// Log list.
    Log,
    /// No specialized renderer.
    Raw,
}

/// One discovered ROS 2 topic or normalized `MAVLink` telemetry stream.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ObservedTopic {
    /// Protocol-native path/name.
    pub path: String,
    /// Safe operator-facing label.
    pub label: String,
    /// Protocol-native type, when known.
    pub message_type: Option<String>,
    /// ROS 2 `QoS` summary, when applicable.
    pub qos: Option<TopicQos>,
    /// Suggested topic renderer.
    pub renderer_hint: Option<RendererHint>,
}

impl ObservedTopic {
    fn validate(&self) -> Result<(), AdapterError> {
        if self.path.is_empty()
            || self.path.len() > 256
            || self.path.chars().any(char::is_control)
            || self.label.is_empty()
            || self.label.len() > 96
            || self.label.chars().any(char::is_control)
            || self.message_type.as_ref().is_some_and(|value| {
                value.is_empty() || value.len() > 192 || value.chars().any(char::is_control)
            })
            || self
                .qos
                .as_ref()
                .and_then(|qos| qos.history_depth)
                .is_some_and(|depth| depth > 100_000)
        {
            return Err(AdapterError::invalid_data("invalid observed topic"));
        }
        Ok(())
    }
}

/// Bounded ROS 2 `QoS` summary.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TopicQos {
    /// Reliability policy.
    pub reliability: QosReliability,
    /// Durability policy.
    pub durability: QosDurability,
    /// History depth, if known and finite.
    pub history_depth: Option<u32>,
}

/// ROS 2 reliability policy.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum QosReliability {
    /// Reliable delivery.
    Reliable,
    /// Best-effort delivery.
    BestEffort,
    /// Mixed or unknown policy.
    Unknown,
}

/// ROS 2 durability policy.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum QosDurability {
    /// Volatile samples.
    Volatile,
    /// Transient-local samples.
    TransientLocal,
    /// Mixed or unknown policy.
    Unknown,
}

fn valid_wire_token(value: &str, maximum: usize) -> bool {
    !value.is_empty()
        && value.len() <= maximum
        && value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.' | b'/' | b':')
        })
}

/// Result of one adapter polling cycle.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct AdapterBatch {
    /// Sanitized observations, bounded by [`AdapterPollContext::max_observations`].
    pub observations: Vec<AdapterObservation>,
}

/// A single bounded polling request issued by the supervisor.
#[derive(Clone, Debug)]
pub struct AdapterPollContext {
    /// Cooperative shutdown/cancellation token.
    pub cancellation: CancellationToken,
    /// Hard deadline enforced independently by the supervisor.
    pub deadline: Instant,
    /// Maximum observations the adapter may return.
    pub max_observations: usize,
}

impl AdapterPollContext {
    /// Remaining wall-clock budget, saturating at zero.
    pub fn remaining(&self) -> Duration {
        self.deadline.saturating_duration_since(Instant::now())
    }
}

/// Adapter error classification used by restart and health policy.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AdapterErrorKind {
    /// Configuration cannot work without operator action.
    Configuration,
    /// Input from the network or child probe was malformed.
    InvalidData,
    /// A transient I/O or timeout failure.
    Transient,
    /// The adapter was cancelled during shutdown.
    Cancelled,
}

/// Sanitized adapter failure. Never include credentials or raw packets in `message`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AdapterError {
    kind: AdapterErrorKind,
    message: Arc<str>,
}

impl AdapterError {
    /// Construct a configuration error.
    pub fn configuration(message: impl Into<Arc<str>>) -> Self {
        Self::new(AdapterErrorKind::Configuration, message)
    }

    /// Construct an invalid-data error.
    pub fn invalid_data(message: impl Into<Arc<str>>) -> Self {
        Self::new(AdapterErrorKind::InvalidData, message)
    }

    /// Construct a retryable transient error.
    pub fn transient(message: impl Into<Arc<str>>) -> Self {
        Self::new(AdapterErrorKind::Transient, message)
    }

    /// Construct a cancellation result.
    pub fn cancelled() -> Self {
        Self::new(AdapterErrorKind::Cancelled, "adapter cancelled")
    }

    fn new(kind: AdapterErrorKind, message: impl Into<Arc<str>>) -> Self {
        let mut message = message.into().to_string();
        message.truncate(256);
        Self {
            kind,
            message: Arc::from(message),
        }
    }

    /// Error category.
    pub const fn kind(&self) -> AdapterErrorKind {
        self.kind
    }
}

impl fmt::Display for AdapterError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for AdapterError {}

/// Pluggable protocol discovery implementation.
#[async_trait]
pub trait Adapter: Send + Sync + 'static {
    /// Static metadata used by the supervisor and signed advertisement.
    fn descriptor(&self) -> AdapterDescriptor;

    /// Run one bounded discovery poll.
    ///
    /// Implementations must honor cancellation, return before the deadline, never send device
    /// commands, and enforce `max_observations`. The supervisor also applies an outer timeout.
    async fn poll(&self, context: AdapterPollContext) -> Result<AdapterBatch, AdapterError>;
}

/// Supervisor-visible lifecycle state.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AdapterState {
    /// Adapter has not completed its first poll.
    Starting,
    /// Recent polls succeeded.
    Healthy,
    /// Recent failures are being retried.
    Degraded,
    /// No successful fresh poll has completed within policy.
    Stale,
    /// Agent shutdown completed.
    Stopped,
}

/// Current bounded adapter status returned by the administration API.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AdapterSnapshot {
    /// Adapter descriptor.
    pub descriptor: AdapterDescriptor,
    /// Current lifecycle state.
    pub state: AdapterState,
    /// Most recent observations, capped by supervisor policy.
    pub observations: Vec<AdapterObservation>,
    /// Most recent successful poll time in Unix milliseconds.
    pub last_success_at_ms: Option<i64>,
    /// Consecutive failed polls.
    pub consecutive_failures: u32,
    /// Sanitized last failure, when any.
    pub last_error: Option<String>,
}

/// Bounded event sent to in-process consumers.
#[derive(Clone, Debug)]
pub enum AdapterEvent {
    /// A snapshot changed after a poll or health transition.
    Snapshot(AdapterSnapshot),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn observation_limits_reject_secret_sized_metadata() {
        let mut metadata = BTreeMap::new();
        metadata.insert("payload".to_owned(), "x".repeat(257));
        let observation = AdapterObservation {
            identity: "mavlink:1:1".to_owned(),
            display_name: "Autopilot".to_owned(),
            device_kind: "drone".to_owned(),
            trust: ObservationTrust::Observed,
            observed_at_ms: 1,
            expires_at_ms: 2,
            sources: vec![ObservedSource {
                id: "telemetry".to_owned(),
                label: "Telemetry".to_owned(),
                category: SourceCategory::Telemetry,
                protocol: "mavlink".to_owned(),
                status: SourceStatus::MetadataOnly,
                renderer_hint: Some(RendererHint::Plot),
                topics: Vec::new(),
            }],
            metadata,
        };
        assert!(observation.validate().is_err());
    }
}
