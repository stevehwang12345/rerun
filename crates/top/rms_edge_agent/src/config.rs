//! Durable configuration loading, validation, and atomic replacement.

use std::{
    collections::BTreeSet,
    fmt, fs, io,
    net::{IpAddr, Ipv6Addr},
    path::{Path, PathBuf},
    time::Duration,
};

use serde::{Deserialize, Serialize};
use tempfile::NamedTempFile;

use crate::adapter::AdapterKind;

const MAX_CONFIG_BYTES: u64 = 256 * 1024;
const MAX_ADAPTER_SETTINGS_BYTES: usize = 32 * 1024;
const SERVER_FRESHNESS_BUDGET_MS: u64 = 10_000;
const MAX_HEARTBEAT_INTERVAL_MS: u64 = 5_000;

/// Runtime safety mode.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Environment {
    /// Strict paths, TLS, and network binding validation.
    #[default]
    Production,
    /// Local integration testing. Authentication and loopback admin binding remain mandatory.
    Development,
}

/// Complete edge-agent configuration.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AgentConfig {
    /// Configuration schema version. Currently `1`.
    pub schema_version: u32,
    /// Runtime safety mode.
    #[serde(default)]
    pub environment: Environment,
    /// Stable identity file. The file contains private key material.
    pub identity_path: PathBuf,
    /// Loopback administration API settings.
    pub admin: AdminConfig,
    /// Signed local-network advertisement and public pairing endpoint.
    pub advertise: AdvertiseConfig,
    /// Adapter supervision limits.
    #[serde(default)]
    pub supervisor: SupervisorConfig,
    /// Optional outbound RMS control-plane heartbeat.
    #[serde(default)]
    pub control_plane: ControlPlaneConfig,
    /// Protocol adapters.
    #[serde(default)]
    pub adapters: Vec<AdapterConfig>,
}

/// Authenticated loopback administration settings.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AdminConfig {
    /// Literal loopback address; hostnames and wildcard addresses are rejected.
    pub bind_ip: IpAddr,
    /// TCP port.
    pub port: u16,
    /// File containing the bearer token. The token is never accepted inline.
    pub bearer_token_path: PathBuf,
    /// Maximum in-flight requests.
    #[serde(default = "default_admin_max_connections")]
    pub max_connections: usize,
    /// Per-request timeout.
    #[serde(default = "default_admin_timeout_ms")]
    pub request_timeout_ms: u64,
}

/// Signed mDNS and pairing-endpoint settings.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AdvertiseConfig {
    /// Whether discovery and the read-only pairing endpoint are enabled.
    #[serde(default)]
    pub enabled: bool,
    /// Operator-facing device name.
    pub display_name: String,
    /// RMS device kind.
    pub device_kind: String,
    /// Literal private/link-local address used for mDNS membership and pairing endpoint binding.
    pub bind_ip: IpAddr,
    /// Public read-only pairing/data-plane manifest port advertised through DNS-SD.
    pub service_port: u16,
    /// Safe manifest endpoint path.
    #[serde(default = "default_manifest_path")]
    pub path: String,
    /// DNS record lifetime.
    #[serde(default = "default_advertisement_ttl")]
    pub ttl_seconds: u32,
    /// Explicit discovery capabilities.
    #[serde(default)]
    pub capabilities: Vec<String>,
    /// Optional organization proof key reference.
    #[serde(default)]
    pub organization_key_id: Option<String>,
    /// Organization that owns the discovery proof.
    #[serde(default)]
    pub organization_id: Option<String>,
    /// Optional file containing the organization discovery PSK.
    #[serde(default)]
    pub organization_psk_path: Option<PathBuf>,
}

/// Bounded adapter restart and freshness policy.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SupervisorConfig {
    /// Per-adapter observation cap.
    #[serde(default = "default_max_observations")]
    pub max_observations_per_adapter: usize,
    /// Adapter hard poll timeout.
    #[serde(default = "default_poll_timeout_ms")]
    pub poll_timeout_ms: u64,
    /// Successful observations older than this become stale.
    #[serde(default = "default_stale_after_ms")]
    pub stale_after_ms: u64,
    /// Initial retry backoff.
    #[serde(default = "default_backoff_initial_ms")]
    pub backoff_initial_ms: u64,
    /// Maximum retry backoff.
    #[serde(default = "default_backoff_max_ms")]
    pub backoff_max_ms: u64,
    /// Maximum failures admitted in one restart window.
    #[serde(default = "default_restart_limit")]
    pub restart_limit: usize,
    /// Restart accounting window.
    #[serde(default = "default_restart_window_ms")]
    pub restart_window_ms: u64,
    /// Bounded in-process event channel.
    #[serde(default = "default_event_capacity")]
    pub event_capacity: usize,
}

