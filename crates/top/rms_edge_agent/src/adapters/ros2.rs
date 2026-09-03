//! Observation-only ROS 2/DDS graph adapter.
//!
//! ROS 2 is deliberately kept outside of the long-running edge-agent process. Each poll starts
//! the embedded, fixed Python probe with a restricted environment, consumes one bounded NDJSON
//! record, and terminates the entire process group on cancellation or timeout. The probe only
//! uses rclpy graph introspection and never creates data subscriptions or command publishers.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::{OsStr, OsString};
use std::fs::{File, OpenOptions};
use std::io::{Read as _, Seek as _, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use serde::Deserialize;

use crate::config::AdapterConfig;

use super::{
    Adapter, AdapterBatch, AdapterDescriptor, AdapterError, AdapterKind, AdapterObservation,
    AdapterPollContext, ObservationTrust, ObservedSource, ObservedTopic, QosDurability,
    QosReliability, RendererHint, SourceCategory, SourceStatus, TopicQos,
};

const PROBE_SOURCE: &str = include_str!("../../../../../tools/ros2_graph_probe.py");
const GRAPH_SCHEMA: &str = "rms.ros2.graph.v1";
const ERROR_SCHEMA: &str = "rms.ros2.error.v1";
const MAX_STDOUT_BYTES: u64 = 1024 * 1024;
const MAX_STDERR_BYTES: u64 = 16 * 1024;
const MAX_NODES: usize = 512;
const MAX_TOPICS: usize = 2048;
const MAX_ENDPOINTS: usize = 4096;
const MAX_TYPES_PER_TOPIC: usize = 16;
const MAX_PREFIXES: usize = 64;
const MAX_STRING_BYTES: usize = 512;
const PROCESS_POLL_INTERVAL: Duration = Duration::from_millis(10);
const MAX_PROCESS_RUNTIME: Duration = Duration::from_secs(10);

/// Deployment settings for one ROS 2 graph participant.
///
/// The Python executable and probe program are not configurable here. This prevents remote or
/// ordinary adapter configuration from selecting arbitrary executable code.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Ros2Settings {
    /// Provisioned, restart-stable device identifier.
    pub device_id: String,
    /// Short label shown to an operator.
    pub display_name: String,
    /// RMS device category.
    #[serde(default = "default_device_kind")]
    pub device_kind: String,
    /// DDS domain to inspect.
    #[serde(default)]
    pub domain_id: u16,
    /// Namespace of the temporary graph participant.
    #[serde(default = "default_probe_namespace")]
    pub probe_namespace: String,
    /// Optional inclusive ROS topic-prefix filters.
    #[serde(default)]
    pub allow_topic_prefixes: Vec<String>,
    /// ROS topic prefixes which must never be included.
    #[serde(default)]
    pub deny_topic_prefixes: Vec<String>,
    /// Time allowed for DDS discovery to settle during each poll.
    #[serde(default = "default_settle_ms")]
    pub settle_ms: u64,
    /// Freshness window applied to a successful graph observation.
    #[serde(default = "default_stale_after_ms")]
    pub stale_after_ms: i64,
    /// Optional explicitly selected, allowlisted RMW implementation.
    #[serde(default)]
    pub rmw_implementation: Option<String>,
    /// ROS automatic discovery scope.
    #[serde(default)]
    pub discovery_range: Ros2DiscoveryRange,
    /// Optional SROS2/DDS-Security identity for the probe participant.
    #[serde(default)]
    pub security: Option<Ros2SecuritySettings>,
}

/// Network scope supplied to `ROS_AUTOMATIC_DISCOVERY_RANGE`.
#[derive(Clone, Copy, Debug, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Ros2DiscoveryRange {
    /// Discover participants reachable through subnet multicast.
    #[default]
    Subnet,
    /// Inspect only participants on the same host.
    Localhost,
    /// Disable automatic discovery. Useful with middleware-managed static discovery.
    Off,
}

impl Ros2DiscoveryRange {
    const fn environment_value(self) -> &'static str {
        match self {
            Self::Subnet => "SUBNET",
            Self::Localhost => "LOCALHOST",
            Self::Off => "OFF",
        }
    }
}

/// SROS2 security identity loaded by rcl/rmw for the probe participant.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Ros2SecuritySettings {
    /// Root of a provisioned SROS2 keystore. This must be an existing absolute directory.
    pub keystore: PathBuf,
    /// Fully-qualified enclave path whose permissions must allow graph discovery.
    pub enclave: String,
    /// Whether missing/invalid security artifacts fail closed.
    #[serde(default)]
    pub strategy: Ros2SecurityStrategy,
}

/// ROS security failure policy.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Ros2SecurityStrategy {
    /// Fail closed when DDS Security cannot be enabled.
    #[default]
    Enforce,
    /// Development-only mode in which rcl may continue without security.
    Permissive,
}

impl Ros2SecurityStrategy {
    const fn environment_value(self) -> &'static str {
        match self {
            Self::Enforce => "Enforce",
            Self::Permissive => "Permissive",
        }
    }
}

/// Supervised ROS 2 graph discovery adapter.
#[derive(Clone, Debug)]
pub struct Ros2Adapter {
    adapter_id: String,
    settings: Ros2Settings,
    probe_command: ProbeCommand,
}

impl Ros2Adapter {
    /// Deserialize and validate an opaque edge-agent adapter configuration.
    pub fn from_config(config: &AdapterConfig) -> Result<Self, AdapterError> {
        if config.kind != AdapterKind::Ros2Dds {
            return Err(AdapterError::configuration(
                "ROS 2 adapter received the wrong protocol configuration",
            ));
        }
        let settings: Ros2Settings = serde_json::from_value(config.settings.clone())
            .map_err(|_error| AdapterError::configuration("ROS 2 adapter settings are invalid"))?;
        Self::new(config.id.clone(), settings)
    }

    /// Construct an adapter using the managed Python runtime and embedded probe.
    pub fn new(adapter_id: String, settings: Ros2Settings) -> Result<Self, AdapterError> {
        validate_adapter_id(&adapter_id)?;
        validate_settings(&settings)?;
        Ok(Self {
            adapter_id,
            settings,
            probe_command: ProbeCommand::managed(),
        })
    }

    #[cfg(test)]
    fn with_probe_command(
        adapter_id: String,
        settings: Ros2Settings,
        probe_command: ProbeCommand,
    ) -> Result<Self, AdapterError> {
        validate_adapter_id(&adapter_id)?;
        validate_settings(&settings)?;
        Ok(Self {
            adapter_id,
            settings,
            probe_command,
        })
    }
}

#[async_trait]
impl Adapter for Ros2Adapter {
    fn descriptor(&self) -> AdapterDescriptor {
        AdapterDescriptor {
            id: self.adapter_id.clone(),
            kind: AdapterKind::Ros2Dds,
            display_name: "ROS 2 / DDS".to_owned(),
        }
    }

