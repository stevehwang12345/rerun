//! Signed DNS-SD advertisement and read-only pairing endpoint.

use std::{
    fmt, io,
    net::{IpAddr, Ipv4Addr, SocketAddr, SocketAddrV4},
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use axum::{
    BoxError, Json, Router,
    error_handling::HandleErrorLayer,
    extract::{DefaultBodyLimit, State},
    http::StatusCode,
    response::{IntoResponse as _, Response},
    routing::{get, post},
};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use ring::hmac;
use serde::{Deserialize, Serialize};
use socket2::{Domain, Protocol, Socket, Type};
use tokio::net::{TcpListener, UdpSocket};
use tokio_util::sync::CancellationToken;
use tower::ServiceBuilder;

use crate::{
    config::{AdvertiseConfig, load_secret},
    health::AgentMetrics,
    identity::DeviceIdentity,
};

const SERVICE_TYPE: &str = "_rms._tcp.local";
const ADVERTISEMENT_CONTEXT: &str = "rms-advertisement-v1";
const CHALLENGE_CONTEXT: &str = "rms-challenge-v1";
const MDNS_PORT: u16 = 5_353;
const MAX_QUERY_BYTES: usize = 4 * 1024;
const MAX_DNS_QUESTIONS: usize = 16;
const MAX_DNS_LABELS: usize = 32;
const MAX_POINTER_JUMPS: usize = 16;
const MIN_RESPONSE_INTERVAL: Duration = Duration::from_millis(50);
const CHALLENGE_PATH: &str = "/rms/v1/challenge";

/// Signed advertisement publisher.
pub struct AdvertisementService {
    config: AdvertiseConfig,
    identity: Arc<DeviceIdentity>,
    organization_proof: Option<OrganizationProof>,
    metrics: Arc<AgentMetrics>,
}

impl fmt::Debug for AdvertisementService {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AdvertisementService")
            .field("config", &self.config)
            .field("identity", &self.identity.public())
            .field(
                "organization_proof",
                &self.organization_proof.as_ref().map(|proof| &proof.key_id),
            )
            .finish_non_exhaustive()
    }
}

struct OrganizationProof {
    organization_id: String,
    key_id: String,
    key: hmac::Key,
}

impl AdvertisementService {
    /// Construct the service and load an optional organization PSK exactly once.
    pub fn new(
        config: AdvertiseConfig,
        identity: Arc<DeviceIdentity>,
        metrics: Arc<AgentMetrics>,
    ) -> Result<Self, AdvertisementError> {
        let organization_proof = match (
            config.organization_id.clone(),
            config.organization_key_id.clone(),
            config.organization_psk_path.as_deref(),
        ) {
            (Some(organization_id), Some(key_id), Some(path)) => {
                let secret = load_secret(path, 32, 512)
                    .map_err(|error| AdvertisementError::new(error.to_string()))?;
                Some(OrganizationProof {
                    organization_id,
                    key_id,
                    key: hmac::Key::new(hmac::HMAC_SHA256, &secret),
                })
            }
            (None, None, None) => None,
            _ => {
                return Err(AdvertisementError::new(
                    "incomplete organization proof configuration",
                ));
            }
        };
        Ok(Self {
            config,
            identity,
            organization_proof,
            metrics,
        })
    }

    /// Serve mDNS and the read-only pairing API until cancellation.
    pub async fn run(&self, cancellation: CancellationToken) -> Result<(), AdvertisementError> {
        if !self.config.enabled {
            cancellation.cancelled().await;
            return Ok(());
        }
        let mdns = self.run_mdns(cancellation.clone());
        let pairing = self.run_pairing_endpoint(cancellation);
        tokio::try_join!(mdns, pairing)?;
        Ok(())
    }