impl Default for SupervisorConfig {
    fn default() -> Self {
        Self {
            max_observations_per_adapter: default_max_observations(),
            poll_timeout_ms: default_poll_timeout_ms(),
            stale_after_ms: default_stale_after_ms(),
            backoff_initial_ms: default_backoff_initial_ms(),
            backoff_max_ms: default_backoff_max_ms(),
            restart_limit: default_restart_limit(),
            restart_window_ms: default_restart_window_ms(),
            event_capacity: default_event_capacity(),
        }
    }
}

/// Outbound RMS status synchronization.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ControlPlaneConfig {
    /// Whether signed heartbeat delivery is enabled.
    #[serde(default)]
    pub enabled: bool,
    /// RMS base URL.
    #[serde(default)]
    pub server_url: String,
    /// File containing the enrollment bearer token.
    #[serde(default)]
    pub bearer_token_path: PathBuf,
    /// Optional PEM root certificate for an internal RMS certificate authority.
    #[serde(default)]
    pub ca_certificate_path: Option<PathBuf>,
    /// Normal heartbeat interval.
    #[serde(default = "default_heartbeat_interval_ms")]
    pub interval_ms: u64,
    /// HTTP request timeout.
    #[serde(default = "default_heartbeat_timeout_ms")]
    pub timeout_ms: u64,
    /// Maximum observations sent per adapter.
    #[serde(default = "default_heartbeat_observations")]
    pub max_observations_per_adapter: usize,
}

/// One adapter instance and its opaque, size-bounded settings.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AdapterConfig {
    /// Stable instance identifier.
    pub id: String,
    /// Protocol implementation.
    pub kind: AdapterKind,
    /// Whether the adapter is started.
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Delay between successful polls.
    #[serde(default = "default_adapter_poll_interval_ms")]
    pub poll_interval_ms: u64,
    /// Protocol-specific settings; must be a JSON/TOML object.
    #[serde(default)]
    pub settings: serde_json::Value,
}