    async fn poll(&self, context: AdapterPollContext) -> Result<AdapterBatch, AdapterError> {
        if context.max_observations == 0 {
            return Ok(AdapterBatch::default());
        }
        if context.cancellation.is_cancelled() {
            return Err(AdapterError::cancelled());
        }

        let settings = self.settings.clone();
        let command = self.probe_command.clone();
        let result = tokio::task::spawn_blocking(move || run_probe(&command, &settings, &context))
            .await
            .map_err(|_error| {
                AdapterError::transient("ROS 2 graph worker stopped unexpectedly")
            })??;
        let observation = graph_to_observation(result, &self.settings)?;
        Ok(AdapterBatch {
            observations: vec![observation],
        })
    }
}

#[derive(Clone, Debug)]
struct ProbeCommand {
    executable: OsString,
    prefix_arguments: Vec<OsString>,
    source: String,
}

impl ProbeCommand {
    fn managed() -> Self {
        #[cfg(windows)]
        let executable = OsString::from("python.exe");
        #[cfg(not(windows))]
        let executable = OsString::from("/usr/bin/python3");
        Self {
            executable,
            prefix_arguments: vec![OsString::from("-B")],
            source: PROBE_SOURCE.to_owned(),
        }
    }

    #[cfg(test)]
    fn python_source(source: impl Into<String>) -> Self {
        let mut command = Self::managed();
        command.source = source.into();
        command
    }
}

struct ProbeOutcome {
    status: ExitStatus,
    stdout: Vec<u8>,
}

fn run_probe(
    probe: &ProbeCommand,
    settings: &Ros2Settings,
    context: &AdapterPollContext,
) -> Result<RawGraph, AdapterError> {
    let script_directory = tempfile::Builder::new()
        .prefix("rms-ros2-graph-")
        .tempdir()
        .map_err(|_error| AdapterError::transient("ROS 2 graph probe could not be prepared"))?;
    let script_path = script_directory.path().join("ros2_graph_probe.py");
    let mut script_file = private_script_file(&script_path)?;
    script_file
        .write_all(probe.source.as_bytes())
        .and_then(|()| script_file.flush())
        .map_err(|_error| AdapterError::transient("ROS 2 graph probe could not be prepared"))?;
    drop(script_file);

    let stdout_file = tempfile::tempfile()
        .map_err(|_error| AdapterError::transient("ROS 2 graph output could not be buffered"))?;
    let stderr_file = tempfile::tempfile()
        .map_err(|_error| AdapterError::transient("ROS 2 graph output could not be buffered"))?;
    let stderr_reader = stderr_file
        .try_clone()
        .map_err(|_error| AdapterError::transient("ROS 2 graph output could not be buffered"))?;
    let stdout_reader = stdout_file
        .try_clone()
        .map_err(|_error| AdapterError::transient("ROS 2 graph output could not be buffered"))?;

    let mut command = Command::new(&probe.executable);
    command
        .args(&probe.prefix_arguments)
        .stdin(Stdio::null())
        .stdout(Stdio::from(stdout_file))
        .stderr(Stdio::from(stderr_file));
    command.arg(&script_path);
    append_probe_arguments(&mut command, settings);
    apply_restricted_environment(&mut command, settings);
    configure_process_group(&mut command);

    let mut child = command.spawn().map_err(|_error| {
        AdapterError::configuration("ROS 2 runtime is unavailable on this edge host")
    })?;
    let runtime_deadline = std::time::Instant::now() + context.remaining().min(MAX_PROCESS_RUNTIME);
    let status = loop {
        if context.cancellation.is_cancelled() {
            terminate_process_tree(&mut child);
            return Err(AdapterError::cancelled());
        }
        if std::time::Instant::now() >= runtime_deadline {
            terminate_process_tree(&mut child);
            return Err(AdapterError::transient("ROS 2 graph probe timed out"));
        }
        if output_exceeds_limit(&stdout_reader, MAX_STDOUT_BYTES)? {
            terminate_process_tree(&mut child);
            return Err(AdapterError::invalid_data(
                "ROS 2 graph probe exceeded its output limit",
            ));
        }
        if output_exceeds_limit(&stderr_reader, MAX_STDERR_BYTES)? {
            terminate_process_tree(&mut child);
            return Err(AdapterError::invalid_data(
                "ROS 2 graph probe exceeded its diagnostic limit",
            ));
        }
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => std::thread::sleep(PROCESS_POLL_INTERVAL),
            Err(_) => {
                terminate_process_tree(&mut child);
                return Err(AdapterError::transient(
                    "ROS 2 graph probe could not be supervised",
                ));
            }
        }
    };

    if output_exceeds_limit(&stdout_reader, MAX_STDOUT_BYTES)? {
        return Err(AdapterError::invalid_data(
            "ROS 2 graph probe exceeded its output limit",
        ));
    }
    if output_exceeds_limit(&stderr_reader, MAX_STDERR_BYTES)? {
        return Err(AdapterError::invalid_data(
            "ROS 2 graph probe exceeded its diagnostic limit",
        ));
    }

    let stdout = read_bounded(stdout_reader, MAX_STDOUT_BYTES)?;
    let outcome = ProbeOutcome { status, stdout };
    parse_probe_outcome(&outcome, settings)
}

fn private_script_file(path: &Path) -> Result<File, AdapterError> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    options
        .open(path)
        .map_err(|_error| AdapterError::transient("ROS 2 graph probe could not be prepared"))
}

fn append_probe_arguments(command: &mut Command, settings: &Ros2Settings) {
    command
        .arg("--device-id")
        .arg(&settings.device_id)
        .arg("--domain-id")
        .arg(settings.domain_id.to_string())
        .arg("--probe-namespace")
        .arg(&settings.probe_namespace)
        .arg("--settle-ms")
        .arg(settings.settle_ms.to_string());
    for prefix in &settings.allow_topic_prefixes {
        command.arg("--allow-prefix").arg(prefix);
    }
    for prefix in &settings.deny_topic_prefixes {
        command.arg("--deny-prefix").arg(prefix);
    }
    if let Some(security) = &settings.security {
        command.arg("--security-enclave").arg(&security.enclave);
    }
}

fn apply_restricted_environment(command: &mut Command, settings: &Ros2Settings) {
    const INHERITED_ENVIRONMENT: &[&str] = &[
        "SystemRoot",
        "WINDIR",
        "PATH",
        "PATHEXT",
        "TEMP",
        "TMP",
        "TMPDIR",
        "LANG",
        "LC_ALL",
        // ROS installations extend these when their managed setup script is sourced.
        "PYTHONPATH",
        "LD_LIBRARY_PATH",
        "DYLD_LIBRARY_PATH",
        "AMENT_PREFIX_PATH",
        "COLCON_PREFIX_PATH",
        "CMAKE_PREFIX_PATH",
        "ROS_DISTRO",
    ];
    let inherited = INHERITED_ENVIRONMENT
        .iter()
        .filter_map(|name| std::env::var_os(name).map(|value| (*name, value)))
        .collect::<Vec<_>>();
    command
        .env_clear()
        .envs(inherited)
        .env("ROS_DOMAIN_ID", settings.domain_id.to_string())
        .env(
            "ROS_AUTOMATIC_DISCOVERY_RANGE",
            settings.discovery_range.environment_value(),
        );
    if let Some(rmw) = &settings.rmw_implementation {
        command.env("RMW_IMPLEMENTATION", rmw);
    }
    if let Some(security) = &settings.security {
        command
            .env("ROS_SECURITY_ENABLE", "true")
            .env(
                "ROS_SECURITY_STRATEGY",
                security.strategy.environment_value(),
            )
            .env("ROS_SECURITY_KEYSTORE", &security.keystore);
    }
}