    async fn run_mdns(&self, cancellation: CancellationToken) -> Result<(), AdvertisementError> {
        let interface = match self.config.bind_ip {
            IpAddr::V4(address) => address,
            IpAddr::V6(_) => {
                return Err(AdvertisementError::new(
                    "IPv6 mDNS advertisement is not enabled in this release",
                ));
            }
        };
        let socket = mdns_socket(interface).map_err(AdvertisementError::io)?;
        let mut packet = vec![0_u8; MAX_QUERY_BYTES + 1];
        let mut next_response = tokio::time::Instant::now();
        loop {
            tokio::select! {
                () = cancellation.cancelled() => return Ok(()),
                result = socket.recv_from(&mut packet) => {
                    let (size, source) = result.map_err(AdvertisementError::io)?;
                    if size > MAX_QUERY_BYTES || !is_lan_request_source(source.ip()) {
                        continue;
                    }
                    let Some(transaction_id) = query_requests_rms(&packet[..size]) else {
                        continue;
                    };
                    let now = tokio::time::Instant::now();
                    if now < next_response {
                        continue;
                    }
                    next_response = now + MIN_RESPONSE_INTERVAL;
                    let advertisement = self.signed_advertisement()?;
                    let response = build_dns_response(transaction_id, &advertisement)?;
                    socket.send_to(&response, source).await.map_err(AdvertisementError::io)?;
                    self.metrics.advertisement_response();
                }
            }
        }
    }

    async fn run_pairing_endpoint(
        &self,
        cancellation: CancellationToken,
    ) -> Result<(), AdvertisementError> {
        let listener = TcpListener::bind(SocketAddr::new(
            self.config.bind_ip,
            self.config.service_port,
        ))
        .await
        .map_err(AdvertisementError::io)?;
        let state = PairingState {
            config: self.config.clone(),
            identity: Arc::clone(&self.identity),
            organization_proof: self
                .organization_proof
                .as_ref()
                .map(|proof| OrganizationProof {
                    organization_id: proof.organization_id.clone(),
                    key_id: proof.key_id.clone(),
                    key: proof.key.clone(),
                }),
        };
        let mut router = Router::new()
            .route(&self.config.path, get(manifest))
            .route(CHALLENGE_PATH, post(challenge));
        if self
            .config
            .capabilities
            .iter()
            .any(|capability| capability == "ros2_dds")
        {
            router = router.route(&source_route(&self.config.path, "ros2"), get(manifest));
        }
        if self
            .config
            .capabilities
            .iter()
            .any(|capability| capability == "mavlink")
        {
            router = router.route(&source_route(&self.config.path, "mavlink"), get(manifest));
        }
        let router = router
            .with_state(state)
            .layer(DefaultBodyLimit::max(1_024))
            .layer(
                ServiceBuilder::new()
                    .layer(HandleErrorLayer::new(handle_service_error))
                    .load_shed()
                    .concurrency_limit(16)
                    .timeout(Duration::from_secs(2)),
            );
        axum::serve(listener, router)
            .with_graceful_shutdown(cancellation.cancelled_owned())
            .await
            .map_err(|error| AdvertisementError::new(format!("pairing endpoint failed: {error}")))
    }

    fn signed_advertisement(&self) -> Result<SignedAdvertisement, AdvertisementError> {
        signed_advertisement(
            &self.config,
            &self.identity,
            self.organization_proof.as_ref(),
        )
    }
}

impl Clone for OrganizationProof {
    fn clone(&self) -> Self {
        Self {
            organization_id: self.organization_id.clone(),
            key_id: self.key_id.clone(),
            key: self.key.clone(),
        }
    }
}

