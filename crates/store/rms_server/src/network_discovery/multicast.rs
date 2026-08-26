//! Bounded, user-initiated LAN discovery over multicast UDP.
//!
//! Advertised host names and IP addresses are never trusted.
//! Every endpoint is pinned to the source address of the UDP datagram that described it.

use std::{
    collections::BTreeMap,
    env,
    fmt::Write as _,
    fs, io,
    net::{IpAddr, Ipv4Addr, SocketAddr, SocketAddrV4},
    path::PathBuf,
    sync::Arc,
    time::Duration,
    time::{SystemTime, UNIX_EPOCH},
};

use async_trait::async_trait;
use sha2::{Digest as _, Sha256};
use tokio::{net::UdpSocket, sync::Mutex, time::Instant};

use super::{
    DiscoveredSource, DiscoveryCancellation, DiscoveryObservation, DiscoveryProvider,
    DiscoveryProviderError, EdgeObservationIdentity, PinnedDiscoveryEndpoint, ProviderVerification,
    ProviderVerificationStatus,
    edge_advertisement::{self, EdgeCapability, EdgeOrganizationProof},
    same_source_pin,
};
use crate::domain::{DiscoveryCandidateCategory, DiscoverySourceCategory, DiscoverySourceStatus};

const DEFAULT_SCAN_TIMEOUT: Duration = Duration::from_millis(1_500);
const CANCELLATION_POLL_INTERVAL: Duration = Duration::from_millis(20);
const MAX_PACKET_SIZE: usize = 16 * 1024;
const MAX_CANDIDATES_PER_SCAN: usize = 64;
const MAX_DNS_QUESTIONS: usize = 16;
const MAX_DNS_RECORDS: usize = 128;
const MAX_DNS_LABELS: usize = 32;
const MAX_DNS_POINTER_JUMPS: usize = 16;
const MAX_HTTP_HEADERS: usize = 64;
const MAX_HEADER_LINE_SIZE: usize = 1_024;
const MAX_XML_VALUES: usize = 16;
const MAX_XML_TAG_SIZE: usize = 512;
const MIN_ADVERTISEMENT_TTL_SECS: u64 = 5;
const MAX_ADVERTISEMENT_TTL_SECS: u64 = 120;
const MAX_EDGE_NONCES: usize = 2_048;

type EdgeNonceKey = (String, [u8; 16]);
type EdgeNonceCache = Arc<Mutex<BTreeMap<EdgeNonceKey, i64>>>;

const MDNS_DESTINATION: SocketAddr =
    SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::new(224, 0, 0, 251), 5353));
const SSDP_DESTINATION: SocketAddr =
    SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::new(239, 255, 255, 250), 1900));
const WS_DISCOVERY_DESTINATION: SocketAddr =
    SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::new(239, 255, 255, 250), 3702));

const SSDP_QUERY: &[u8] = b"M-SEARCH * HTTP/1.1\r\n\
HOST: 239.255.255.250:1900\r\n\
MAN: \"ssdp:discover\"\r\n\
MX: 1\r\n\
ST: ssdp:all\r\n\
USER-AGENT: Rerun-RMS/1.0 UPnP/1.1\r\n\
\r\n";

#[derive(Clone)]
pub(crate) struct MulticastDiscoveryProvider {
    scan_timeout: Duration,
    max_candidates: usize,
    organization_proof: Option<EdgeOrganizationProofConfig>,
    edge_nonces: EdgeNonceCache,
}

#[derive(Clone)]
struct EdgeOrganizationProofConfig {
    organization_id: String,
    key_id: String,
    secret: Vec<u8>,
}

impl Default for MulticastDiscoveryProvider {
    fn default() -> Self {
        Self {
            scan_timeout: DEFAULT_SCAN_TIMEOUT,
            max_candidates: MAX_CANDIDATES_PER_SCAN,
            organization_proof: None,
            edge_nonces: Arc::new(Mutex::new(BTreeMap::new())),
        }
    }
}

impl MulticastDiscoveryProvider {
    pub(crate) fn from_environment() -> io::Result<Self> {
        let organization_id = env::var("RMS_EDGE_DISCOVERY_ORGANIZATION_ID").ok();
        let key_id = env::var("RMS_EDGE_DISCOVERY_KID").ok();
        let secret_path = env::var_os("RMS_EDGE_DISCOVERY_PSK_FILE").map(PathBuf::from);
        let organization_proof = match (organization_id, key_id, secret_path) {
            (None, None, None) => None,
            (Some(organization_id), Some(key_id), Some(secret_path)) => {
                if !valid_organization_token(&organization_id, 64)
                    || !valid_organization_token(&key_id, 32)
                    || !secret_path.is_absolute()
                {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "RMS Edge discovery organization proof configuration is invalid.",
                    ));
                }
                let metadata = fs::metadata(&secret_path)?;
                if !metadata.is_file() {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "RMS Edge discovery PSK must be a regular file.",
                    ));
                }
                validate_private_secret_permissions(&metadata)?;
                let mut secret = fs::read(secret_path)?;
                while secret.last().is_some_and(u8::is_ascii_whitespace) {
                    secret.pop();
                }
                if secret.len() < 32 {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "RMS Edge discovery PSK must contain at least 32 bytes.",
                    ));
                }
                Some(EdgeOrganizationProofConfig {
                    organization_id,
                    key_id,
                    secret,
                })
            }
            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "RMS_EDGE_DISCOVERY_ORGANIZATION_ID, RMS_EDGE_DISCOVERY_KID, and RMS_EDGE_DISCOVERY_PSK_FILE must be configured together.",
                ));
            }
        };
        Ok(Self {
            organization_proof,
            ..Self::default()
        })
    }

    fn organization_proof(&self) -> Option<EdgeOrganizationProof<'_>> {
        self.organization_proof
            .as_ref()
            .map(|proof| EdgeOrganizationProof {
                organization_id: &proof.organization_id,
                key_id: &proof.key_id,
                secret: &proof.secret,
            })
    }

    async fn reject_replayed_edge_nonces(
        &self,
        observations: Vec<DiscoveryObservation>,
    ) -> Vec<DiscoveryObservation> {
        let now_seconds = unix_now_seconds();
        let mut nonces = self.edge_nonces.lock().await;
        nonces.retain(|_, expires_at| *expires_at >= now_seconds);
        let mut accepted = Vec::with_capacity(observations.len());
        for observation in observations {
            let Some(identity) = &observation.edge_identity else {
                accepted.push(observation);
                continue;
            };
            let key = (identity.device_id.clone(), identity.nonce);
            if nonces.contains_key(&key) {
                continue;
            }
            if nonces.len() >= MAX_EDGE_NONCES
                && let Some(oldest) = nonces
                    .iter()
                    .min_by_key(|(_, expires_at)| **expires_at)
                    .map(|(key, _)| key.clone())
            {
                nonces.remove(&oldest);
            }
            nonces.insert(
                key,
                identity
                    .timestamp_seconds
                    .saturating_add(MAX_ADVERTISEMENT_TTL_SECS.cast_signed()),
            );
            accepted.push(observation);
        }
        accepted
    }
}