impl AgentConfig {
    /// Validate production and resource-safety invariants.
    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.schema_version != 1 {
            return Err(ConfigError::validation("schema_version must be 1"));
        }
        validate_path(&self.identity_path, self.environment, "identity_path")?;
        self.validate_admin()?;
        self.validate_advertise()?;
        self.validate_supervisor()?;
        self.validate_control_plane()?;
        self.validate_adapters()?;
        self.validate_control_plane_freshness()?;
        self.validate_adapter_advertisement_contract()
    }

    fn validate_admin(&self) -> Result<(), ConfigError> {
        if !self.admin.bind_ip.is_loopback() || self.admin.port == 0 {
            return Err(ConfigError::validation(
                "admin must bind a nonzero port on a literal loopback address",
            ));
        }
        validate_path(
            &self.admin.bearer_token_path,
            self.environment,
            "admin.bearer_token_path",
        )?;
        if self.admin.bearer_token_path == self.identity_path {
            return Err(ConfigError::validation(
                "identity and admin token paths must be different",
            ));
        }
        if !(1..=128).contains(&self.admin.max_connections)
            || !(100..=30_000).contains(&self.admin.request_timeout_ms)
        {
            return Err(ConfigError::validation("admin resource limits are invalid"));
        }
        Ok(())
    }

    fn validate_advertise(&self) -> Result<(), ConfigError> {
        if self.advertise.display_name.is_empty()
            || self.advertise.display_name.len() > 64
            || contains_control(&self.advertise.display_name)
        {
            return Err(ConfigError::validation("invalid advertise.display_name"));
        }
        if !matches!(
            self.advertise.device_kind.as_str(),
            "robot" | "drone" | "vehicle" | "camera" | "gateway"
        ) {
            return Err(ConfigError::validation("invalid advertise.device_kind"));
        }
        if self.advertise.enabled {
            if self.advertise.service_port == 0
                || !matches!(self.advertise.bind_ip, IpAddr::V4(_))
                || !is_private_or_link_local(self.advertise.bind_ip)
            {
                return Err(ConfigError::validation(
                    "advertisement requires a literal private/link-local bind and nonzero port",
                ));
            }
            if !safe_path(&self.advertise.path) || self.advertise.path == "/rms/v1/challenge" {
                return Err(ConfigError::validation("invalid advertise.path"));
            }
        }
        if !(5..=120).contains(&self.advertise.ttl_seconds)
            || self.advertise.capabilities.len() > 16
        {
            return Err(ConfigError::validation("advertisement limits are invalid"));
        }
        let mut capabilities = BTreeSet::new();
        for capability in &self.advertise.capabilities {
            if !valid_token(capability, 32) || !capabilities.insert(capability) {
                return Err(ConfigError::validation("invalid or duplicate capability"));
            }
        }
        match (
            &self.advertise.organization_id,
            &self.advertise.organization_key_id,
            &self.advertise.organization_psk_path,
        ) {
            (Some(organization_id), Some(key_id), Some(path)) => {
                if !valid_token(organization_id, 64) || !valid_token(key_id, 32) {
                    return Err(ConfigError::validation("invalid organization id or key id"));
                }
                validate_path(path, self.environment, "advertise.organization_psk_path")?;
            }
            (None, None, None) => {}
            _ => {
                return Err(ConfigError::validation(
                    "organization id, key id, and PSK path must be configured together",
                ));
            }
        }
        Ok(())
    }

    fn validate_supervisor(&self) -> Result<(), ConfigError> {
        let supervisor = &self.supervisor;
        if !(1..=512).contains(&supervisor.max_observations_per_adapter)
            || !(100..=60_000).contains(&supervisor.poll_timeout_ms)
            || !(1_000..=300_000).contains(&supervisor.stale_after_ms)
            || !(10..=60_000).contains(&supervisor.backoff_initial_ms)
            || supervisor.backoff_initial_ms > supervisor.backoff_max_ms
            || supervisor.backoff_max_ms > 300_000
            || !(1..=100).contains(&supervisor.restart_limit)
            || !(1_000..=3_600_000).contains(&supervisor.restart_window_ms)
            || !(1..=4_096).contains(&supervisor.event_capacity)
        {
            return Err(ConfigError::validation(
                "invalid supervisor resource limits",
            ));
        }
        Ok(())
    }

    fn validate_control_plane(&self) -> Result<(), ConfigError> {
        let control_plane = &self.control_plane;
        if !control_plane.enabled {
            return Ok(());
        }
        validate_path(
            &control_plane.bearer_token_path,
            self.environment,
            "control_plane.bearer_token_path",
        )?;
        if let Some(path) = &control_plane.ca_certificate_path {
            validate_path(path, self.environment, "control_plane.ca_certificate_path")?;
        }
        let url = reqwest::Url::parse(&control_plane.server_url)
            .map_err(|_error| ConfigError::validation("invalid control-plane URL"))?;
        let secure = url.scheme() == "https";
        let development_loopback = self.environment == Environment::Development
            && url.scheme() == "http"
            && url
                .host_str()
                .and_then(|host| host.parse::<IpAddr>().ok())
                .is_some_and(|address| address.is_loopback());
        if !secure && !development_loopback {
            return Err(ConfigError::validation(
                "control plane requires HTTPS (loopback HTTP is development-only)",
            ));
        }
        if url.query().is_some()
            || url.fragment().is_some()
            || url.username() != ""
            || url.password().is_some()
        {
            return Err(ConfigError::validation("unsafe control-plane URL"));
        }
        if !(500..=MAX_HEARTBEAT_INTERVAL_MS).contains(&control_plane.interval_ms)
            || !(100..=30_000).contains(&control_plane.timeout_ms)
            || control_plane.timeout_ms >= control_plane.interval_ms
            || !(1..=512).contains(&control_plane.max_observations_per_adapter)
        {
            return Err(ConfigError::validation("invalid control-plane limits"));
        }
        Ok(())
    }

    fn validate_control_plane_freshness(&self) -> Result<(), ConfigError> {
        if !self.control_plane.enabled {
            return Ok(());
        }
        if self.supervisor.stale_after_ms > SERVER_FRESHNESS_BUDGET_MS {
            return Err(ConfigError::validation(
                "supervisor.stale_after_ms exceeds the RMS heartbeat freshness budget",
            ));
        }
        if self
            .adapters
            .iter()
            .filter(|adapter| adapter.enabled)
            .any(|adapter| {
                adapter
                    .poll_interval_ms
                    .saturating_add(self.supervisor.poll_timeout_ms)
                    .saturating_add(self.control_plane.interval_ms)
                    > SERVER_FRESHNESS_BUDGET_MS
            })
        {
            return Err(ConfigError::validation(
                "adapter poll and heartbeat intervals exceed the RMS freshness budget",
            ));
        }
        Ok(())
    }

    fn validate_adapters(&self) -> Result<(), ConfigError> {
        if self.adapters.len() > 16 {
            return Err(ConfigError::validation("too many adapters"));
        }
        let mut identifiers = BTreeSet::new();
        for adapter in &self.adapters {
            if !valid_token(&adapter.id, 48) || !identifiers.insert(adapter.id.as_str()) {
                return Err(ConfigError::validation("invalid or duplicate adapter id"));
            }
            if !(100..=300_000).contains(&adapter.poll_interval_ms)
                || adapter
                    .poll_interval_ms
                    .saturating_add(self.supervisor.poll_timeout_ms)
                    > self.supervisor.stale_after_ms
                || !adapter.settings.is_object()
                || serde_json::to_vec(&adapter.settings)
                    .map_err(ConfigError::serialize)?
                    .len()
                    > MAX_ADAPTER_SETTINGS_BYTES
            {
                return Err(ConfigError::validation(
                    "invalid adapter settings or limits",
                ));
            }
        }
        Ok(())
    }

    fn validate_adapter_advertisement_contract(&self) -> Result<(), ConfigError> {
        let allowed = ["mavlink", "ros2_dds", "status"];
        if self
            .advertise
            .capabilities
            .iter()
            .any(|capability| !allowed.contains(&capability.as_str()))
        {
            return Err(ConfigError::validation(
                "advertisement contains an unsupported capability",
            ));
        }
        if !self.advertise.enabled {
            return Ok(());
        }
        if !self
            .advertise
            .capabilities
            .iter()
            .any(|capability| matches!(capability.as_str(), "mavlink" | "ros2_dds"))
        {
            return Err(ConfigError::validation(
                "advertisement requires at least one discovery adapter capability",
            ));
        }
        for adapter in self.adapters.iter().filter(|adapter| adapter.enabled) {
            if !self
                .advertise
                .capabilities
                .iter()
                .any(|capability| capability == adapter.kind.as_str())
            {
                return Err(ConfigError::validation(
                    "enabled adapter is missing from advertised capabilities",
                ));
            }
        }
        for capability in self
            .advertise
            .capabilities
            .iter()
            .filter(|capability| capability.as_str() != "status")
        {
            if !self
                .adapters
                .iter()
                .any(|adapter| adapter.enabled && adapter.kind.as_str() == capability)
            {
                return Err(ConfigError::validation(
                    "advertised adapter capability is not enabled",
                ));
            }
        }
        Ok(())
    }
}