#[derive(Clone)]
struct PairingState {
    config: AdvertiseConfig,
    identity: Arc<DeviceIdentity>,
    organization_proof: Option<OrganizationProof>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct SignedAdvertisement {
    schema_version: u32,
    service: &'static str,
    instance: String,
    device_id: String,
    public_key: String,
    nonce: String,
    timestamp_seconds: u64,
    display_name: String,
    device_kind: String,
    capabilities: Vec<String>,
    port: u16,
    path: String,
    signature: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    organization_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    organization_key_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    organization_proof: Option<String>,
    sources: Vec<ManifestSource>,
    #[serde(skip)]
    ttl_seconds: u32,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ManifestSource {
    id: String,
    protocol: String,
    status: &'static str,
}

async fn manifest(State(state): State<PairingState>) -> Response {
    match signed_advertisement(
        &state.config,
        &state.identity,
        state.organization_proof.as_ref(),
    ) {
        Ok(advertisement) => (StatusCode::OK, Json(advertisement)).into_response(),
        Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct ChallengeRequest {
    nonce: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ChallengeResponse {
    schema_version: u32,
    device_id: String,
    public_key: String,
    request_nonce: String,
    agent_nonce: String,
    timestamp_ms: i64,
    signature: String,
}

async fn challenge(
    State(state): State<PairingState>,
    Json(request): Json<ChallengeRequest>,
) -> Response {
    let Ok(decoded) = URL_SAFE_NO_PAD.decode(&request.nonce) else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    if !(16..=64).contains(&decoded.len()) || request.nonce.len() > 96 {
        return StatusCode::BAD_REQUEST.into_response();
    }
    let Ok(agent_nonce) = random_nonce() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let Ok(timestamp_ms) = unix_time_ms() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let canonical = format!(
        "{CHALLENGE_CONTEXT}\nid={}\nrequest_nonce={}\nagent_nonce={}\nts={timestamp_ms}",
        state.identity.public().device_id,
        request.nonce,
        agent_nonce,
    );
    Json(ChallengeResponse {
        schema_version: 1,
        device_id: state.identity.public().device_id.to_string(),
        public_key: state.identity.public().public_key.clone(),
        request_nonce: request.nonce,
        agent_nonce,
        timestamp_ms,
        signature: state.identity.sign_base64(canonical.as_bytes()),
    })
    .into_response()
}

async fn handle_service_error(_error: BoxError) -> Response {
    StatusCode::SERVICE_UNAVAILABLE.into_response()
}

fn signed_advertisement(
    config: &AdvertiseConfig,
    identity: &DeviceIdentity,
    organization: Option<&OrganizationProof>,
) -> Result<SignedAdvertisement, AdvertisementError> {
    let mut capabilities = config.capabilities.clone();
    capabilities.sort();
    let caps = capabilities.join(",");
    let device_id = identity.public().device_id.to_string();
    let instance = format!("{device_id}.{SERVICE_TYPE}");
    let nonce = random_nonce()?;
    let timestamp_seconds = unix_time_seconds()?;
    let canonical = canonical_advertisement(
        &instance,
        &device_id,
        &nonce,
        timestamp_seconds,
        config.service_port,
        &config.path,
        &config.device_kind,
        &caps,
        &config.display_name,
    );
    let signature = identity.sign_base64(canonical.as_bytes());
    let (organization_id, organization_key_id, organization_proof) =
        organization.map_or((None, None, None), |proof| {
            let proof_message = format!(
                "{canonical}\norg={}\norg_kid={}",
                proof.organization_id, proof.key_id
            );
            let tag = hmac::sign(&proof.key, proof_message.as_bytes());
            (
                Some(proof.organization_id.clone()),
                Some(proof.key_id.clone()),
                Some(URL_SAFE_NO_PAD.encode(tag.as_ref())),
            )
        });
    let sources = capabilities
        .iter()
        .filter_map(|capability| match capability.as_str() {
            "ros2_dds" => Some(ManifestSource {
                id: "ros2-graph".to_owned(),
                protocol: "ros2".to_owned(),
                status: "metadata_only",
            }),
            "mavlink" => Some(ManifestSource {
                id: "mavlink-telemetry".to_owned(),
                protocol: "mavlink".to_owned(),
                status: "metadata_only",
            }),
            _ => None,
        })
        .collect();
    Ok(SignedAdvertisement {
        schema_version: 1,
        service: SERVICE_TYPE,
        instance,
        device_id,
        public_key: identity.public().public_key.clone(),
        nonce,
        timestamp_seconds,
        display_name: config.display_name.clone(),
        device_kind: config.device_kind.clone(),
        capabilities,
        port: config.service_port,
        path: config.path.clone(),
        signature,
        organization_id,
        organization_key_id,
        organization_proof,
        sources,
        ttl_seconds: config.ttl_seconds,
    })
}

fn source_route(manifest_path: &str, suffix: &str) -> String {
    if manifest_path == "/" {
        format!("/{suffix}")
    } else {
        format!("{}/{suffix}", manifest_path.trim_end_matches('/'))
    }
}

fn canonical_advertisement(
    instance: &str,
    device_id: &str,
    nonce: &str,
    timestamp_seconds: u64,
    port: u16,
    path: &str,
    device_kind: &str,
    capabilities: &str,
    display_name: &str,
) -> String {
    format!(
        "{ADVERTISEMENT_CONTEXT}\nservice={SERVICE_TYPE}\ninstance={}\nid={device_id}\nnonce={nonce}\nts={timestamp_seconds}\nport={port}\npath={path}\nkind={device_kind}\ncaps={capabilities}\nname={display_name}",
        instance.to_ascii_lowercase(),
    )
}

fn mdns_socket(interface: Ipv4Addr) -> io::Result<UdpSocket> {
    let socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
    socket.set_reuse_address(true)?;
    socket.set_nonblocking(true)?;
    socket.bind(&SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, MDNS_PORT).into())?;
    socket.join_multicast_v4(&Ipv4Addr::new(224, 0, 0, 251), &interface)?;
    socket.set_multicast_ttl_v4(1)?;
    socket.set_multicast_loop_v4(false)?;
    UdpSocket::from_std(socket.into())
}

fn query_requests_rms(packet: &[u8]) -> Option<u16> {
    if packet.len() < 12 {
        return None;
    }
    let transaction_id = u16::from_be_bytes([packet[0], packet[1]]);
    let flags = u16::from_be_bytes([packet[2], packet[3]]);
    let questions = usize::from(u16::from_be_bytes([packet[4], packet[5]]));
    if flags & 0x8000 != 0 || questions == 0 || questions > MAX_DNS_QUESTIONS {
        return None;
    }
    let mut cursor = 12;
    let mut found = false;
    for _ in 0..questions {
        let name = read_dns_name(packet, &mut cursor)?;
        let record_type = read_u16(packet, &mut cursor)?;
        let _class = read_u16(packet, &mut cursor)?;
        if name.eq_ignore_ascii_case(SERVICE_TYPE) && matches!(record_type, 12 | 255) {
            found = true;
        }
    }
    found.then_some(transaction_id)
}

fn build_dns_response(
    transaction_id: u16,
    advertisement: &SignedAdvertisement,
) -> Result<Vec<u8>, AdvertisementError> {
    let mut packet = Vec::with_capacity(1_024);
    packet.extend_from_slice(&transaction_id.to_be_bytes());
    packet.extend_from_slice(&0x8400_u16.to_be_bytes());
    packet.extend_from_slice(&0_u16.to_be_bytes());
    packet.extend_from_slice(&3_u16.to_be_bytes());
    packet.extend_from_slice(&0_u16.to_be_bytes());
    packet.extend_from_slice(&0_u16.to_be_bytes());

    let service = encode_dns_name(SERVICE_TYPE)?;
    let instance = encode_dns_name(&advertisement.instance)?;
    append_record(&mut packet, &service, 12, 1, advertisement, &instance)?;

    let target = encode_dns_name(&format!("rms-edge-{}.local", &advertisement.device_id[..8]))?;
    let mut srv = vec![0_u8; 4];
    srv.extend_from_slice(&advertisement.port.to_be_bytes());
    srv.extend_from_slice(&target);
    append_record(&mut packet, &instance, 33, 0x8001, advertisement, &srv)?;

    let txt = advertisement_txt(advertisement)?;
    append_record(&mut packet, &instance, 16, 0x8001, advertisement, &txt)?;
    if packet.len() > 1_400 {
        return Err(AdvertisementError::new(
            "advertisement exceeds DNS packet limit",
        ));
    }
    Ok(packet)
}

fn append_record(
    packet: &mut Vec<u8>,
    name: &[u8],
    record_type: u16,
    class: u16,
    advertisement: &SignedAdvertisement,
    data: &[u8],
) -> Result<(), AdvertisementError> {
    let data_length = u16::try_from(data.len())
        .map_err(|_error| AdvertisementError::new("DNS record exceeds length limit"))?;
    packet.extend_from_slice(name);
    packet.extend_from_slice(&record_type.to_be_bytes());
    packet.extend_from_slice(&class.to_be_bytes());
    packet.extend_from_slice(&advertisement_ttl(advertisement).to_be_bytes());
    packet.extend_from_slice(&data_length.to_be_bytes());
    packet.extend_from_slice(data);
    Ok(())
}

fn advertisement_ttl(advertisement: &SignedAdvertisement) -> u32 {
    // TTL is validated in configuration. It is not signed because expiry is transport policy.
    advertisement.ttl_seconds
}

fn advertisement_txt(advertisement: &SignedAdvertisement) -> Result<Vec<u8>, AdvertisementError> {
    let mut values = vec![
        "v=1".to_owned(),
        format!("id={}", advertisement.device_id),
        format!("pk={}", advertisement.public_key),
        format!("nonce={}", advertisement.nonce),
        format!("ts={}", advertisement.timestamp_seconds),
        format!("name={}", advertisement.display_name),
        format!("kind={}", advertisement.device_kind),
        format!("caps={}", advertisement.capabilities.join(",")),
        format!("path={}", advertisement.path),
        format!("sig={}", advertisement.signature),
    ];
    if let Some(organization_id) = &advertisement.organization_id {
        values.push(format!("org={organization_id}"));
    }
    if let Some(key_id) = &advertisement.organization_key_id {
        values.push(format!("org_kid={key_id}"));
    }
    if let Some(proof) = &advertisement.organization_proof {
        values.push(format!("org_proof={proof}"));
    }
    let mut encoded = Vec::with_capacity(768);
    for value in values {
        let length = u8::try_from(value.len())
            .map_err(|_error| AdvertisementError::new("DNS TXT value exceeds length limit"))?;
        encoded.push(length);
        encoded.extend_from_slice(value.as_bytes());
    }
    Ok(encoded)
}

fn encode_dns_name(name: &str) -> Result<Vec<u8>, AdvertisementError> {
    let mut encoded = Vec::with_capacity(name.len() + 2);
    for label in name.trim_end_matches('.').split('.') {
        if label.is_empty() || label.len() > 63 {
            return Err(AdvertisementError::new("invalid DNS label"));
        }
        encoded.push(
            u8::try_from(label.len()).map_err(|_error| AdvertisementError::new("DNS label"))?,
        );
        encoded.extend_from_slice(label.as_bytes());
    }
    encoded.push(0);
    Ok(encoded)
}

fn read_dns_name(packet: &[u8], cursor: &mut usize) -> Option<String> {
    let mut position = *cursor;
    let mut jumped = false;
    let mut jumps = 0;
    let mut labels = Vec::new();
    loop {
        let length = *packet.get(position)?;
        if length & 0xc0 == 0xc0 {
            let next = *packet.get(position + 1)?;
            let pointer = usize::from((u16::from(length & 0x3f) << 8) | u16::from(next));
            if !jumped {
                *cursor = position + 2;
            }
            position = pointer;
            jumped = true;
            jumps += 1;
            if jumps > MAX_POINTER_JUMPS {
                return None;
            }
            continue;
        }
        if length == 0 {
            if !jumped {
                *cursor = position + 1;
            }
            break;
        }
        if length > 63 || labels.len() >= MAX_DNS_LABELS {
            return None;
        }
        let start = position + 1;
        let end = start.checked_add(usize::from(length))?;
        labels.push(std::str::from_utf8(packet.get(start..end)?).ok()?);
        position = end;
    }
    Some(labels.join("."))
}

fn read_u16(packet: &[u8], cursor: &mut usize) -> Option<u16> {
    let bytes = packet.get(*cursor..cursor.checked_add(2)?)?;
    *cursor += 2;
    Some(u16::from_be_bytes([bytes[0], bytes[1]]))
}

fn random_nonce() -> Result<String, AdvertisementError> {
    let mut nonce = [0_u8; 16];
    getrandom::fill(&mut nonce)
        .map_err(|error| AdvertisementError::new(format!("random generation failed: {error}")))?;
    Ok(URL_SAFE_NO_PAD.encode(nonce))
}

fn unix_time_seconds() -> Result<u64, AdvertisementError> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .map_err(|_error| AdvertisementError::new("system clock precedes Unix epoch"))
}

fn unix_time_ms() -> Result<i64, AdvertisementError> {
    let milliseconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_error| AdvertisementError::new("system clock precedes Unix epoch"))?
        .as_millis();
    i64::try_from(milliseconds).map_err(|_error| AdvertisementError::new("system clock overflow"))
}

fn is_lan_request_source(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(address) => {
            address.is_private() || address.is_link_local() || address.is_loopback()
        }
        IpAddr::V6(address) => {
            address.is_unicast_link_local()
                || address.is_loopback()
                || address.octets()[0] & 0xfe == 0xfc
        }
    }
}

/// Advertisement service error.
#[derive(Debug)]
pub struct AdvertisementError {
    message: String,
}

impl AdvertisementError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }

    fn io(error: impl fmt::Display) -> Self {
        Self::new(format!("advertisement I/O failed: {error}"))
    }
}