fn output_exceeds_limit(file: &File, limit: u64) -> Result<bool, AdapterError> {
    file.metadata()
        .map(|metadata| metadata.len() > limit)
        .map_err(|_error| AdapterError::transient("ROS 2 graph output could not be supervised"))
}

#[expect(
    clippy::verbose_file_reads,
    reason = "the child writes through an inherited anonymous file handle, not a stable path"
)]
fn read_bounded(mut file: File, limit: u64) -> Result<Vec<u8>, AdapterError> {
    let size = file
        .metadata()
        .map_err(|_error| AdapterError::transient("ROS 2 graph output could not be read"))?
        .len();
    if size > limit {
        return Err(AdapterError::invalid_data(
            "ROS 2 graph probe exceeded its output limit",
        ));
    }
    file.rewind()
        .map_err(|_error| AdapterError::transient("ROS 2 graph output could not be read"))?;
    let mut output = Vec::with_capacity(size as usize);
    file.read_to_end(&mut output)
        .map_err(|_error| AdapterError::transient("ROS 2 graph output could not be read"))?;
    Ok(output)
}

fn parse_probe_outcome(
    outcome: &ProbeOutcome,
    settings: &Ros2Settings,
) -> Result<RawGraph, AdapterError> {
    let record = parse_single_ndjson_record(&outcome.stdout)?;
    let schema = record
        .get("schema")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| AdapterError::invalid_data("ROS 2 graph record has no schema"))?;
    if schema == ERROR_SCHEMA {
        let error: RawProbeError = serde_json::from_value(record).map_err(|_error| {
            AdapterError::invalid_data("ROS 2 graph error record is malformed")
        })?;
        return Err(match error.code.as_str() {
            "ros_unavailable" | "invalid_configuration" => {
                AdapterError::configuration("ROS 2 graph discovery is not configured")
            }
            "output_limit" => {
                AdapterError::invalid_data("ROS 2 graph exceeded the configured limits")
            }
            _ => AdapterError::transient("ROS 2 graph discovery failed"),
        });
    }
    if schema != GRAPH_SCHEMA || !outcome.status.success() {
        return Err(AdapterError::invalid_data(
            "ROS 2 graph probe returned an invalid result",
        ));
    }
    let graph: RawGraph = serde_json::from_value(record)
        .map_err(|_error| AdapterError::invalid_data("ROS 2 graph record is malformed"))?;
    validate_graph(&graph, settings)?;
    Ok(graph)
}