#[async_trait]
impl DiscoveryProvider for MulticastDiscoveryProvider {
    async fn discover(
        &self,
        cancellation: DiscoveryCancellation,
    ) -> Result<Vec<DiscoveryObservation>, DiscoveryProviderError> {
        if cancellation.is_cancelled() {
            return Ok(Vec::new());
        }

        let deadline = Instant::now() + self.scan_timeout;
        let mdns_query = mdns_query();
        let ws_discovery_query = ws_discovery_query();
        let organization_proof = self.organization_proof();
        let (mdns, ssdp, ws_discovery) = tokio::join!(
            scan_protocol(
                WireProtocol::Mdns,
                &mdns_query,
                MDNS_DESTINATION,
                deadline,
                cancellation.clone(),
                self.max_candidates,
                organization_proof,
            ),
            scan_protocol(
                WireProtocol::Ssdp,
                SSDP_QUERY,
                SSDP_DESTINATION,
                deadline,
                cancellation.clone(),
                self.max_candidates,
                None,
            ),
            scan_protocol(
                WireProtocol::WsDiscovery,
                ws_discovery_query.as_bytes(),
                WS_DISCOVERY_DESTINATION,
                deadline,
                cancellation.clone(),
                self.max_candidates,
                None,
            ),
        );

        if cancellation.is_cancelled() {
            return Ok(Vec::new());
        }

        let mut observations = Vec::new();
        let mut successful_protocols = 0;
        let mut errors = Vec::new();
        for (protocol, result) in [
            ("mDNS", mdns),
            ("SSDP", ssdp),
            ("ONVIF WS-Discovery", ws_discovery),
        ] {
            match result {
                Ok(mut protocol_observations) => {
                    successful_protocols += 1;
                    observations.append(&mut protocol_observations);
                }
                Err(err) => errors.push(format!("{protocol}: {err}")),
            }
        }

        if successful_protocols == 0 {
            return Err(DiscoveryProviderError::new(format!(
                "Multicast discovery failed: {}",
                errors.join("; ")
            )));
        }

        Ok(self
            .reject_replayed_edge_nonces(deduplicate_and_bound(observations, self.max_candidates))
            .await)
    }

    async fn verify(
        &self,
        candidate: &DiscoveryObservation,
        cancellation: DiscoveryCancellation,
    ) -> Result<ProviderVerification, DiscoveryProviderError> {
        if cancellation.is_cancelled() {
            return Ok(unavailable_verification());
        }

        let observations = self.discover(cancellation.clone()).await?;
        if cancellation.is_cancelled() {
            return Ok(unavailable_verification());
        }

        if let Some(observation) = observations.iter().find(|observation| {
            observation.fingerprint == candidate.fingerprint
                && same_source_pin(observation, candidate)
                && edge_identity_continuity(observation, candidate)
        }) {
            return Ok(ProviderVerification {
                status: ProviderVerificationStatus::Verified,
                observation: Some(observation.clone()),
            });
        }

        let fingerprint_seen = observations
            .iter()
            .any(|observation| observation.fingerprint == candidate.fingerprint);
        Ok(ProviderVerification {
            status: if fingerprint_seen {
                ProviderVerificationStatus::Incompatible
            } else {
                ProviderVerificationStatus::Unavailable
            },
            observation: None,
        })
    }
}

fn unavailable_verification() -> ProviderVerification {
    ProviderVerification {
        status: ProviderVerificationStatus::Unavailable,
        observation: None,
    }
}

#[derive(Clone, Copy)]
enum WireProtocol {
    Mdns,
    Ssdp,
    WsDiscovery,
}

async fn scan_protocol(
    protocol: WireProtocol,
    query: &[u8],
    destination: SocketAddr,
    deadline: Instant,
    cancellation: DiscoveryCancellation,
    max_candidates: usize,
    organization_proof: Option<EdgeOrganizationProof<'_>>,
) -> io::Result<Vec<DiscoveryObservation>> {
    if cancellation.is_cancelled() {
        return Ok(Vec::new());
    }

    let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).await?;
    socket.set_multicast_ttl_v4(1)?;
    socket.set_multicast_loop_v4(false)?;
    socket.send_to(query, destination).await?;

    let mut observations = Vec::new();
    let mut packet = vec![0_u8; MAX_PACKET_SIZE + 1];
    while Instant::now() < deadline
        && !cancellation.is_cancelled()
        && observations.len() < max_candidates
    {
        let wake_at = (Instant::now() + CANCELLATION_POLL_INTERVAL).min(deadline);
        tokio::select! {
            result = socket.recv_from(&mut packet) => {
                let (packet_size, source) = result?;
                if !is_usable_source_address(source.ip()) {
                    continue;
                }
                let parsed = match protocol {
                    WireProtocol::Mdns => parse_mdns_packet(
                        &packet[..packet_size],
                        organization_proof,
                        unix_now_seconds(),
                    ),
                    WireProtocol::Ssdp => parse_ssdp_packet(&packet[..packet_size]),
                    WireProtocol::WsDiscovery => parse_ws_discovery_packet(&packet[..packet_size]),
                };
                let Ok(advertisements) = parsed else {
                    continue;
                };
                for advertisement in advertisements {
                    if observations.len() >= max_candidates {
                        break;
                    }
                    observations.push(advertisement.into_observation(source));
                }
            }
            () = tokio::time::sleep_until(wake_at) => {}
        }
    }

    Ok(deduplicate_and_bound(observations, max_candidates))
}

fn is_usable_source_address(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(address) => address.is_private() || address.is_link_local(),
        IpAddr::V6(address) => address.is_unique_local() || address.is_unicast_link_local(),
    }
}

fn deduplicate_and_bound(
    mut observations: Vec<DiscoveryObservation>,
    max_candidates: usize,
) -> Vec<DiscoveryObservation> {
    observations.sort_by(|left, right| {
        left.fingerprint
            .cmp(&right.fingerprint)
            .then_with(|| first_source_pin(left).cmp(&first_source_pin(right)))
    });

    let mut deduplicated: Vec<DiscoveryObservation> = Vec::new();
    for observation in observations {
        if let Some(existing) = deduplicated.iter_mut().find(|existing| {
            existing.fingerprint == observation.fingerprint
                && same_source_pin(existing, &observation)
        }) {
            existing.last_seen_at_ms = existing.last_seen_at_ms.max(observation.last_seen_at_ms);
            existing.expires_at_ms = existing.expires_at_ms.max(observation.expires_at_ms);
        } else if deduplicated.len() < max_candidates {
            deduplicated.push(observation);
        }
    }
    deduplicated
}

fn first_source_pin(observation: &DiscoveryObservation) -> Option<(IpAddr, u16)> {
    observation
        .sources
        .first()
        .map(|source| (source.endpoint.source_address, source.endpoint.port))
}

struct ParsedAdvertisement {
    fingerprint_namespace: &'static str,
    identity: String,
    display_name: String,
    category: DiscoveryCandidateCategory,
    suggested_device_kind: &'static str,
    integration_kind: &'static str,
    endpoint_label: String,
    source_id: &'static str,
    source_label: String,
    source_category: DiscoverySourceCategory,
    protocol: &'static str,
    endpoint_scheme: String,
    endpoint_port: u16,
    endpoint_path: String,
    ttl_seconds: u64,
    additional_sources: Vec<ParsedSourceAdvertisement>,
    edge_identity: Option<EdgeObservationIdentity>,
}

struct ParsedSourceAdvertisement {
    id: &'static str,
    label: &'static str,
    category: DiscoverySourceCategory,
    protocol: &'static str,
    scheme: &'static str,
    port: u16,
    path: String,
}