impl fmt::Display for AdvertisementError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for AdvertisementError {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AdvertiseConfig;
    use ring::signature::{ED25519, UnparsedPublicKey};

    fn query() -> Vec<u8> {
        let mut packet = vec![0_u8; 12];
        packet[5] = 1;
        packet.extend_from_slice(&encode_dns_name(SERVICE_TYPE).expect("service encodes"));
        packet.extend_from_slice(&12_u16.to_be_bytes());
        packet.extend_from_slice(&0x8001_u16.to_be_bytes());
        packet
    }

    #[test]
    fn recognizes_only_rms_dns_sd_question() {
        assert_eq!(query_requests_rms(&query()), Some(0));
        let mut malformed = query();
        malformed.truncate(13);
        assert_eq!(query_requests_rms(&malformed), None);
    }

    #[test]
    fn signed_response_is_bounded_and_contains_three_records() {
        let root = tempfile::tempdir().expect("temporary directory");
        let identity = DeviceIdentity::load_or_create(&root.path().join("identity.json"))
            .expect("identity is created");
        let config = AdvertiseConfig {
            enabled: true,
            display_name: "Edge Gateway".to_owned(),
            device_kind: "gateway".to_owned(),
            bind_ip: "192.168.1.2".parse().expect("test address"),
            service_port: 9_879,
            path: "/rms/v1/manifest".to_owned(),
            ttl_seconds: 30,
            capabilities: vec!["ros2_dds".to_owned(), "mavlink".to_owned()],
            organization_id: None,
            organization_key_id: None,
            organization_psk_path: None,
        };
        let advertisement =
            signed_advertisement(&config, &identity, None).expect("advertisement signs");
        let response = build_dns_response(7, &advertisement).expect("response builds");
        assert!(response.len() < 1_400);
        assert_eq!(u16::from_be_bytes([response[6], response[7]]), 3);
        assert!(response.windows(3).any(|window| window == b"v=1"));
    }