fn parse_single_ndjson_record(bytes: &[u8]) -> Result<serde_json::Value, AdapterError> {
    if bytes.is_empty() || bytes.len() as u64 > MAX_STDOUT_BYTES || !bytes.ends_with(b"\n") {
        return Err(AdapterError::invalid_data(
            "ROS 2 graph probe did not return one bounded record",
        ));
    }
    let text = std::str::from_utf8(bytes)
        .map_err(|_error| AdapterError::invalid_data("ROS 2 graph record is not UTF-8"))?;
    let mut records = text.lines().filter(|line| !line.is_empty());
    let first = records
        .next()
        .ok_or_else(|| AdapterError::invalid_data("ROS 2 graph record is empty"))?;
    if records.next().is_some() {
        return Err(AdapterError::invalid_data(
            "ROS 2 graph probe returned multiple records",
        ));
    }
    serde_json::from_str(first)
        .map_err(|_error| AdapterError::invalid_data("ROS 2 graph record is malformed JSON"))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RawGraph {
    schema: String,
    captured_at_unix_ms: i64,
    device_id: String,
    domain_id: u16,
    probe_namespace: String,
    rmw_implementation: String,
    security: RawSecurity,
    nodes: Vec<RawNode>,
    topics: Vec<RawTopic>,
    limits_reached: RawLimits,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RawSecurity {
    enabled: bool,
    strategy: String,
    enclave: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RawNode {
    name: String,
    namespace: String,
    #[serde(default)]
    enclave: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RawTopic {
    name: String,
    types: Vec<String>,
    publishers: Vec<RawEndpoint>,
    subscribers: Vec<RawEndpoint>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RawEndpoint {
    node_name: String,
    node_namespace: String,
    topic_type: String,
    qos: RawQos,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RawQos {
    history: String,
    depth: u64,
    reliability: String,
    durability: String,
    liveliness: String,
    #[serde(default)]
    deadline_ns: Option<u64>,
    #[serde(default)]
    lifespan_ns: Option<u64>,
    #[serde(default)]
    liveliness_lease_duration_ns: Option<u64>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RawLimits {
    nodes: bool,
    topics: bool,
    endpoints: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawProbeError {
    #[serde(rename = "schema")]
    _schema: String,
    code: String,
    #[serde(rename = "message")]
    _message: String,
}

fn validate_graph(graph: &RawGraph, settings: &Ros2Settings) -> Result<(), AdapterError> {
    if graph.schema != GRAPH_SCHEMA
        || graph.device_id != settings.device_id
        || graph.domain_id != settings.domain_id
        || graph.probe_namespace != settings.probe_namespace
    {
        return Err(AdapterError::invalid_data(
            "ROS 2 graph identity does not match its request",
        ));
    }
    if graph.nodes.len() > MAX_NODES || graph.topics.len() > MAX_TOPICS {
        return Err(AdapterError::invalid_data("ROS 2 graph exceeds its limits"));
    }
    validate_safe_string(&graph.rmw_implementation)?;
    validate_safe_string(&graph.security.strategy)?;
    if let Some(enclave) = &graph.security.enclave {
        validate_ros_name(enclave)?;
    }
    match &settings.security {
        Some(security)
            if !graph.security.enabled
                || graph.security.strategy != security.strategy.environment_value()
                || graph.security.enclave.as_deref() != Some(security.enclave.as_str()) =>
        {
            return Err(AdapterError::invalid_data(
                "ROS 2 security identity was not applied",
            ));
        }
        None if graph.security.enabled || graph.security.enclave.is_some() => {
            return Err(AdapterError::invalid_data(
                "ROS 2 probe returned an unexpected security identity",
            ));
        }
        Some(_) | None => {}
    }
    let mut endpoint_count = 0_usize;
    for node in &graph.nodes {
        validate_safe_string(&node.name)?;
        validate_ros_name(&node.namespace)?;
        if let Some(enclave) = &node.enclave {
            validate_ros_name(enclave)?;
        }
    }
    for topic in &graph.topics {
        validate_ros_name(&topic.name)?;
        if topic.types.len() > MAX_TYPES_PER_TOPIC
            || !topic_allowed(
                &topic.name,
                &settings.allow_topic_prefixes,
                &settings.deny_topic_prefixes,
            )
        {
            return Err(AdapterError::invalid_data(
                "ROS 2 graph contains a disallowed topic",
            ));
        }
        for topic_type in &topic.types {
            validate_type_name(topic_type)?;
        }
        endpoint_count = endpoint_count
            .checked_add(topic.publishers.len() + topic.subscribers.len())
            .ok_or_else(|| AdapterError::invalid_data("ROS 2 graph endpoint count overflowed"))?;
        for endpoint in topic.publishers.iter().chain(&topic.subscribers) {
            validate_safe_string(&endpoint.node_name)?;
            validate_ros_name(&endpoint.node_namespace)?;
            validate_type_name(&endpoint.topic_type)?;
            validate_qos(&endpoint.qos)?;
        }
    }
    if endpoint_count > MAX_ENDPOINTS {
        return Err(AdapterError::invalid_data(
            "ROS 2 graph endpoint limit was exceeded",
        ));
    }
    if graph.captured_at_unix_ms <= 0 {
        return Err(AdapterError::invalid_data(
            "ROS 2 graph timestamp is invalid",
        ));
    }
    Ok(())
}

fn validate_qos(qos: &RawQos) -> Result<(), AdapterError> {
    for value in [
        &qos.history,
        &qos.reliability,
        &qos.durability,
        &qos.liveliness,
    ] {
        validate_safe_string(value)?;
    }
    if qos.depth > 1_000_000
        || qos.deadline_ns.is_some_and(|value| value > i64::MAX as u64)
        || qos.lifespan_ns.is_some_and(|value| value > i64::MAX as u64)
        || qos
            .liveliness_lease_duration_ns
            .is_some_and(|value| value > i64::MAX as u64)
    {
        return Err(AdapterError::invalid_data(
            "ROS 2 QoS value is out of range",
        ));
    }
    Ok(())
}

fn graph_to_observation(
    graph: RawGraph,
    settings: &Ros2Settings,
) -> Result<AdapterObservation, AdapterError> {
    let now = unix_time_ms()?;
    let publisher_count = graph
        .topics
        .iter()
        .map(|topic| topic.publishers.len())
        .sum::<usize>();
    let subscriber_count = graph
        .topics
        .iter()
        .map(|topic| topic.subscribers.len())
        .sum::<usize>();
    let mut categories = BTreeSet::new();
    let mut renderer_hints = BTreeSet::new();
    for topic in &graph.topics {
        for topic_type in &topic.types {
            classify_topic_type(topic_type, &mut categories, &mut renderer_hints);
        }
    }
    if categories.is_empty() {
        categories.insert("messages");
        renderer_hints.insert("raw");
    }
    let sources = graph_to_sources(&graph);
    let mut metadata = BTreeMap::new();
    metadata.insert("capability".to_owned(), "observation_only".to_owned());
    metadata.insert("domainId".to_owned(), graph.domain_id.to_string());
    metadata.insert("namespace".to_owned(), graph.probe_namespace);
    metadata.insert("rmw".to_owned(), graph.rmw_implementation);
    metadata.insert("nodeCount".to_owned(), graph.nodes.len().to_string());
    metadata.insert("topicCount".to_owned(), graph.topics.len().to_string());
    metadata.insert("publisherCount".to_owned(), publisher_count.to_string());
    metadata.insert("subscriberCount".to_owned(), subscriber_count.to_string());
    metadata.insert(
        "sourceCategories".to_owned(),
        categories.into_iter().collect::<Vec<_>>().join(","),
    );
    metadata.insert(
        "rendererHints".to_owned(),
        renderer_hints.into_iter().collect::<Vec<_>>().join(","),
    );
    metadata.insert(
        "security".to_owned(),
        if graph.security.enabled {
            graph.security.strategy.to_lowercase()
        } else {
            "disabled".to_owned()
        },
    );
    metadata.insert(
        "truncated".to_owned(),
        (graph.limits_reached.nodes
            || graph.limits_reached.topics
            || graph.limits_reached.endpoints)
            .to_string(),
    );
    let trust = if settings
        .security
        .as_ref()
        .is_some_and(|security| security.strategy == Ros2SecurityStrategy::Enforce)
        && graph.security.enabled
        && graph.security.strategy == "Enforce"
    {
        ObservationTrust::Authenticated
    } else {
        ObservationTrust::Observed
    };
    let observation = AdapterObservation {
        identity: format!("ros2:{}", settings.device_id),
        display_name: settings.display_name.clone(),
        device_kind: settings.device_kind.clone(),
        trust,
        observed_at_ms: now,
        expires_at_ms: now.saturating_add(settings.stale_after_ms),
        sources,
        metadata,
    };
    observation.validate()?;
    Ok(observation)
}

fn graph_to_sources(graph: &RawGraph) -> Vec<ObservedSource> {
    let mut grouped: BTreeMap<&'static str, Vec<ObservedTopic>> = BTreeMap::new();
    for topic in &graph.topics {
        let topic_type = topic.types.first().cloned();
        let (group, _, renderer) = topic_type.as_deref().map(classification).unwrap_or((
            "messages",
            SourceCategory::Log,
            RendererHint::Raw,
        ));
        let topics = grouped.entry(group).or_default();
        if topics.len() >= 256 {
            continue;
        }
        topics.push(ObservedTopic {
            path: topic.name.clone(),
            label: topic_label(&topic.name),
            message_type: topic_type,
            qos: aggregate_qos(topic),
            renderer_hint: Some(renderer),
        });
    }
    if grouped.is_empty() {
        grouped.insert("graph", Vec::new());
    }
    grouped
        .into_iter()
        .map(|(group, topics)| {
            let (_, category, renderer) = topics
                .first()
                .and_then(|topic| topic.message_type.as_deref())
                .map(classification)
                .unwrap_or(("graph", SourceCategory::State, RendererHint::State));
            ObservedSource {
                id: format!("ros2:{group}"),
                label: source_label(group).to_owned(),
                category,
                protocol: "ros2".to_owned(),
                // Graph discovery does not imply that an ingest subscription is provisioned.
                status: SourceStatus::MetadataOnly,
                renderer_hint: Some(renderer),
                topics,
            }
        })
        .collect()
}

fn aggregate_qos(topic: &RawTopic) -> Option<TopicQos> {
    let endpoints = topic
        .publishers
        .iter()
        .chain(&topic.subscribers)
        .collect::<Vec<_>>();
    let first = endpoints.first()?;
    let reliability = aggregate_policy(
        &endpoints,
        |endpoint| endpoint.qos.reliability.as_str(),
        |value| match value {
            "reliable" | "reliability_reliable" => QosReliability::Reliable,
            "best_effort" | "besteffort" | "reliability_best_effort" => QosReliability::BestEffort,
            _ => QosReliability::Unknown,
        },
    );
    let durability = aggregate_policy(
        &endpoints,
        |endpoint| endpoint.qos.durability.as_str(),
        |value| match value {
            "volatile" | "durability_volatile" => QosDurability::Volatile,
            "transient_local" | "durability_transient_local" => QosDurability::TransientLocal,
            _ => QosDurability::Unknown,
        },
    );
    let first_depth = first.qos.depth;
    let history_depth = (first_depth <= 100_000
        && endpoints
            .iter()
            .all(|endpoint| endpoint.qos.depth == first_depth))
    .then_some(first_depth as u32);
    Some(TopicQos {
        reliability,
        durability,
        history_depth,
    })
}

fn aggregate_policy<T: Copy + Eq>(
    endpoints: &[&RawEndpoint],
    raw: impl Fn(&RawEndpoint) -> &str,
    parse: impl Fn(&str) -> T,
) -> T {
    let first = parse(raw(endpoints[0]));
    if endpoints
        .iter()
        .skip(1)
        .all(|endpoint| parse(raw(endpoint)) == first)
    {
        first
    } else {
        // Both QoS enums use `Unknown` as their last discriminant, but Rust cannot express that
        // generically. Callers ensure mixed input maps to the unknown value before this helper.
        parse("unknown")
    }
}

fn topic_label(path: &str) -> String {
    path.rsplit('/')
        .find(|part| !part.is_empty())
        .unwrap_or("topic")
        .chars()
        .take(96)
        .collect()
}

fn source_label(group: &str) -> &str {
    match group {
        "camera" => "Camera",
        "spatial" => "Spatial sensors",
        "transform" => "Transforms",
        "trajectory" => "Trajectory",
        "pose" => "Pose",
        "diagnostics" => "Diagnostics",
        "telemetry" => "Telemetry",
        "messages" => "ROS messages",
        _ => "ROS graph",
    }
}

fn classification(topic_type: &str) -> (&'static str, SourceCategory, RendererHint) {
    match topic_type {
        "sensor_msgs/msg/Image" | "sensor_msgs/msg/CompressedImage" => {
            ("camera", SourceCategory::Camera, RendererHint::Image)
        }
        "sensor_msgs/msg/PointCloud2" => {
            ("spatial", SourceCategory::Spatial, RendererHint::PointCloud)
        }
        "tf2_msgs/msg/TFMessage" => (
            "transform",
            SourceCategory::Spatial,
            RendererHint::Transform3d,
        ),
        "sensor_msgs/msg/BatteryState"
        | "sensor_msgs/msg/FluidPressure"
        | "sensor_msgs/msg/Illuminance"
        | "sensor_msgs/msg/Imu"
        | "sensor_msgs/msg/Joy"
        | "sensor_msgs/msg/JointState"
        | "sensor_msgs/msg/Range"
        | "sensor_msgs/msg/RelativeHumidity"
        | "sensor_msgs/msg/Temperature"
        | "std_msgs/msg/Float64Array"
        | "std_msgs/msg/Float64MultiArray" => {
            ("telemetry", SourceCategory::Telemetry, RendererHint::Plot)
        }
        // Everything else stays raw until the MCAP decoder emits an archetype with a matching
        // product renderer. This deliberately includes CameraInfo, LaserScan, Odometry, Path,
        // Pose, and NavSatFix (GeoPoints has no RMS map-renderer contract yet).
        _ => ("messages", SourceCategory::Log, RendererHint::Raw),
    }
}

fn classify_topic_type<'a>(
    topic_type: &str,
    categories: &mut BTreeSet<&'a str>,
    renderer_hints: &mut BTreeSet<&'a str>,
) {
    let (group, _, renderer) = classification(topic_type);
    let renderer = match renderer {
        RendererHint::Image => "image",
        RendererHint::PointCloud => "point_cloud",
        RendererHint::Spatial => "spatial",
        RendererHint::Transform3d => "transform3d",
        RendererHint::Plot => "plot",
        RendererHint::State => "state",
        RendererHint::Log => "log",
        RendererHint::Raw => "raw",
    };
    categories.insert(group);
    renderer_hints.insert(renderer);
}

fn validate_settings(settings: &Ros2Settings) -> Result<(), AdapterError> {
    validate_identifier(&settings.device_id, 128)?;
    validate_display_name(&settings.display_name)?;
    if !matches!(
        settings.device_kind.as_str(),
        "robot" | "drone" | "vehicle" | "camera" | "gateway"
    ) {
        return Err(AdapterError::configuration(
            "ROS 2 adapter has an unsupported device kind",
        ));
    }
    if settings.domain_id > 232 {
        return Err(AdapterError::configuration(
            "ROS 2 domain id must be between 0 and 232",
        ));
    }
    validate_ros_name_config(&settings.probe_namespace)?;
    if settings.allow_topic_prefixes.len() > MAX_PREFIXES
        || settings.deny_topic_prefixes.len() > MAX_PREFIXES
    {
        return Err(AdapterError::configuration(
            "ROS 2 topic filter has too many prefixes",
        ));
    }
    for prefix in settings
        .allow_topic_prefixes
        .iter()
        .chain(&settings.deny_topic_prefixes)
    {
        validate_ros_name_config(prefix)?;
    }
    if !(100..=5_000).contains(&settings.settle_ms) {
        return Err(AdapterError::configuration(
            "ROS 2 discovery settle time must be between 100 and 5000 milliseconds",
        ));
    }
    if !(1_000..=600_000).contains(&settings.stale_after_ms) {
        return Err(AdapterError::configuration(
            "ROS 2 stale time must be between 1 and 600 seconds",
        ));
    }
    if let Some(rmw) = &settings.rmw_implementation
        && !matches!(
            rmw.as_str(),
            "rmw_cyclonedds_cpp" | "rmw_fastrtps_cpp" | "rmw_connextdds"
        )
    {
        return Err(AdapterError::configuration(
            "ROS 2 adapter has an unsupported RMW implementation",
        ));
    }
    if let Some(security) = &settings.security {
        if !security.keystore.is_absolute() || !security.keystore.is_dir() {
            return Err(AdapterError::configuration(
                "ROS 2 security keystore is unavailable",
            ));
        }
        validate_ros_name_config(&security.enclave)?;
    }
    Ok(())
}

fn validate_adapter_id(value: &str) -> Result<(), AdapterError> {
    validate_identifier(value, 96)
        .map_err(|_error| AdapterError::configuration("ROS 2 adapter id is invalid"))
}

fn validate_identifier(value: &str, max_len: usize) -> Result<(), AdapterError> {
    if value.is_empty()
        || value.len() > max_len
        || !value.bytes().enumerate().all(|(index, byte)| {
            byte.is_ascii_alphanumeric() || (index > 0 && b"._:-".contains(&byte))
        })
    {
        return Err(AdapterError::configuration(
            "ROS 2 managed identifier is invalid",
        ));
    }
    Ok(())
}

fn validate_display_name(value: &str) -> Result<(), AdapterError> {
    if value.is_empty()
        || value.len() > 96
        || value.chars().any(char::is_control)
        || value.trim() != value
    {
        return Err(AdapterError::configuration("ROS 2 display name is invalid"));
    }
    Ok(())
}

fn validate_ros_name_config(value: &str) -> Result<(), AdapterError> {
    validate_ros_name(value)
        .map_err(|_error| AdapterError::configuration("ROS 2 name filter is invalid"))
}

fn validate_ros_name(value: &str) -> Result<(), AdapterError> {
    if value.is_empty()
        || value.len() > MAX_STRING_BYTES
        || !value.starts_with('/')
        || value.chars().any(|character| {
            !(character.is_ascii_alphanumeric() || character == '_' || character == '/')
        })
        || value.contains("//")
    {
        return Err(AdapterError::invalid_data(
            "ROS 2 graph contains an invalid name",
        ));
    }
    Ok(())
}

fn validate_type_name(value: &str) -> Result<(), AdapterError> {
    if value.is_empty()
        || value.len() > MAX_STRING_BYTES
        || value.chars().any(|character| {
            !(character.is_ascii_alphanumeric() || character == '_' || character == '/')
        })
    {
        return Err(AdapterError::invalid_data(
            "ROS 2 graph contains an invalid type name",
        ));
    }
    Ok(())
}

fn validate_safe_string(value: &str) -> Result<(), AdapterError> {
    if value.is_empty() || value.len() > MAX_STRING_BYTES || value.chars().any(char::is_control) {
        return Err(AdapterError::invalid_data(
            "ROS 2 graph contains an invalid string",
        ));
    }
    Ok(())
}

fn topic_allowed(name: &str, allow: &[String], deny: &[String]) -> bool {
    (allow.is_empty() || allow.iter().any(|prefix| prefix_matches(name, prefix)))
        && !deny.iter().any(|prefix| prefix_matches(name, prefix))
}

fn prefix_matches(name: &str, prefix: &str) -> bool {
    prefix == "/"
        || name == prefix
        || name
            .strip_prefix(prefix)
            .is_some_and(|remainder| remainder.starts_with('/'))
}

fn unix_time_ms() -> Result<i64, AdapterError> {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_error| AdapterError::transient("edge host clock is invalid"))?
        .as_millis();
    i64::try_from(millis).map_err(|_error| AdapterError::transient("edge host clock is invalid"))
}

#[cfg(unix)]
fn configure_process_group(command: &mut Command) {
    use std::os::unix::process::CommandExt as _;
    command.process_group(0);
}

#[cfg(windows)]
fn configure_process_group(command: &mut Command) {
    use std::os::windows::process::CommandExt as _;
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    command.creation_flags(CREATE_NEW_PROCESS_GROUP);
}

fn terminate_process_tree(child: &mut Child) {
    let pid = child.id();
    #[cfg(windows)]
    {
        let program = std::env::var_os("SystemRoot")
            .map(PathBuf::from)
            .map(|root| root.join("System32").join("taskkill.exe"))
            .unwrap_or_else(|| PathBuf::from("taskkill.exe"));
        let mut terminate = Command::new(program);
        terminate
            .args([
                OsStr::new("/PID"),
                OsStr::new(&pid.to_string()),
                OsStr::new("/T"),
                OsStr::new("/F"),
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        drop(terminate.status());
    }
    #[cfg(unix)]
    {
        let mut terminate = Command::new("/bin/kill");
        terminate
            .args([OsStr::new("-KILL"), OsStr::new(&format!("-{pid}"))])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .env_clear();
        drop(terminate.status());
    }
    drop(child.kill());
    drop(child.wait());
}

fn default_device_kind() -> String {
    "robot".to_owned()
}

fn default_probe_namespace() -> String {
    "/rms".to_owned()
}

const fn default_settle_ms() -> u64 {
    750
}

const fn default_stale_after_ms() -> i64 {
    15_000
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings() -> Ros2Settings {
        Ros2Settings {
            device_id: "robot-01".to_owned(),
            display_name: "Inspection Robot".to_owned(),
            device_kind: "robot".to_owned(),
            domain_id: 7,
            probe_namespace: "/rms".to_owned(),
            allow_topic_prefixes: vec!["/sensors".to_owned(), "/tf".to_owned()],
            deny_topic_prefixes: vec!["/sensors/private".to_owned()],
            settle_ms: 100,
            stale_after_ms: 5_000,
            rmw_implementation: Some("rmw_cyclonedds_cpp".to_owned()),
            discovery_range: Ros2DiscoveryRange::Subnet,
            security: None,
        }
    }

    fn graph_json() -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({
            "schema": GRAPH_SCHEMA,
            "capturedAtUnixMs": 1_700_000_000_000_i64,
            "deviceId": "robot-01",
            "domainId": 7,
            "probeNamespace": "/rms",
            "rmwImplementation": "rmw_cyclonedds_cpp",
            "security": {"enabled": false, "strategy": "Permissive", "enclave": null},
            "nodes": [{"name": "camera", "namespace": "/sensors"}],
            "topics": [{
                "name": "/sensors/front/image",
                "types": ["sensor_msgs/msg/Image"],
                "publishers": [{
                    "nodeName": "camera",
                    "nodeNamespace": "/sensors",
                    "topicType": "sensor_msgs/msg/Image",
                    "qos": {
                        "history": "keep_last",
                        "depth": 5,
                        "reliability": "best_effort",
                        "durability": "volatile",
                        "liveliness": "automatic"
                    }
                }],
                "subscribers": []
            }],
            "limitsReached": {"nodes": false, "topics": false, "endpoints": false}
        }))
        .unwrap()
        .into_iter()
        .chain(*b"\n")
        .collect()
    }

    #[test]
    fn parses_and_maps_graph_without_exposing_endpoints() {
        let raw = parse_probe_outcome(
            &ProbeOutcome {
                status: successful_status(),
                stdout: graph_json(),
            },
            &settings(),
        )
        .unwrap();
        let observation = graph_to_observation(raw, &settings()).unwrap();
        assert_eq!(observation.identity, "ros2:robot-01");
        assert_eq!(observation.metadata["sourceCategories"], "camera");
        assert_eq!(observation.metadata["rendererHints"], "image");
        assert_eq!(observation.sources[0].status, SourceStatus::MetadataOnly);
        let serialized = serde_json::to_string(&observation).unwrap();
        assert!(!serialized.contains("endpoint_gid"));
        assert!(!serialized.contains("192.168."));
        assert!(!serialized.contains("nodeNamespace"));
    }

    #[test]
    fn renderer_contract_matches_current_mcap_decoder_outputs() {
        let camera_types = ["sensor_msgs/msg/Image", "sensor_msgs/msg/CompressedImage"];
        for topic_type in camera_types {
            assert_eq!(
                classification(topic_type),
                ("camera", SourceCategory::Camera, RendererHint::Image)
            );
        }

        assert_eq!(
            classification("sensor_msgs/msg/PointCloud2"),
            ("spatial", SourceCategory::Spatial, RendererHint::PointCloud)
        );
        assert_eq!(
            classification("tf2_msgs/msg/TFMessage"),
            (
                "transform",
                SourceCategory::Spatial,
                RendererHint::Transform3d
            )
        );

        let scalar_types = [
            "sensor_msgs/msg/BatteryState",
            "sensor_msgs/msg/FluidPressure",
            "sensor_msgs/msg/Illuminance",
            "sensor_msgs/msg/Imu",
            "sensor_msgs/msg/Joy",
            "sensor_msgs/msg/JointState",
            "sensor_msgs/msg/Range",
            "sensor_msgs/msg/RelativeHumidity",
            "sensor_msgs/msg/Temperature",
            "std_msgs/msg/Float64Array",
            "std_msgs/msg/Float64MultiArray",
        ];
        for topic_type in scalar_types {
            assert_eq!(
                classification(topic_type),
                ("telemetry", SourceCategory::Telemetry, RendererHint::Plot)
            );
        }

        let unsupported_types = [
            "sensor_msgs/msg/CameraInfo",
            "sensor_msgs/msg/LaserScan",
            "sensor_msgs/msg/NavSatFix",
            "nav_msgs/msg/Odometry",
            "nav_msgs/msg/Path",
            "geometry_msgs/msg/Pose",
            "geometry_msgs/msg/PoseStamped",
            "geometry_msgs/msg/PoseWithCovarianceStamped",
            "diagnostic_msgs/msg/DiagnosticArray",
        ];
        for topic_type in unsupported_types {
            assert_eq!(
                classification(topic_type),
                ("messages", SourceCategory::Log, RendererHint::Raw)
            );
        }

        assert_eq!(
            serde_json::to_value(RendererHint::Transform3d).unwrap(),
            serde_json::json!("transform3d")
        );
    }

    #[test]
    fn discovery_metadata_reports_raw_and_transform3d_without_overclaiming() {
        let mut categories = BTreeSet::new();
        let mut renderer_hints = BTreeSet::new();
        classify_topic_type(
            "tf2_msgs/msg/TFMessage",
            &mut categories,
            &mut renderer_hints,
        );
        classify_topic_type(
            "sensor_msgs/msg/LaserScan",
            &mut categories,
            &mut renderer_hints,
        );

        assert_eq!(categories, BTreeSet::from(["messages", "transform"]));
        assert_eq!(renderer_hints, BTreeSet::from(["raw", "transform3d"]));
    }

    #[test]
    fn rejects_multiple_records_and_missing_newline() {
        assert!(parse_single_ndjson_record(b"{}\n{}\n").is_err());
        assert!(parse_single_ndjson_record(b"{}").is_err());
    }

    #[test]
    fn rejects_oversized_and_invalid_utf8_records() {
        assert!(parse_single_ndjson_record(&vec![b'x'; MAX_STDOUT_BYTES as usize + 1]).is_err());
        assert!(parse_single_ndjson_record(&[0xff, b'\n']).is_err());
    }

    #[test]
    fn rust_filter_rejects_probe_filter_bypass() {
        let mut value: serde_json::Value = serde_json::from_slice(&graph_json()).unwrap();
        value["topics"][0]["name"] = serde_json::json!("/sensors/private/image");
        let mut bytes = serde_json::to_vec(&value).unwrap();
        bytes.push(b'\n');
        assert!(
            parse_probe_outcome(
                &ProbeOutcome {
                    status: successful_status(),
                    stdout: bytes,
                },
                &settings(),
            )
            .is_err()
        );
    }

    #[test]
    fn rejects_unknown_fields_and_endpoint_floods() {
        let mut value: serde_json::Value = serde_json::from_slice(&graph_json()).unwrap();
        value["unexpected"] = serde_json::json!("not accepted");
        let mut bytes = serde_json::to_vec(&value).unwrap();
        bytes.push(b'\n');
        assert!(
            parse_probe_outcome(
                &ProbeOutcome {
                    status: successful_status(),
                    stdout: bytes,
                },
                &settings(),
            )
            .is_err()
        );
    }

    #[test]
    fn probe_errors_are_sanitized() {
        let bytes = b"{\"schema\":\"rms.ros2.error.v1\",\"code\":\"graph_query_failed\",\"message\":\"/secret/keystore/key.pem at 192.168.1.3\"}\n";
        let error = parse_probe_outcome(
            &ProbeOutcome {
                status: failed_status(),
                stdout: bytes.to_vec(),
            },
            &settings(),
        )
        .unwrap_err();
        assert_eq!(error.to_string(), "ROS 2 graph discovery failed");
        assert!(!error.to_string().contains("secret"));
        assert!(!error.to_string().contains("192.168"));
    }

    #[test]
    fn validates_config_and_never_accepts_probe_paths() {
        let value = serde_json::json!({
            "deviceId": "robot-01",
            "displayName": "Robot",
            "probePath": "/tmp/untrusted.py"
        });
        assert!(serde_json::from_value::<Ros2Settings>(value).is_err());
        let mut invalid = settings();
        invalid.domain_id = 233;
        assert!(Ros2Adapter::new("ros-main".to_owned(), invalid).is_err());
    }

    #[test]
    fn fake_process_is_supervised_without_a_shell() {
        const FAKE_PROBE: &str = r#"
import argparse, json, time
p=argparse.ArgumentParser()
p.add_argument('--device-id', required=True)
p.add_argument('--domain-id', required=True, type=int)
p.add_argument('--probe-namespace', required=True)
p.add_argument('--settle-ms')
p.add_argument('--allow-prefix', action='append')
p.add_argument('--deny-prefix', action='append')
p.add_argument('--security-enclave')
a=p.parse_args()
print(json.dumps({
  'schema':'rms.ros2.graph.v1','capturedAtUnixMs':int(time.time()*1000),
  'deviceId':a.device_id,'domainId':a.domain_id,'probeNamespace':a.probe_namespace,
  'rmwImplementation':'rmw_cyclonedds_cpp',
  'security':{'enabled':False,'strategy':'Permissive','enclave':None},
  'nodes':[],'topics':[],
  'limitsReached':{'nodes':False,'topics':False,'endpoints':False}
}, separators=(',',':')))
"#;
        let command = ProbeCommand::python_source(FAKE_PROBE);
        let adapter =
            Ros2Adapter::with_probe_command("ros-main".to_owned(), settings(), command).unwrap();
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let batch = runtime
            .block_on(adapter.poll(AdapterPollContext {
                cancellation: tokio_util::sync::CancellationToken::new(),
                deadline: tokio::time::Instant::now() + Duration::from_secs(5),
                max_observations: 1,
            }))
            .unwrap();
        assert_eq!(batch.observations.len(), 1);
        assert_eq!(
            batch.observations[0].sources[0].status,
            SourceStatus::MetadataOnly
        );
    }

    #[test]
    fn fake_process_timeout_is_bounded_and_killed() {
        const HANGING_PROBE: &str = "import time\ntime.sleep(30)\n";
        let adapter = Ros2Adapter::with_probe_command(
            "ros-main".to_owned(),
            settings(),
            ProbeCommand::python_source(HANGING_PROBE),
        )
        .unwrap();
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let started = std::time::Instant::now();
        let error = runtime
            .block_on(adapter.poll(AdapterPollContext {
                cancellation: tokio_util::sync::CancellationToken::new(),
                deadline: tokio::time::Instant::now() + Duration::from_millis(150),
                max_observations: 1,
            }))
            .unwrap_err();
        assert_eq!(error.kind(), crate::adapter::AdapterErrorKind::Transient);
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn timeout_terminates_the_fake_probe_process_tree() {
        let marker_directory = tempfile::tempdir().unwrap();
        let marker = marker_directory.path().join("orphan-marker");
        let marker_literal = serde_json::to_string(marker.to_str().unwrap()).unwrap();
        let grandchild = format!(
            "import pathlib,time;time.sleep(1);pathlib.Path({marker_literal}).write_text('orphan')"
        );
        let grandchild_literal = serde_json::to_string(&grandchild).unwrap();
        let source = format!(
            "import subprocess,sys,time\nsubprocess.Popen([sys.executable,'-c',{grandchild_literal}])\ntime.sleep(30)\n"
        );
        let adapter = Ros2Adapter::with_probe_command(
            "ros-main".to_owned(),
            settings(),
            ProbeCommand::python_source(source),
        )
        .unwrap();
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let error = runtime
            .block_on(adapter.poll(AdapterPollContext {
                cancellation: tokio_util::sync::CancellationToken::new(),
                deadline: tokio::time::Instant::now() + Duration::from_millis(250),
                max_observations: 1,
            }))
            .unwrap_err();
        assert_eq!(error.kind(), crate::adapter::AdapterErrorKind::Transient);
        std::thread::sleep(Duration::from_millis(1_250));
        assert!(
            !marker.exists(),
            "probe grandchild survived process-tree kill"
        );
    }

    #[test]
    fn fake_process_stdout_and_stderr_caps_fail_closed() {
        for source in [
            "import sys\nsys.stdout.write('x' * 1100000)\nsys.stdout.flush()\n",
            "import sys, time\nsys.stderr.write('x' * 20000)\nsys.stderr.flush()\ntime.sleep(1)\n",
        ] {
            let adapter = Ros2Adapter::with_probe_command(
                "ros-main".to_owned(),
                settings(),
                ProbeCommand::python_source(source),
            )
            .unwrap();
            let runtime = tokio::runtime::Runtime::new().unwrap();
            let error = runtime
                .block_on(adapter.poll(AdapterPollContext {
                    cancellation: tokio_util::sync::CancellationToken::new(),
                    deadline: tokio::time::Instant::now() + Duration::from_secs(3),
                    max_observations: 1,
                }))
                .unwrap_err();
            assert_eq!(error.kind(), crate::adapter::AdapterErrorKind::InvalidData);
        }
    }

    #[test]
    fn sros2_environment_is_allowlisted_and_fail_closed() {
        const SECURITY_PROBE: &str = r#"
import argparse, json, os, pathlib, time
p=argparse.ArgumentParser()
p.add_argument('--device-id', required=True)
p.add_argument('--domain-id', required=True, type=int)
p.add_argument('--probe-namespace', required=True)
p.add_argument('--settle-ms')
p.add_argument('--allow-prefix', action='append')
p.add_argument('--deny-prefix', action='append')
p.add_argument('--security-enclave', required=True)
a=p.parse_args()
ok=(os.environ.get('ROS_SECURITY_ENABLE')=='true'
    and os.environ.get('ROS_SECURITY_STRATEGY')=='Enforce'
    and pathlib.Path(os.environ.get('ROS_SECURITY_KEYSTORE','')).is_dir()
    and os.environ.get('ROS_DOMAIN_ID')==str(a.domain_id)
    and os.environ.get('ROS_AUTOMATIC_DISCOVERY_RANGE')=='SUBNET'
    and os.environ.get('RMW_IMPLEMENTATION')=='rmw_cyclonedds_cpp'
    and 'HOME' not in os.environ and 'USERPROFILE' not in os.environ)
print(json.dumps({
  'schema':'rms.ros2.graph.v1','capturedAtUnixMs':int(time.time()*1000),
  'deviceId':a.device_id if ok else 'environment-leak',
  'domainId':a.domain_id,'probeNamespace':a.probe_namespace,
  'rmwImplementation':os.environ.get('RMW_IMPLEMENTATION','missing'),
  'security':{'enabled':ok,'strategy':os.environ.get('ROS_SECURITY_STRATEGY','missing'),
              'enclave':a.security_enclave},
  'nodes':[],'topics':[],
  'limitsReached':{'nodes':False,'topics':False,'endpoints':False}
}, separators=(',',':')))
"#;
        let keystore = tempfile::tempdir().unwrap();
        let mut secure_settings = settings();
        secure_settings.security = Some(Ros2SecuritySettings {
            keystore: keystore.path().to_owned(),
            enclave: "/rms/edge/discovery".to_owned(),
            strategy: Ros2SecurityStrategy::Enforce,
        });
        let adapter = Ros2Adapter::with_probe_command(
            "ros-main".to_owned(),
            secure_settings,
            ProbeCommand::python_source(SECURITY_PROBE),
        )
        .unwrap();
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let batch = runtime
            .block_on(adapter.poll(AdapterPollContext {
                cancellation: tokio_util::sync::CancellationToken::new(),
                deadline: tokio::time::Instant::now() + Duration::from_secs(3),
                max_observations: 1,
            }))
            .unwrap();
        assert_eq!(batch.observations[0].trust, ObservationTrust::Authenticated);
    }

    #[test]
    fn cancellation_before_spawn_is_observation_only() {
        let cancellation = tokio_util::sync::CancellationToken::new();
        cancellation.cancel();
        let adapter = Ros2Adapter::new("ros-main".to_owned(), settings()).unwrap();
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let error = runtime
            .block_on(adapter.poll(AdapterPollContext {
                cancellation,
                deadline: tokio::time::Instant::now() + Duration::from_secs(1),
                max_observations: 1,
            }))
            .unwrap_err();
        assert_eq!(error.kind(), crate::adapter::AdapterErrorKind::Cancelled);
    }

    #[cfg(unix)]
    fn successful_status() -> ExitStatus {
        use std::os::unix::process::ExitStatusExt as _;
        ExitStatus::from_raw(0)
    }

    #[cfg(windows)]
    fn successful_status() -> ExitStatus {
        use std::os::windows::process::ExitStatusExt as _;
        ExitStatus::from_raw(0)
    }

    #[cfg(unix)]
    fn failed_status() -> ExitStatus {
        use std::os::unix::process::ExitStatusExt as _;
        ExitStatus::from_raw(1 << 8)
    }

    #[cfg(windows)]
    fn failed_status() -> ExitStatus {
        use std::os::windows::process::ExitStatusExt as _;
        ExitStatus::from_raw(1)
    }
}