/// Atomic on-disk configuration store.
#[derive(Clone, Debug)]
pub struct ConfigStore {
    path: PathBuf,
}

impl ConfigStore {
    /// Create a store for a fixed path.
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// Load, size-bound, deserialize, and validate a configuration.
    pub fn load(&self) -> Result<AgentConfig, ConfigError> {
        let metadata = fs::metadata(&self.path).map_err(ConfigError::io)?;
        if metadata.len() > MAX_CONFIG_BYTES {
            return Err(ConfigError::validation("configuration file is too large"));
        }
        let bytes = fs::read(&self.path).map_err(ConfigError::io)?;
        let text = std::str::from_utf8(&bytes)
            .map_err(|_error| ConfigError::validation("configuration is not UTF-8"))?;
        let config: AgentConfig = toml::from_str(text).map_err(ConfigError::toml_decode)?;
        config.validate()?;
        Ok(config)
    }

    /// Atomically replace configuration after validation and fsync.
    pub fn persist(&self, config: &AgentConfig) -> Result<(), ConfigError> {
        config.validate()?;
        let encoded = toml::to_string_pretty(config).map_err(ConfigError::toml_encode)?;
        if encoded.len() > usize::try_from(MAX_CONFIG_BYTES).unwrap_or(usize::MAX) {
            return Err(ConfigError::validation("configuration file is too large"));
        }
        let parent = self
            .path
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        fs::create_dir_all(parent).map_err(ConfigError::io)?;
        let mut temporary = NamedTempFile::new_in(parent).map_err(ConfigError::io)?;
        io::Write::write_all(&mut temporary, encoded.as_bytes()).map_err(ConfigError::io)?;
        io::Write::write_all(&mut temporary, b"\n").map_err(ConfigError::io)?;
        temporary.as_file().sync_all().map_err(ConfigError::io)?;
        let (_file, temporary_path) = temporary.keep().map_err(|err| ConfigError::io(err.error))?;
        if let Err(err) = atomicwrites::replace_atomic(&temporary_path, &self.path) {
            drop(fs::remove_file(&temporary_path));
            return Err(ConfigError::io(err));
        }
        Ok(())
    }