    #[test]
    fn organization_proof_is_bound_to_the_organization_and_advertisement() {
        let root = tempfile::tempdir().expect("temporary directory");
        let identity = DeviceIdentity::load_or_create(&root.path().join("identity.json"))
            .expect("identity is created");
        let config = AdvertiseConfig {
            enabled: true,
            display_name: "Edge Gateway".to_owned(),
            device_kind: "gateway".to_owned(),
            bind_ip: "192.168.1.2".parse().expect("test address"),
            service_port: 9_879,
            path: "/rms/v1/manifest".to_owned(),
            ttl_seconds: 30,
            capabilities: vec!["ros2_dds".to_owned()],
            organization_id: Some("org-rms".to_owned()),
            organization_key_id: Some("fleet-a".to_owned()),
            organization_psk_path: Some(root.path().join("unused.psk")),
        };
        let key_bytes = [7_u8; 32];
        let proof = OrganizationProof {
            organization_id: "org-rms".to_owned(),
            key_id: "fleet-a".to_owned(),
            key: hmac::Key::new(hmac::HMAC_SHA256, &key_bytes),
        };
        let advertisement =
            signed_advertisement(&config, &identity, Some(&proof)).expect("advertisement signs");
        let canonical = canonical_advertisement(
            &advertisement.instance,
            &advertisement.device_id,
            &advertisement.nonce,
            advertisement.timestamp_seconds,
            advertisement.port,
            &advertisement.path,
            &advertisement.device_kind,
            &advertisement.capabilities.join(","),
            &advertisement.display_name,
        );
        let proof_message = format!("{canonical}\norg=org-rms\norg_kid=fleet-a");
        let proof_bytes = URL_SAFE_NO_PAD
            .decode(advertisement.organization_proof.expect("proof exists"))
            .expect("proof decodes");
        hmac::verify(
            &hmac::Key::new(hmac::HMAC_SHA256, &key_bytes),
            proof_message.as_bytes(),
            &proof_bytes,
        )
        .expect("organization proof verifies");
    }