impl ParsedAdvertisement {
    fn into_observation(self, packet_source: SocketAddr) -> DiscoveryObservation {
        let now_ms = crate::state::now_ms();
        let ttl_ms = i64::try_from(
            self.ttl_seconds
                .clamp(MIN_ADVERTISEMENT_TTL_SECS, MAX_ADVERTISEMENT_TTL_SECS)
                .saturating_mul(1_000),
        )
        .unwrap_or(i64::MAX);
        let mut sources = vec![DiscoveredSource {
            id: self.source_id.to_owned(),
            label: sanitize_label(&self.source_label, "Discovered source"),
            category: self.source_category,
            status: DiscoverySourceStatus::Ready,
            protocol: self.protocol.to_owned(),
            endpoint: PinnedDiscoveryEndpoint {
                source_address: packet_source.ip(),
                port: self.endpoint_port,
                scheme: self.endpoint_scheme,
                path: self.endpoint_path,
            },
        }];
        sources.extend(
            self.additional_sources
                .into_iter()
                .map(|source| DiscoveredSource {
                    id: source.id.to_owned(),
                    label: source.label.to_owned(),
                    category: source.category,
                    status: DiscoverySourceStatus::Ready,
                    protocol: source.protocol.to_owned(),
                    endpoint: PinnedDiscoveryEndpoint {
                        source_address: packet_source.ip(),
                        port: source.port,
                        scheme: source.scheme.to_owned(),
                        path: source.path,
                    },
                }),
        );
        DiscoveryObservation {
            fingerprint: stable_fingerprint(self.fingerprint_namespace, &self.identity),
            display_name: sanitize_label(&self.display_name, "LAN device"),
            category: self.category,
            suggested_device_kind: self.suggested_device_kind.to_owned(),
            integration_kind: self.integration_kind.to_owned(),
            endpoint_label: sanitize_label(&self.endpoint_label, "LAN endpoint"),
            last_seen_at_ms: now_ms,
            expires_at_ms: now_ms.saturating_add(ttl_ms),
            sources,
            edge_identity: self.edge_identity,
        }
    }
}

fn edge_identity_continuity(
    current: &DiscoveryObservation,
    previous: &DiscoveryObservation,
) -> bool {
    match (&current.edge_identity, &previous.edge_identity) {
        (None, None) => true,
        (Some(current), Some(previous)) => {
            current.device_id == previous.device_id
                && current.public_key == previous.public_key
                && current.nonce != previous.nonce
                && current.organization_id == previous.organization_id
                && (!previous.organization_trusted || current.organization_trusted)
        }
        _ => false,
    }
}

fn unix_now_seconds() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| {
            i64::try_from(duration.as_secs()).unwrap_or(i64::MAX)
        })
}

fn valid_organization_token(value: &str, maximum: usize) -> bool {
    !value.is_empty()
        && value.len() <= maximum
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

#[cfg(unix)]
fn validate_private_secret_permissions(metadata: &fs::Metadata) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt as _;

    if metadata.permissions().mode() & 0o077 != 0 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "RMS Edge discovery PSK must not grant group or other access.",
        ));
    }
    Ok(())
}

#[cfg(not(unix))]
#[expect(
    clippy::unnecessary_wraps,
    reason = "the platform-specific validators share a fallible call-site contract"
)]
fn validate_private_secret_permissions(_metadata: &fs::Metadata) -> io::Result<()> {
    Ok(())
}

fn stable_fingerprint(namespace: &str, identity: &str) -> String {
    let digest = Sha256::digest([namespace.as_bytes(), b"\0", identity.as_bytes()].concat());
    let mut encoded = String::with_capacity(digest.len() * 2);
    for byte in digest {
        write!(&mut encoded, "{byte:02x}").expect("writing to a String cannot fail");
    }
    format!("multicast:{namespace}:{encoded}")
}

#[derive(Clone, Copy)]
struct MdnsServiceProfile {
    service_type: &'static str,
    category: DiscoveryCandidateCategory,
    suggested_device_kind: &'static str,
    integration_kind: &'static str,
    endpoint_label: &'static str,
    source_id: &'static str,
    source_label: &'static str,
    source_category: DiscoverySourceCategory,
    protocol: &'static str,
    scheme: &'static str,
}

const MDNS_SERVICE_PROFILES: [MdnsServiceProfile; 3] = [
    MdnsServiceProfile {
        service_type: "_rms._tcp.local",
        category: DiscoveryCandidateCategory::Robot,
        suggested_device_kind: "robot",
        integration_kind: "rerun",
        endpoint_label: "RMS endpoint",
        source_id: "telemetry",
        source_label: "RMS 데이터",
        source_category: DiscoverySourceCategory::Telemetry,
        protocol: "rerun",
        scheme: "rerun+http",
    },
    MdnsServiceProfile {
        service_type: "_rerun._tcp.local",
        category: DiscoveryCandidateCategory::Robot,
        suggested_device_kind: "robot",
        integration_kind: "rerun",
        endpoint_label: "Rerun endpoint",
        source_id: "telemetry",
        source_label: "Rerun 데이터",
        source_category: DiscoverySourceCategory::Telemetry,
        protocol: "rerun",
        scheme: "rerun+http",
    },
    MdnsServiceProfile {
        service_type: "_rtsp._tcp.local",
        category: DiscoveryCandidateCategory::Camera,
        suggested_device_kind: "camera",
        integration_kind: "rtsp",
        endpoint_label: "RTSP camera",
        source_id: "camera",
        source_label: "카메라 영상",
        source_category: DiscoverySourceCategory::Camera,
        protocol: "rtsp",
        scheme: "rtsp",
    },
];

fn mdns_query() -> Vec<u8> {
    let mut packet = Vec::with_capacity(128);
    packet.extend_from_slice(&0_u16.to_be_bytes());
    packet.extend_from_slice(&0_u16.to_be_bytes());
    packet.extend_from_slice(&(MDNS_SERVICE_PROFILES.len() as u16).to_be_bytes());
    packet.extend_from_slice(&0_u16.to_be_bytes());
    packet.extend_from_slice(&0_u16.to_be_bytes());
    packet.extend_from_slice(&0_u16.to_be_bytes());
    for profile in MDNS_SERVICE_PROFILES {
        for label in profile.service_type.split('.') {
            packet.push(label.len() as u8);
            packet.extend_from_slice(label.as_bytes());
        }
        packet.push(0);
        packet.extend_from_slice(&12_u16.to_be_bytes());
        // The top bit asks responders to send a unicast answer to our ephemeral source port.
        packet.extend_from_slice(&0x8001_u16.to_be_bytes());
    }
    packet
}

#[derive(Debug)]
struct PacketParseError;

impl From<std::str::Utf8Error> for PacketParseError {
    fn from(_error: std::str::Utf8Error) -> Self {
        Self
    }
}

type PacketParseResult<T> = Result<T, PacketParseError>;

#[derive(Debug)]
struct DnsRecord {
    name: String,
    ttl_seconds: u64,
    data: DnsRecordData,
}

#[derive(Debug)]
enum DnsRecordData {
    Ptr(String),
    Srv { port: u16 },
    Txt(BTreeMap<String, String>),
    Other,
}