    /// Configured path.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// Configuration error with a deliberately non-secret message.
#[derive(Debug)]
pub struct ConfigError {
    message: String,
}

impl ConfigError {
    fn validation(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }

    fn io(error: impl fmt::Display) -> Self {
        Self::validation(format!("configuration I/O failed: {error}"))
    }

    fn toml_decode(error: impl fmt::Display) -> Self {
        Self::validation(format!("configuration parsing failed: {error}"))
    }

    fn toml_encode(error: impl fmt::Display) -> Self {
        Self::validation(format!("configuration encoding failed: {error}"))
    }

    fn serialize(error: impl fmt::Display) -> Self {
        Self::validation(format!("adapter settings serialization failed: {error}"))
    }
}

impl fmt::Display for ConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for ConfigError {}

/// Load a secret token with strict size and character constraints.
pub(crate) fn load_secret(
    path: &Path,
    minimum: usize,
    maximum: usize,
) -> Result<Vec<u8>, ConfigError> {
    let metadata = fs::metadata(path).map_err(ConfigError::io)?;
    #[cfg(unix)]
    validate_secret_permissions(&metadata)?;
    if metadata.len() > u64::try_from(maximum).unwrap_or(u64::MAX) {
        return Err(ConfigError::validation("secret file exceeds size limit"));
    }
    let bytes = fs::read(path).map_err(ConfigError::io)?;
    let trimmed = bytes
        .strip_suffix(b"\r\n")
        .or_else(|| bytes.strip_suffix(b"\n"))
        .unwrap_or(&bytes);
    if !(minimum..=maximum).contains(&trimmed.len())
        || trimmed.iter().any(|byte| !(0x21..=0x7e).contains(byte))
    {
        return Err(ConfigError::validation("secret file has invalid content"));
    }
    Ok(trimmed.to_vec())
}

#[cfg(unix)]
fn validate_secret_permissions(metadata: &fs::Metadata) -> Result<(), ConfigError> {
    use std::os::unix::fs::PermissionsExt as _;
    if metadata.permissions().mode() & 0o077 != 0 {
        return Err(ConfigError::validation(
            "secret file permissions must not grant group/other access",
        ));
    }
    Ok(())
}

fn validate_path(path: &Path, environment: Environment, field: &str) -> Result<(), ConfigError> {
    if path.as_os_str().is_empty()
        || (environment == Environment::Production && !path.is_absolute())
    {
        return Err(ConfigError::validation(format!(
            "{field} must be an absolute non-empty path in production"
        )));
    }
    Ok(())
}

fn safe_path(path: &str) -> bool {
    path.starts_with('/')
        && path.len() <= 128
        && !path.contains("..")
        && !path.contains(['?', '#', '\\'])
        && path.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'-' | b'_' | b'.' | b'~')
        })
}

fn valid_token(value: &str, maximum: usize) -> bool {
    !value.is_empty()
        && value.len() <= maximum
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
}

fn contains_control(value: &str) -> bool {
    value.chars().any(char::is_control)
}

fn is_private_or_link_local(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(address) => address.is_private() || address.is_link_local(),
        IpAddr::V6(address) => is_ipv6_unique_local(address) || address.is_unicast_link_local(),
    }
}

fn is_ipv6_unique_local(address: Ipv6Addr) -> bool {
    address.octets()[0] & 0xfe == 0xfc
}

const fn default_admin_max_connections() -> usize {
    32
}
const fn default_admin_timeout_ms() -> u64 {
    2_000
}
fn default_manifest_path() -> String {
    "/rms/v1/manifest".to_owned()
}
const fn default_advertisement_ttl() -> u32 {
    30
}
const fn default_max_observations() -> usize {
    128
}
const fn default_poll_timeout_ms() -> u64 {
    2_000
}
const fn default_stale_after_ms() -> u64 {
    SERVER_FRESHNESS_BUDGET_MS
}
const fn default_backoff_initial_ms() -> u64 {
    250
}
const fn default_backoff_max_ms() -> u64 {
    30_000
}
const fn default_restart_limit() -> usize {
    5
}
const fn default_restart_window_ms() -> u64 {
    60_000
}
const fn default_event_capacity() -> usize {
    256
}
const fn default_heartbeat_interval_ms() -> u64 {
    2_000
}
const fn default_heartbeat_timeout_ms() -> u64 {
    1_000
}
const fn default_heartbeat_observations() -> usize {
    64
}
const fn default_adapter_poll_interval_ms() -> u64 {
    2_000
}
const fn default_true() -> bool {
    true
}