    #[tokio::test]
    async fn pairing_endpoint_signs_a_bounded_caller_challenge() {
        let root = tempfile::tempdir().expect("temporary directory");
        let identity = Arc::new(
            DeviceIdentity::load_or_create(&root.path().join("identity.json"))
                .expect("identity is created"),
        );
        let probe = std::net::TcpListener::bind("127.0.0.1:0").expect("test port binds");
        let port = probe.local_addr().expect("test address exists").port();
        drop(probe);
        let config = AdvertiseConfig {
            enabled: true,
            display_name: "Edge Gateway".to_owned(),
            device_kind: "gateway".to_owned(),
            bind_ip: "127.0.0.1".parse().expect("test address"),
            service_port: port,
            path: "/rms/v1/manifest".to_owned(),
            ttl_seconds: 30,
            capabilities: vec!["ros2_dds".to_owned()],
            organization_id: None,
            organization_key_id: None,
            organization_psk_path: None,
        };
        let service = Arc::new(
            AdvertisementService::new(
                config,
                Arc::clone(&identity),
                Arc::new(AgentMetrics::default()),
            )
            .expect("advertisement service is configured"),
        );
        let cancellation = CancellationToken::new();
        let service_cancellation = cancellation.clone();
        let task_service = Arc::clone(&service);
        let task = tokio::spawn(async move {
            task_service
                .run_pairing_endpoint(service_cancellation)
                .await
        });
        let client = reqwest::Client::new();
        let manifest_url = format!("http://127.0.0.1:{port}/rms/v1/manifest");
        let mut ready = false;
        for _ in 0..40 {
            if client.get(&manifest_url).send().await.is_ok() {
                ready = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        assert!(ready, "pairing endpoint did not start");
        let source_manifest = client
            .get(format!("{manifest_url}/ros2"))
            .send()
            .await
            .expect("source manifest request completes");
        assert_eq!(source_manifest.status(), StatusCode::OK);
        let request_nonce = URL_SAFE_NO_PAD.encode([9_u8; 16]);
        let response = client
            .post(format!("http://127.0.0.1:{port}{CHALLENGE_PATH}"))
            .json(&serde_json::json!({ "nonce": request_nonce }))
            .send()
            .await
            .expect("challenge request completes");
        assert_eq!(response.status(), StatusCode::OK);
        let response: serde_json::Value = response.json().await.expect("response is JSON");
        let agent_nonce = response["agentNonce"]
            .as_str()
            .expect("agent nonce is present");
        let timestamp = response["timestampMs"]
            .as_i64()
            .expect("timestamp is present");
        let canonical = format!(
            "{CHALLENGE_CONTEXT}\nid={}\nrequest_nonce={}\nagent_nonce={}\nts={timestamp}",
            identity.public().device_id,
            request_nonce,
            agent_nonce,
        );
        let public_key = URL_SAFE_NO_PAD
            .decode(
                response["publicKey"]
                    .as_str()
                    .expect("public key is present"),
            )
            .expect("public key decodes");
        let signature = URL_SAFE_NO_PAD
            .decode(
                response["signature"]
                    .as_str()
                    .expect("signature is present"),
            )
            .expect("signature decodes");
        UnparsedPublicKey::new(&ED25519, public_key)
            .verify(canonical.as_bytes(), &signature)
            .expect("challenge signature verifies");
        cancellation.cancel();
        tokio::time::timeout(Duration::from_secs(2), task)
            .await
            .expect("pairing endpoint shuts down")
            .expect("pairing task joins")
            .expect("pairing endpoint exits successfully");
    }
}
