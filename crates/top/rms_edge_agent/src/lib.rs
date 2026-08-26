//! RMS edge agent runtime.
//!
//! The agent discovers protocol-native devices without granting control, publishes a signed
//! local-network identity, and exposes an authenticated loopback administration endpoint.

pub mod adapter;
pub mod advertise;
pub mod config;
pub mod health;
pub mod identity;
pub mod service;

pub use adapter::{
    Adapter, AdapterBatch, AdapterDescriptor, AdapterError, AdapterEvent, AdapterKind,
    AdapterObservation, AdapterPollContext, AdapterSnapshot, AdapterState, ObservationTrust,
    ObservedSource, ObservedTopic, QosDurability, QosReliability, RendererHint, SourceCategory,
    SourceStatus, TopicQos,
};
pub use config::{AgentConfig, ConfigError, ConfigStore};
pub use identity::{DeviceIdentity, IdentityError, PublicIdentity};
pub use service::{EdgeAgent, EdgeAgentError};
