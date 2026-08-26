//! Agent health aggregation and Prometheus metrics.

use std::fmt;

use prometheus_client::{
    encoding::text::encode,
    metrics::{counter::Counter, gauge::Gauge},
    registry::Registry,
};
use serde::Serialize;

use crate::adapter::{AdapterSnapshot, AdapterState};

/// Aggregate service health.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentHealth {
    /// Every enabled adapter has fresh successful state.
    Healthy,
    /// At least one adapter is starting, retrying, or stale.
    Degraded,
    /// No enabled adapter is usable, or the service is shutting down.
    Stale,
}

/// Compute aggregate health without hiding partial adapter failures.
pub fn aggregate_health(snapshots: &[AdapterSnapshot], shutting_down: bool) -> AgentHealth {
    if shutting_down {
        return AgentHealth::Stale;
    }
    if snapshots.is_empty() {
        return AgentHealth::Degraded;
    }
    if snapshots
        .iter()
        .all(|snapshot| snapshot.state == AdapterState::Healthy)
    {
        return AgentHealth::Healthy;
    }
    if snapshots
        .iter()
        .all(|snapshot| matches!(snapshot.state, AdapterState::Stale | AdapterState::Stopped))
    {
        return AgentHealth::Stale;
    }
    AgentHealth::Degraded
}

/// Bounded process metrics exported in Prometheus text format.
#[derive(Debug)]
pub struct AgentMetrics {
    registry: Registry,
    adapter_polls: Counter,
    adapter_failures: Counter,
    observations: Counter,
    advertisement_responses: Counter,
    heartbeat_successes: Counter,
    heartbeat_failures: Counter,
    healthy_adapters: Gauge,
    control_plane_connected: Gauge,
}

impl Default for AgentMetrics {
    fn default() -> Self {
        let mut registry = Registry::default();
        let adapter_polls = Counter::default();
        let adapter_failures = Counter::default();
        let observations = Counter::default();
        let advertisement_responses = Counter::default();
        let heartbeat_successes = Counter::default();
        let heartbeat_failures = Counter::default();
        let healthy_adapters = Gauge::default();
        let control_plane_connected = Gauge::default();
        registry.register(
            "rms_edge_adapter_polls",
            "Completed adapter poll attempts.",
            adapter_polls.clone(),
        );
        registry.register(
            "rms_edge_adapter_failures",
            "Failed adapter poll attempts.",
            adapter_failures.clone(),
        );
        registry.register(
            "rms_edge_observations",
            "Validated protocol observations.",
            observations.clone(),
        );
        registry.register(
            "rms_edge_advertisement_responses",
            "Bounded mDNS responses emitted.",
            advertisement_responses.clone(),
        );
        registry.register(
            "rms_edge_heartbeat_successes",
            "Accepted control-plane heartbeats.",
            heartbeat_successes.clone(),
        );
        registry.register(
            "rms_edge_heartbeat_failures",
            "Failed control-plane heartbeat attempts.",
            heartbeat_failures.clone(),
        );
        registry.register(
            "rms_edge_healthy_adapters",
            "Number of adapters with fresh successful state.",
            healthy_adapters.clone(),
        );
        registry.register(
            "rms_edge_control_plane_connected",
            "Whether the latest control-plane heartbeat was accepted.",
            control_plane_connected.clone(),
        );
        Self {
            registry,
            adapter_polls,
            adapter_failures,
            observations,
            advertisement_responses,
            heartbeat_successes,
            heartbeat_failures,
            healthy_adapters,
            control_plane_connected,
        }
    }
}

impl AgentMetrics {
    pub(crate) fn adapter_poll(&self, observations: usize) {
        self.adapter_polls.inc();
        self.observations
            .inc_by(u64::try_from(observations).unwrap_or(u64::MAX));
    }

    pub(crate) fn adapter_failure(&self) {
        self.adapter_polls.inc();
        self.adapter_failures.inc();
    }

    pub(crate) fn advertisement_response(&self) {
        self.advertisement_responses.inc();
    }

    pub(crate) fn heartbeat_success(&self) {
        self.heartbeat_successes.inc();
        self.control_plane_connected.set(1);
    }

    pub(crate) fn heartbeat_failure(&self) {
        self.heartbeat_failures.inc();
        self.control_plane_connected.set(0);
    }

    pub(crate) fn set_healthy_adapters(&self, count: usize) {
        self.healthy_adapters
            .set(i64::try_from(count).unwrap_or(i64::MAX));
    }

    /// Encode metrics using the Prometheus/OpenMetrics text exposition format.
    pub fn encode(&self) -> Result<String, MetricsError> {
        let mut output = String::new();
        encode(&mut output, &self.registry).map_err(MetricsError)?;
        Ok(output)
    }
}

/// Metrics encoding error.
#[derive(Debug)]
pub struct MetricsError(fmt::Error);

impl fmt::Display for MetricsError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "metrics encoding failed: {}", self.0)
    }
}

impl std::error::Error for MetricsError {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::{AdapterDescriptor, AdapterKind};

    fn snapshot(state: AdapterState) -> AdapterSnapshot {
        AdapterSnapshot {
            descriptor: AdapterDescriptor {
                id: "adapter".to_owned(),
                kind: AdapterKind::Mavlink,
                display_name: "MAVLink".to_owned(),
            },
            state,
            observations: Vec::new(),
            last_success_at_ms: None,
            consecutive_failures: 0,
            last_error: None,
        }
    }

    #[test]
    fn metrics_use_stable_names() {
        let metrics = AgentMetrics::default();
        metrics.adapter_poll(3);
        let encoded = metrics.encode().expect("metrics encode");
        assert!(encoded.contains("rms_edge_adapter_polls_total 1"));
        assert!(encoded.contains("rms_edge_observations_total 3"));
    }

    #[test]
    fn no_fresh_adapter_is_stale_and_not_ready() {
        assert_eq!(
            aggregate_health(
                &[
                    snapshot(AdapterState::Stale),
                    snapshot(AdapterState::Stopped),
                ],
                false,
            ),
            AgentHealth::Stale
        );
        assert_eq!(
            aggregate_health(
                &[
                    snapshot(AdapterState::Healthy),
                    snapshot(AdapterState::Stale),
                ],
                false,
            ),
            AgentHealth::Degraded
        );
    }
}