fn parse_mdns_packet(
    packet: &[u8],
    organization_proof: Option<EdgeOrganizationProof<'_>>,
    now_seconds: i64,
) -> PacketParseResult<Vec<ParsedAdvertisement>> {
    ensure_packet_size(packet)?;
    if packet.len() < 12 {
        return Err(PacketParseError);
    }

    let mut cursor = 0;
    let _transaction_id = read_u16(packet, &mut cursor)?;
    let flags = read_u16(packet, &mut cursor)?;
    let question_count = usize::from(read_u16(packet, &mut cursor)?);
    let answer_count = usize::from(read_u16(packet, &mut cursor)?);
    let authority_count = usize::from(read_u16(packet, &mut cursor)?);
    let additional_count = usize::from(read_u16(packet, &mut cursor)?);

    if flags & 0x8000 == 0 {
        return Ok(Vec::new());
    }
    if flags & 0x780f != 0 {
        return Err(PacketParseError);
    }
    if question_count > MAX_DNS_QUESTIONS
        || answer_count
            .saturating_add(authority_count)
            .saturating_add(additional_count)
            > MAX_DNS_RECORDS
    {
        return Err(PacketParseError);
    }

    for _ in 0..question_count {
        read_dns_name(packet, &mut cursor)?;
        advance(&mut cursor, 4, packet.len())?;
    }

    let record_count = answer_count
        .saturating_add(authority_count)
        .saturating_add(additional_count);
    let mut records = Vec::with_capacity(record_count);
    for _ in 0..record_count {
        let name = normalize_dns_name(&read_dns_name(packet, &mut cursor)?);
        let record_type = read_u16(packet, &mut cursor)?;
        let _class = read_u16(packet, &mut cursor)?;
        let ttl_seconds = u64::from(read_u32(packet, &mut cursor)?);
        let data_length = usize::from(read_u16(packet, &mut cursor)?);
        let data_start = cursor;
        let data_end = data_start
            .checked_add(data_length)
            .filter(|end| *end <= packet.len())
            .ok_or(PacketParseError)?;

        let data = match record_type {
            12 => {
                let mut data_cursor = data_start;
                let target = normalize_dns_name(&read_dns_name(packet, &mut data_cursor)?);
                if data_cursor > data_end {
                    return Err(PacketParseError);
                }
                DnsRecordData::Ptr(target)
            }
            16 => DnsRecordData::Txt(parse_dns_txt(&packet[data_start..data_end])?),
            33 => {
                if data_length < 7 {
                    return Err(PacketParseError);
                }
                let port = u16::from_be_bytes([packet[data_start + 4], packet[data_start + 5]]);
                let mut target_cursor = data_start + 6;
                read_dns_name(packet, &mut target_cursor)?;
                if target_cursor > data_end {
                    return Err(PacketParseError);
                }
                DnsRecordData::Srv { port }
            }
            _ => DnsRecordData::Other,
        };
        cursor = data_end;
        records.push(DnsRecord {
            name,
            ttl_seconds,
            data,
        });
    }

    let mut ptr_profiles = BTreeMap::new();
    for record in &records {
        let DnsRecordData::Ptr(instance) = &record.data else {
            continue;
        };
        if let Some(profile) = mdns_profile_for_service(&record.name) {
            ptr_profiles.insert(instance.clone(), (profile, record.ttl_seconds));
        }
    }

    let mut advertisements = Vec::new();
    for record in &records {
        let DnsRecordData::Srv { port } = record.data else {
            continue;
        };
        if port == 0 {
            continue;
        }

        let profile_and_ttl = ptr_profiles
            .get(&record.name)
            .copied()
            .or_else(|| mdns_profile_for_instance(&record.name).map(|profile| (profile, 0)));
        let Some((profile, ptr_ttl)) = profile_and_ttl else {
            continue;
        };
        let txt = records.iter().find_map(|candidate| {
            if candidate.name == record.name
                && let DnsRecordData::Txt(properties) = &candidate.data
            {
                return Some(properties);
            }
            None
        });
        if profile.service_type == "_rms._tcp.local"
            && txt.is_some_and(|properties| properties.contains_key("v"))
        {
            let Some(properties) = txt else {
                continue;
            };
            let Ok(verified) = edge_advertisement::verify(
                &record.name,
                port,
                properties,
                now_seconds,
                organization_proof,
            ) else {
                continue;
            };
            if let Some(advertisement) =
                signed_edge_advertisement(verified, port, nonzero_min(record.ttl_seconds, ptr_ttl))
            {
                advertisements.push(advertisement);
            }
            continue;
        }
        let advertised_name = txt
            .and_then(|properties| {
                properties
                    .get("name")
                    .or_else(|| properties.get("fn"))
                    .or_else(|| properties.get("friendly_name"))
            })
            .map(String::as_str)
            .unwrap_or_else(|| mdns_instance_label(&record.name, profile.service_type));
        let endpoint_path = txt
            .and_then(|properties| properties.get("path"))
            .map_or_else(|| "/".to_owned(), |path| safe_path(path));
        advertisements.push(ParsedAdvertisement {
            fingerprint_namespace: "mdns",
            identity: format!("{}:{}", profile.service_type, record.name),
            display_name: advertised_name.to_owned(),
            category: profile.category,
            suggested_device_kind: profile.suggested_device_kind,
            integration_kind: profile.integration_kind,
            endpoint_label: profile.endpoint_label.to_owned(),
            source_id: profile.source_id,
            source_label: profile.source_label.to_owned(),
            source_category: profile.source_category,
            protocol: profile.protocol,
            endpoint_scheme: profile.scheme.to_owned(),
            endpoint_port: port,
            endpoint_path,
            ttl_seconds: nonzero_min(record.ttl_seconds, ptr_ttl),
            additional_sources: Vec::new(),
            edge_identity: None,
        });
    }

    Ok(advertisements)
}

fn signed_edge_advertisement(
    verified: edge_advertisement::VerifiedEdgeAdvertisement,
    port: u16,
    ttl_seconds: u64,
) -> Option<ParsedAdvertisement> {
    let category = match verified.device_kind {
        "robot" => DiscoveryCandidateCategory::Robot,
        "drone" => DiscoveryCandidateCategory::Drone,
        "vehicle" => DiscoveryCandidateCategory::Vehicle,
        "camera" => DiscoveryCandidateCategory::Camera,
        "gateway" => DiscoveryCandidateCategory::Gateway,
        _ => return None,
    };
    let mut sources = verified
        .capabilities
        .iter()
        .filter_map(|capability| {
            let (id, label, source_category, protocol, suffix) = match capability {
                EdgeCapability::Ros2Dds => (
                    "ros2-graph",
                    "ROS 2 데이터",
                    DiscoverySourceCategory::State,
                    "ros2",
                    "ros2",
                ),
                EdgeCapability::Mavlink => (
                    "mavlink-telemetry",
                    "MAVLink 텔레메트리",
                    DiscoverySourceCategory::Telemetry,
                    "mavlink",
                    "mavlink",
                ),
                EdgeCapability::Status => return None,
            };
            let path = if verified.path == "/" {
                format!("/{suffix}")
            } else {
                format!("{}/{suffix}", verified.path.trim_end_matches('/'))
            };
            Some(ParsedSourceAdvertisement {
                id,
                label,
                category: source_category,
                protocol,
                scheme: "http",
                port,
                path,
            })
        })
        .collect::<Vec<_>>();
    if sources.is_empty() {
        return None;
    }
    let primary = sources.remove(0);
    let identity = EdgeObservationIdentity {
        device_id: verified.device_id.clone(),
        public_key: verified.public_key,
        nonce: verified.nonce,
        timestamp_seconds: verified.timestamp_seconds,
        organization_id: verified.organization_id,
        organization_trusted: verified.organization_trusted,
    };
    Some(ParsedAdvertisement {
        fingerprint_namespace: "edge-v1",
        identity: verified.device_id,
        display_name: verified.display_name,
        category,
        suggested_device_kind: verified.device_kind,
        integration_kind: "rms_edge",
        endpoint_label: "RMS Edge Agent".to_owned(),
        source_id: primary.id,
        source_label: primary.label.to_owned(),
        source_category: primary.category,
        protocol: primary.protocol,
        endpoint_scheme: primary.scheme.to_owned(),
        endpoint_port: primary.port,
        endpoint_path: primary.path,
        ttl_seconds,
        additional_sources: sources,
        edge_identity: Some(identity),
    })
}