/// Convert milliseconds to a duration without repeating policy details.
pub(crate) const fn duration_ms(milliseconds: u64) -> Duration {
    Duration::from_millis(milliseconds)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid_config(root: &Path) -> AgentConfig {
        AgentConfig {
            schema_version: 1,
            environment: Environment::Development,
            identity_path: root.join("identity.json"),
            admin: AdminConfig {
                bind_ip: "127.0.0.1".parse().expect("test address"),
                port: 9_878,
                bearer_token_path: root.join("admin.token"),
                max_connections: 8,
                request_timeout_ms: 1_000,
            },
            advertise: AdvertiseConfig {
                enabled: false,
                display_name: "RMS Edge".to_owned(),
                device_kind: "gateway".to_owned(),
                bind_ip: "192.168.1.2".parse().expect("test address"),
                service_port: 9_879,
                path: default_manifest_path(),
                ttl_seconds: 30,
                capabilities: vec!["ros2_dds".to_owned()],
                organization_id: None,
                organization_key_id: None,
                organization_psk_path: None,
            },
            supervisor: SupervisorConfig::default(),
            control_plane: ControlPlaneConfig::default(),
            adapters: Vec::new(),
        }
    }

    #[test]
    fn rejects_non_loopback_admin() {
        let root = tempfile::tempdir().expect("temporary directory");
        let mut config = valid_config(root.path());
        config.admin.bind_ip = "192.168.1.1".parse().expect("test address");
        assert!(config.validate().is_err());
    }

    #[test]
    fn atomically_round_trips_configuration() {
        let root = tempfile::tempdir().expect("temporary directory");
        let path = root.path().join("agent.toml");
        let store = ConfigStore::new(&path);
        let config = valid_config(root.path());
        store.persist(&config).expect("configuration persists");
        let loaded = store.load().expect("configuration loads");
        assert_eq!(loaded.schema_version, 1);
        assert_eq!(
            loaded.admin.bind_ip,
            "127.0.0.1".parse::<IpAddr>().expect("test address")
        );
    }

    #[test]
    fn production_rejects_relative_secret_paths() {
        let root = tempfile::tempdir().expect("temporary directory");
        let mut config = valid_config(root.path());
        config.environment = Environment::Production;
        config.admin.bearer_token_path = PathBuf::from("token");
        assert!(config.validate().is_err());
    }

    #[test]
    fn advertisement_rejects_reserved_challenge_route() {
        let root = tempfile::tempdir().expect("temporary directory");
        let mut config = valid_config(root.path());
        config.advertise.enabled = true;
        config.advertise.path = "/rms/v1/challenge".to_owned();
        assert!(config.validate_advertise().is_err());
    }

    #[test]
    fn control_plane_timing_must_fit_the_server_freshness_budget() {
        let root = tempfile::tempdir().expect("temporary directory");
        let mut config = valid_config(root.path());
        config.control_plane = ControlPlaneConfig {
            enabled: true,
            server_url: "http://127.0.0.1:8080/".to_owned(),
            bearer_token_path: root.path().join("enrollment.token"),
            ca_certificate_path: None,
            interval_ms: 2_000,
            timeout_ms: 1_000,
            max_observations_per_adapter: 64,
        };
        config.adapters.push(AdapterConfig {
            id: "vehicle-link".to_owned(),
            kind: AdapterKind::Mavlink,
            enabled: true,
            poll_interval_ms: 2_000,
            settings: serde_json::json!({}),
        });

        assert!(config.validate().is_ok());

        config.control_plane.interval_ms = MAX_HEARTBEAT_INTERVAL_MS + 1;
        assert!(config.validate().is_err());
        config.control_plane.interval_ms = 2_000;

        config.supervisor.stale_after_ms = SERVER_FRESHNESS_BUDGET_MS + 1;
        assert!(config.validate().is_err());
        config.supervisor.stale_after_ms = SERVER_FRESHNESS_BUDGET_MS;

        config.adapters[0].poll_interval_ms = 6_001;
        assert!(config.validate().is_err());
    }
}