fn mdns_profile_for_service(service: &str) -> Option<MdnsServiceProfile> {
    MDNS_SERVICE_PROFILES
        .iter()
        .find(|profile| service.eq_ignore_ascii_case(profile.service_type))
        .copied()
}

fn mdns_profile_for_instance(instance: &str) -> Option<MdnsServiceProfile> {
    MDNS_SERVICE_PROFILES
        .iter()
        .find(|profile| {
            instance.len() > profile.service_type.len()
                && instance
                    .get(instance.len() - profile.service_type.len()..)
                    .is_some_and(|suffix| suffix.eq_ignore_ascii_case(profile.service_type))
                && instance
                    .as_bytes()
                    .get(instance.len() - profile.service_type.len() - 1)
                    == Some(&b'.')
        })
        .copied()
}

fn mdns_instance_label<'a>(instance: &'a str, service: &str) -> &'a str {
    instance
        .strip_suffix(service)
        .and_then(|prefix| prefix.strip_suffix('.'))
        .filter(|prefix| !prefix.is_empty())
        .unwrap_or("LAN device")
}

fn normalize_dns_name(name: &str) -> String {
    name.trim_end_matches('.').to_ascii_lowercase()
}

fn read_dns_name(packet: &[u8], cursor: &mut usize) -> PacketParseResult<String> {
    let mut labels = Vec::new();
    let mut position = *cursor;
    let mut jumped = false;
    let mut pointer_jumps = 0;
    let mut encoded_length = 0;

    loop {
        let length = *packet.get(position).ok_or(PacketParseError)?;
        if length == 0 {
            if !jumped {
                *cursor = position + 1;
            }
            break;
        }
        if length & 0xc0 == 0xc0 {
            let low = *packet.get(position + 1).ok_or(PacketParseError)?;
            let pointer = usize::from((u16::from(length & 0x3f) << 8) | u16::from(low));
            if pointer >= packet.len() || pointer_jumps >= MAX_DNS_POINTER_JUMPS {
                return Err(PacketParseError);
            }
            if !jumped {
                *cursor = position + 2;
                jumped = true;
            }
            position = pointer;
            pointer_jumps += 1;
            continue;
        }
        if length & 0xc0 != 0 || length > 63 || labels.len() >= MAX_DNS_LABELS {
            return Err(PacketParseError);
        }

        let label_start = position + 1;
        let label_end = label_start
            .checked_add(usize::from(length))
            .filter(|end| *end <= packet.len())
            .ok_or(PacketParseError)?;
        let label =
            std::str::from_utf8(&packet[label_start..label_end]).map_err(PacketParseError::from)?;
        if label.chars().any(char::is_control) {
            return Err(PacketParseError);
        }
        encoded_length += usize::from(length) + 1;
        if encoded_length > 254 {
            return Err(PacketParseError);
        }
        labels.push(label);
        position = label_end;
        if !jumped {
            *cursor = position;
        }
    }

    Ok(labels.join("."))
}

fn parse_dns_txt(data: &[u8]) -> PacketParseResult<BTreeMap<String, String>> {
    let mut properties = BTreeMap::new();
    let mut cursor = 0;
    while cursor < data.len() {
        let length = usize::from(data[cursor]);
        cursor += 1;
        let end = cursor
            .checked_add(length)
            .filter(|end| *end <= data.len())
            .ok_or(PacketParseError)?;
        if let Ok(field) = std::str::from_utf8(&data[cursor..end]) {
            let (key, value) = field.split_once('=').unwrap_or((field, ""));
            if !key.is_empty()
                && key.len() <= 64
                && key
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
            {
                properties
                    .entry(key.to_ascii_lowercase())
                    .or_insert_with(|| value.chars().take(256).collect());
            }
        }
        cursor = end;
    }
    Ok(properties)
}

fn read_u16(packet: &[u8], cursor: &mut usize) -> PacketParseResult<u16> {
    let end = advance(cursor, 2, packet.len())?;
    Ok(u16::from_be_bytes([packet[end - 2], packet[end - 1]]))
}

fn read_u32(packet: &[u8], cursor: &mut usize) -> PacketParseResult<u32> {
    let end = advance(cursor, 4, packet.len())?;
    Ok(u32::from_be_bytes([
        packet[end - 4],
        packet[end - 3],
        packet[end - 2],
        packet[end - 1],
    ]))
}

fn advance(cursor: &mut usize, amount: usize, packet_size: usize) -> PacketParseResult<usize> {
    let end = cursor
        .checked_add(amount)
        .filter(|end| *end <= packet_size)
        .ok_or(PacketParseError)?;
    *cursor = end;
    Ok(end)
}

fn parse_ssdp_packet(packet: &[u8]) -> PacketParseResult<Vec<ParsedAdvertisement>> {
    ensure_packet_size(packet)?;
    let text = std::str::from_utf8(packet).map_err(PacketParseError::from)?;
    if text.bytes().any(|byte| byte == 0) || (!text.contains("\r\n\r\n") && !text.contains("\n\n"))
    {
        return Err(PacketParseError);
    }

    let mut lines = text.lines();
    let status_line = lines.next().ok_or(PacketParseError)?.trim_end_matches('\r');
    if status_line.len() > MAX_HEADER_LINE_SIZE
        || !status_line.to_ascii_uppercase().starts_with("HTTP/1.1 200")
    {
        return Ok(Vec::new());
    }

    let mut headers = BTreeMap::new();
    for (index, line) in lines.enumerate() {
        if index >= MAX_HTTP_HEADERS {
            return Err(PacketParseError);
        }
        let line = line.trim_end_matches('\r');
        if line.is_empty() {
            break;
        }
        if line.len() > MAX_HEADER_LINE_SIZE
            || line
                .chars()
                .any(|character| character.is_control() && character != '\t')
        {
            return Err(PacketParseError);
        }
        let (name, value) = line.split_once(':').ok_or(PacketParseError)?;
        let name = name.trim();
        if name.is_empty()
            || !name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        {
            return Err(PacketParseError);
        }
        headers
            .entry(name.to_ascii_lowercase())
            .or_insert_with(|| value.trim().to_owned());
    }

    let usn = headers
        .get("usn")
        .filter(|value| !value.is_empty() && value.len() <= 512)
        .ok_or(PacketParseError)?;
    let search_target = headers
        .get("st")
        .or_else(|| headers.get("nt"))
        .filter(|value| !value.is_empty() && value.len() <= 512)
        .ok_or(PacketParseError)?;
    let server = headers.get("server").map_or("UPnP device", String::as_str);
    let evidence = format!("{usn} {search_target} {server}").to_ascii_lowercase();
    let classification = classify_ssdp(&evidence);
    let (endpoint_scheme, endpoint_port, endpoint_path) = headers
        .get("location")
        .and_then(|location| parse_advertised_endpoint(location, &["http", "https", "rtsp"]))
        .ok_or(PacketParseError)?;
    let ttl_seconds = headers
        .get("cache-control")
        .and_then(|value| parse_max_age(value))
        .unwrap_or(30);
    let identity = usn.split("::").next().unwrap_or(usn).trim().to_owned();
    let display_name = if server.trim().is_empty() {
        search_target.to_owned()
    } else {
        server.to_owned()
    };

    Ok(vec![ParsedAdvertisement {
        fingerprint_namespace: "ssdp",
        identity,
        display_name,
        category: classification.category,
        suggested_device_kind: classification.suggested_device_kind,
        integration_kind: classification.integration_kind,
        endpoint_label: "SSDP device endpoint".to_owned(),
        source_id: classification.source_id,
        source_label: classification.source_label.to_owned(),
        source_category: classification.source_category,
        protocol: "ssdp",
        endpoint_scheme,
        endpoint_port,
        endpoint_path,
        ttl_seconds,
        additional_sources: Vec::new(),
        edge_identity: None,
    }])
}

struct SsdpClassification {
    category: DiscoveryCandidateCategory,
    suggested_device_kind: &'static str,
    integration_kind: &'static str,
    source_id: &'static str,
    source_label: &'static str,
    source_category: DiscoverySourceCategory,
}

fn classify_ssdp(evidence: &str) -> SsdpClassification {
    if ["camera", "onvif", "networkvideotransmitter", "videoencoder"]
        .iter()
        .any(|marker| evidence.contains(marker))
    {
        SsdpClassification {
            category: DiscoveryCandidateCategory::Camera,
            suggested_device_kind: "camera",
            integration_kind: "rtsp",
            source_id: "camera",
            source_label: "카메라 영상",
            source_category: DiscoverySourceCategory::Camera,
        }
    } else if evidence.contains("drone") || evidence.contains("uav") {
        SsdpClassification {
            category: DiscoveryCandidateCategory::Drone,
            suggested_device_kind: "drone",
            integration_kind: "rerun",
            source_id: "state",
            source_label: "장비 상태",
            source_category: DiscoverySourceCategory::State,
        }
    } else if evidence.contains("vehicle") {
        SsdpClassification {
            category: DiscoveryCandidateCategory::Vehicle,
            suggested_device_kind: "vehicle",
            integration_kind: "rerun",
            source_id: "state",
            source_label: "장비 상태",
            source_category: DiscoverySourceCategory::State,
        }
    } else {
        SsdpClassification {
            category: DiscoveryCandidateCategory::Gateway,
            suggested_device_kind: "gateway",
            integration_kind: "rerun",
            source_id: "state",
            source_label: "장비 상태",
            source_category: DiscoverySourceCategory::State,
        }
    }
}

fn parse_max_age(value: &str) -> Option<u64> {
    value.split(',').find_map(|directive| {
        let (name, value) = directive.trim().split_once('=')?;
        if name.trim().eq_ignore_ascii_case("max-age") {
            value.trim().trim_matches('"').parse().ok()
        } else {
            None
        }
    })
}

fn ws_discovery_query() -> String {
    let message_id = uuid::Uuid::new_v4();
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\
<e:Envelope xmlns:e=\"http://www.w3.org/2003/05/soap-envelope\" \
xmlns:w=\"http://schemas.xmlsoap.org/ws/2004/08/addressing\" \
xmlns:d=\"http://schemas.xmlsoap.org/ws/2005/04/discovery\" \
xmlns:dn=\"http://www.onvif.org/ver10/network/wsdl\">\
<e:Header>\
<w:MessageID>uuid:{message_id}</w:MessageID>\
<w:To>urn:schemas-xmlsoap-org:ws:2005:04:discovery</w:To>\
<w:Action>http://schemas.xmlsoap.org/ws/2005/04/discovery/Probe</w:Action>\
</e:Header>\
<e:Body><d:Probe><d:Types>dn:NetworkVideoTransmitter</d:Types></d:Probe></e:Body>\
</e:Envelope>"
    )
}

fn parse_ws_discovery_packet(packet: &[u8]) -> PacketParseResult<Vec<ParsedAdvertisement>> {
    ensure_packet_size(packet)?;
    let text = std::str::from_utf8(packet).map_err(PacketParseError::from)?;
    let lowercase = text.to_ascii_lowercase();
    if lowercase.contains("<!doctype") || lowercase.contains("<!entity") {
        return Err(PacketParseError);
    }
    if !has_xml_local_tag(text, "ProbeMatches")? {
        return Ok(Vec::new());
    }

    let types = extract_xml_values(text, "Types")?;
    let scopes = extract_xml_values(text, "Scopes")?;
    let x_addresses = extract_xml_values(text, "XAddrs")?;
    let evidence = types
        .iter()
        .chain(scopes.iter())
        .chain(x_addresses.iter())
        .copied()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase();
    if !evidence.contains("onvif") && !evidence.contains("networkvideotransmitter") {
        return Ok(Vec::new());
    }

    let identity = extract_xml_values(text, "Address")?
        .into_iter()
        .find(|value| !value.trim().is_empty() && value.len() <= 512)
        .ok_or(PacketParseError)?
        .trim()
        .to_owned();
    let endpoint = x_addresses
        .iter()
        .flat_map(|value| value.split_ascii_whitespace())
        .find_map(|address| parse_advertised_endpoint(address, &["http", "https"]))
        .ok_or(PacketParseError)?;
    let display_name = scopes
        .iter()
        .find_map(|value| onvif_scope_name(value))
        .unwrap_or_else(|| "ONVIF camera".to_owned());

    Ok(vec![ParsedAdvertisement {
        fingerprint_namespace: "onvif",
        identity,
        display_name,
        category: DiscoveryCandidateCategory::Camera,
        suggested_device_kind: "camera",
        integration_kind: "rtsp",
        endpoint_label: "ONVIF device service".to_owned(),
        source_id: "camera",
        source_label: "카메라 영상".to_owned(),
        source_category: DiscoverySourceCategory::Camera,
        protocol: "onvif",
        endpoint_scheme: endpoint.0,
        endpoint_port: endpoint.1,
        endpoint_path: endpoint.2,
        ttl_seconds: 30,
        additional_sources: Vec::new(),
        edge_identity: None,
    }])
}

fn has_xml_local_tag(text: &str, expected_local_name: &str) -> PacketParseResult<bool> {
    let mut cursor = 0;
    while let Some(relative_start) = text[cursor..].find('<') {
        let start = cursor + relative_start;
        let relative_end = text[start + 1..].find('>').ok_or(PacketParseError)?;
        let end = start + 1 + relative_end;
        if end - start > MAX_XML_TAG_SIZE {
            return Err(PacketParseError);
        }
        let tag = text[start + 1..end].trim();
        if !tag.starts_with('/') && !tag.starts_with('!') && !tag.starts_with('?') {
            let qualified_name = tag
                .split_ascii_whitespace()
                .next()
                .unwrap_or_default()
                .trim_end_matches('/');
            if qualified_name
                .rsplit(':')
                .next()
                .is_some_and(|name| name.eq_ignore_ascii_case(expected_local_name))
            {
                return Ok(true);
            }
        }
        cursor = end + 1;
    }
    Ok(false)
}

fn extract_xml_values<'a>(
    text: &'a str,
    expected_local_name: &str,
) -> PacketParseResult<Vec<&'a str>> {
    let mut values = Vec::new();
    let mut cursor = 0;
    while cursor < text.len() && values.len() < MAX_XML_VALUES {
        let Some(relative_start) = text[cursor..].find('<') else {
            break;
        };
        let start = cursor + relative_start;
        let relative_end = text[start + 1..].find('>').ok_or(PacketParseError)?;
        let end = start + 1 + relative_end;
        if end - start > MAX_XML_TAG_SIZE {
            return Err(PacketParseError);
        }
        let tag = text[start + 1..end].trim();
        if tag.starts_with('/') || tag.starts_with('!') || tag.starts_with('?') {
            cursor = end + 1;
            continue;
        }
        let qualified_name = tag
            .split_ascii_whitespace()
            .next()
            .unwrap_or_default()
            .trim_end_matches('/');
        let local_name = qualified_name.rsplit(':').next().unwrap_or_default();
        if local_name.eq_ignore_ascii_case(expected_local_name) && !tag.ends_with('/') {
            let closing_prefix = format!("</{qualified_name}");
            let value_start = end + 1;
            let relative_close = text[value_start..]
                .find(&closing_prefix)
                .ok_or(PacketParseError)?;
            let value_end = value_start + relative_close;
            let closing_end = text[value_end + closing_prefix.len()..]
                .find('>')
                .ok_or(PacketParseError)?;
            let value = text[value_start..value_end].trim();
            if value.contains('<') || value.len() > 2_048 {
                return Err(PacketParseError);
            }
            values.push(value);
            cursor = value_end + closing_prefix.len() + closing_end + 1;
        } else {
            cursor = end + 1;
        }
    }
    Ok(values)
}

fn onvif_scope_name(scopes: &str) -> Option<String> {
    const NAME_SCOPE: &str = "onvif://www.onvif.org/name/";
    scopes.split_ascii_whitespace().find_map(|scope| {
        let lowercase = scope.to_ascii_lowercase();
        let offset = lowercase.find(NAME_SCOPE)? + NAME_SCOPE.len();
        Some(percent_decode_component(&scope[offset..]))
    })
}

fn percent_decode_component(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut cursor = 0;
    while cursor < bytes.len() && decoded.len() < 256 {
        if bytes[cursor] == b'%'
            && cursor + 2 < bytes.len()
            && let (Some(high), Some(low)) =
                (hex_value(bytes[cursor + 1]), hex_value(bytes[cursor + 2]))
        {
            decoded.push((high << 4) | low);
            cursor += 3;
            continue;
        }
        decoded.push(bytes[cursor]);
        cursor += 1;
    }
    String::from_utf8_lossy(&decoded).into_owned()
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// Extracts only the scheme, port, and path from an advertised URL.
///
/// The advertised host is deliberately discarded; the UDP source address is used instead.
fn parse_advertised_endpoint(
    value: &str,
    allowed_schemes: &[&str],
) -> Option<(String, u16, String)> {
    if value.len() > 2_048 || value.chars().any(char::is_control) {
        return None;
    }
    let (scheme, remainder) = value.trim().split_once("://")?;
    let scheme = scheme.to_ascii_lowercase();
    if !allowed_schemes
        .iter()
        .any(|allowed| scheme.eq_ignore_ascii_case(allowed))
    {
        return None;
    }
    let authority_end = remainder.find(['/', '?', '#']).unwrap_or(remainder.len());
    let authority = &remainder[..authority_end];
    if authority.is_empty() || authority.contains('@') || authority.chars().any(char::is_whitespace)
    {
        return None;
    }

    let explicit_port = if let Some(bracketed) = authority.strip_prefix('[') {
        let closing_bracket = bracketed.find(']')?;
        let suffix = &bracketed[closing_bracket + 1..];
        if suffix.is_empty() {
            None
        } else {
            Some(suffix.strip_prefix(':')?.parse::<u16>().ok()?)
        }
    } else if let Some((host, port)) = authority.rsplit_once(':') {
        if host.contains(':') || host.is_empty() {
            return None;
        }
        Some(port.parse::<u16>().ok()?)
    } else {
        None
    };
    let port = explicit_port.unwrap_or(match scheme.as_str() {
        "https" => 443,
        "rtsp" => 554,
        _ => 80,
    });
    if port == 0 {
        return None;
    }

    let path_and_suffix = &remainder[authority_end..];
    let path_end = path_and_suffix
        .find(['?', '#'])
        .unwrap_or(path_and_suffix.len());
    let path = if path_and_suffix.starts_with('/') {
        safe_path(&path_and_suffix[..path_end])
    } else {
        "/".to_owned()
    };
    Some((scheme, port, path))
}

fn sanitize_label(value: &str, fallback: &str) -> String {
    let sanitized = value
        .trim()
        .chars()
        .filter(|character| !character.is_control())
        .take(96)
        .collect::<String>();
    if sanitized.is_empty() {
        fallback.to_owned()
    } else {
        sanitized
    }
}

fn safe_path(value: &str) -> String {
    if !value.starts_with('/') || value.chars().any(char::is_control) {
        return "/".to_owned();
    }
    let path = value.chars().take(256).collect::<String>();
    if path.is_empty() {
        "/".to_owned()
    } else {
        path
    }
}

fn nonzero_min(left: u64, right: u64) -> u64 {
    match (left, right) {
        (0, 0) => 30,
        (0, right) => right,
        (left, 0) => left,
        (left, right) => left.min(right),
    }
}

fn ensure_packet_size(packet: &[u8]) -> PacketParseResult<()> {
    if packet.len() > MAX_PACKET_SIZE {
        Err(PacketParseError)
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn push_dns_name(packet: &mut Vec<u8>, name: &str) {
        for label in name.split('.') {
            packet.push(label.len() as u8);
            packet.extend_from_slice(label.as_bytes());
        }
        packet.push(0);
    }

    fn mdns_rtsp_response() -> Vec<u8> {
        let mut packet = Vec::new();
        packet.extend_from_slice(&0_u16.to_be_bytes());
        packet.extend_from_slice(&0x8400_u16.to_be_bytes());
        packet.extend_from_slice(&0_u16.to_be_bytes());
        packet.extend_from_slice(&1_u16.to_be_bytes());
        packet.extend_from_slice(&0_u16.to_be_bytes());
        packet.extend_from_slice(&2_u16.to_be_bytes());

        let service_offset = packet.len();
        push_dns_name(&mut packet, "_rtsp._tcp.local");
        packet.extend_from_slice(&12_u16.to_be_bytes());
        packet.extend_from_slice(&1_u16.to_be_bytes());
        packet.extend_from_slice(&60_u32.to_be_bytes());
        let mut instance_name = Vec::new();
        instance_name.push(7);
        instance_name.extend_from_slice(b"Camera1");
        instance_name.extend_from_slice(
            &(0xc000_u16 | u16::try_from(service_offset).expect("test offset fits")).to_be_bytes(),
        );
        packet.extend_from_slice(
            &u16::try_from(instance_name.len())
                .expect("test record fits")
                .to_be_bytes(),
        );
        packet.extend_from_slice(&instance_name);

        let instance_offset = packet.len();
        packet.extend_from_slice(&instance_name);
        packet.extend_from_slice(&33_u16.to_be_bytes());
        packet.extend_from_slice(&1_u16.to_be_bytes());
        packet.extend_from_slice(&45_u32.to_be_bytes());
        let mut srv_data = Vec::new();
        srv_data.extend_from_slice(&0_u16.to_be_bytes());
        srv_data.extend_from_slice(&0_u16.to_be_bytes());
        srv_data.extend_from_slice(&8554_u16.to_be_bytes());
        push_dns_name(&mut srv_data, "camera.local");
        packet.extend_from_slice(
            &u16::try_from(srv_data.len())
                .expect("test record fits")
                .to_be_bytes(),
        );
        packet.extend_from_slice(&srv_data);

        packet.extend_from_slice(
            &(0xc000_u16 | u16::try_from(instance_offset).expect("test offset fits")).to_be_bytes(),
        );
        packet.extend_from_slice(&16_u16.to_be_bytes());
        packet.extend_from_slice(&1_u16.to_be_bytes());
        packet.extend_from_slice(&45_u32.to_be_bytes());
        let txt = b"\x0bpath=/front";
        packet.extend_from_slice(
            &u16::try_from(txt.len())
                .expect("test record fits")
                .to_be_bytes(),
        );
        packet.extend_from_slice(txt);
        packet
    }

    #[test]
    fn parses_allowlisted_mdns_and_ignores_advertised_host() {
        let advertisements = parse_mdns_packet(&mdns_rtsp_response(), None, unix_now_seconds())
            .expect("valid mDNS response parses");
        assert_eq!(advertisements.len(), 1);
        let advertisement = &advertisements[0];
        assert_eq!(advertisement.display_name, "camera1");
        assert_eq!(advertisement.endpoint_port, 8554);
        assert_eq!(advertisement.endpoint_path, "/front");
        assert_eq!(advertisement.protocol, "rtsp");
    }

    #[test]
    fn rejects_mdns_compression_pointer_loop() {
        let mut packet = vec![0_u8; 12];
        packet[2..4].copy_from_slice(&0x8400_u16.to_be_bytes());
        packet[6..8].copy_from_slice(&1_u16.to_be_bytes());
        packet.extend_from_slice(&[0xc0, 0x0c]);
        packet.extend_from_slice(&12_u16.to_be_bytes());
        packet.extend_from_slice(&1_u16.to_be_bytes());
        packet.extend_from_slice(&30_u32.to_be_bytes());
        packet.extend_from_slice(&2_u16.to_be_bytes());
        packet.extend_from_slice(&[0xc0, 0x0c]);
        assert!(parse_mdns_packet(&packet, None, unix_now_seconds()).is_err());
    }

    #[test]
    fn rejects_oversized_mdns_packet() {
        assert!(
            parse_mdns_packet(&vec![0_u8; MAX_PACKET_SIZE + 1], None, unix_now_seconds()).is_err()
        );
    }

    #[test]
    fn parses_ssdp_without_trusting_location_host() {
        let packet = b"HTTP/1.1 200 OK\r\n\
ST: urn:schemas-upnp-org:device:camera:1\r\n\
USN: uuid:camera-123::urn:schemas-upnp-org:device:camera:1\r\n\
SERVER: Example Camera\r\n\
LOCATION: http://203.0.113.99:8080/device.xml?secret=ignored\r\n\
CACHE-CONTROL: max-age=1800\r\n\
\r\n";
        let advertisements = parse_ssdp_packet(packet).expect("valid SSDP response parses");
        assert_eq!(advertisements.len(), 1);
        let observation = advertisements
            .into_iter()
            .next()
            .expect("one advertisement")
            .into_observation("192.0.2.10:1900".parse().expect("test address parses"));
        let endpoint = &observation.sources[0].endpoint;
        assert_eq!(
            endpoint.source_address,
            "192.0.2.10".parse::<IpAddr>().expect("test address parses")
        );
        assert_eq!(endpoint.port, 8080);
        assert_eq!(endpoint.path, "/device.xml");
        assert_eq!(
            observation.expires_at_ms - observation.last_seen_at_ms,
            120_000
        );
    }

    #[test]
    fn rejects_malformed_and_oversized_ssdp_packets() {
        assert!(parse_ssdp_packet(b"HTTP/1.1 200 OK\r\nUSN missing-colon\r\n\r\n").is_err());
        assert!(parse_ssdp_packet(&vec![b'a'; MAX_PACKET_SIZE + 1]).is_err());
    }

    #[test]
    fn parses_onvif_probe_match_and_pins_packet_source() {
        let packet = br#"<?xml version="1.0"?>
<e:Envelope xmlns:e="http://www.w3.org/2003/05/soap-envelope" xmlns:a="http://schemas.xmlsoap.org/ws/2004/08/addressing" xmlns:d="http://schemas.xmlsoap.org/ws/2005/04/discovery">
<e:Body><d:ProbeMatches><d:ProbeMatch>
<a:EndpointReference><a:Address>urn:uuid:camera-456</a:Address></a:EndpointReference>
<d:Types>dn:NetworkVideoTransmitter</d:Types>
<d:Scopes>onvif://www.onvif.org/name/Loading%20Dock</d:Scopes>
<d:XAddrs>http://198.51.100.77:8899/onvif/device_service</d:XAddrs>
</d:ProbeMatch></d:ProbeMatches></e:Body></e:Envelope>"#;
        let advertisement = parse_ws_discovery_packet(packet)
            .expect("valid WS-Discovery response parses")
            .into_iter()
            .next()
            .expect("one advertisement");
        assert_eq!(advertisement.display_name, "Loading Dock");
        let observation =
            advertisement.into_observation("192.0.2.20:3702".parse().expect("test address parses"));
        assert_eq!(
            observation.sources[0].endpoint.source_address,
            "192.0.2.20".parse::<IpAddr>().expect("test address parses")
        );
        assert_eq!(observation.sources[0].endpoint.port, 8899);
    }

    #[test]
    fn rejects_unsafe_or_oversized_ws_discovery_packets() {
        let entity_packet =
            br#"<!DOCTYPE x [<!ENTITY y SYSTEM "file:///etc/passwd">]><d:ProbeMatches/>"#;
        assert!(parse_ws_discovery_packet(entity_packet).is_err());
        assert!(parse_ws_discovery_packet(&vec![b'a'; MAX_PACKET_SIZE + 1]).is_err());
    }

    #[test]
    fn accepts_only_lan_source_addresses() {
        for allowed in [
            "10.1.2.3",
            "172.16.4.5",
            "192.168.6.7",
            "169.254.8.9",
            "fd12::1",
            "fe80::1",
        ] {
            assert!(
                is_usable_source_address(allowed.parse().expect("test address parses")),
                "expected {allowed} to be accepted"
            );
        }
        for rejected in [
            "127.0.0.1",
            "8.8.8.8",
            "192.0.2.1",
            "203.0.113.4",
            "::1",
            "2001:4860:4860::8888",
        ] {
            assert!(
                !is_usable_source_address(rejected.parse().expect("test address parses")),
                "expected {rejected} to be rejected"
            );
        }
    }

    #[tokio::test]
    async fn local_udp_responder_smoke_pins_datagram_source() {
        let responder = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .expect("loopback responder binds");
        let responder_address = responder.local_addr().expect("responder has an address");
        let responder_task = tokio::spawn(async move {
            let mut query = [0_u8; 512];
            let (_, peer) = responder
                .recv_from(&mut query)
                .await
                .expect("responder receives query");
            responder
                .send_to(&mdns_rtsp_response(), peer)
                .await
                .expect("responder sends reply");
        });

        let client = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .expect("loopback client binds");
        client
            .send_to(&mdns_query(), responder_address)
            .await
            .expect("client sends query");
        let mut response = vec![0_u8; MAX_PACKET_SIZE + 1];
        let (size, source) =
            tokio::time::timeout(Duration::from_secs(1), client.recv_from(&mut response))
                .await
                .expect("responder replies before timeout")
                .expect("client receives reply");
        responder_task.await.expect("responder task succeeds");

        let observation = parse_mdns_packet(&response[..size], None, unix_now_seconds())
            .expect("response parses")
            .into_iter()
            .next()
            .expect("response contains a candidate")
            .into_observation(source);
        assert_eq!(observation.sources[0].endpoint.source_address, source.ip());
    }

    #[tokio::test]
    async fn cancelled_scan_does_not_touch_the_network() {
        let cancellation = DiscoveryCancellation::new();
        cancellation.cancel();
        let observations = MulticastDiscoveryProvider::default()
            .discover(cancellation)
            .await
            .expect("a cancelled scan succeeds");
        assert!(observations.is_empty());
    }

    #[tokio::test]
    #[ignore = "sends real multicast probes on the local network"]
    async fn local_multicast_discovery_smoke() {
        let provider = MulticastDiscoveryProvider {
            scan_timeout: Duration::from_millis(500),
            max_candidates: 8,
            organization_proof: None,
            edge_nonces: Arc::new(Mutex::new(BTreeMap::new())),
        };
        let observations = provider
            .discover(DiscoveryCancellation::new())
            .await
            .expect("local multicast sockets are available");
        assert!(observations.len() <= 8);
    }
}
