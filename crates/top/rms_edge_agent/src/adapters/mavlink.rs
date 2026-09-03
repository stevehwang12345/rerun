//! Passive, observation-only `MAVLink` heartbeat adapter.
//!
//! The adapter never transmits `MAVLink`. It listens on one configured local interface, validates
//! `HEARTBEAT` frames and optional `MAVLink` 2 signing, then returns bounded metadata-only observations.

use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    fs,
    io::{Read as _, Write as _},
    net::{IpAddr, SocketAddr},
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use async_trait::async_trait;
use atomicwrites::{AllowOverwrite, AtomicFile};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use tokio::net::UdpSocket;
use tokio::sync::{Mutex, OnceCell};

use crate::{
    adapter::{
        Adapter, AdapterBatch, AdapterDescriptor, AdapterError, AdapterKind, AdapterObservation,
        AdapterPollContext, ObservationTrust, ObservedSource, ObservedTopic, RendererHint,
        SourceCategory, SourceStatus,
    },
    config::AdapterConfig,
};

const V1_MAGIC: u8 = 0xFE;
const V2_MAGIC: u8 = 0xFD;
const V2_SIGNED: u8 = 0x01;
const HEARTBEAT_ID: u32 = 0;
const HEARTBEAT_LEN: usize = 9;
const HEARTBEAT_CRC_EXTRA: u8 = 50;
const SIGNATURE_LEN: usize = 13;
const SIGNING_KEY_LEN: usize = 32;
const SIGNING_EPOCH_UNIX_SECONDS: u64 = 1_420_070_400;
const MAX_DATAGRAM_BYTES: usize = 65_535;
const MAX_PACKETS_PER_DATAGRAM: usize = 256;
const MAX_ALLOWED_SOURCES: usize = 256;
const MAX_PEERS: usize = 512;
const MAX_REPLAY_LINKS: usize = 4_096;
const MAX_INVALID_DATAGRAMS_PER_POLL: usize = 256;
const MAX_DATAGRAMS_PER_POLL: usize = 1_024;
const REPLAY_SCHEMA_VERSION: u32 = 2;

// Source addresses are deliberately excluded: MAVLink anti-replay identity is defined by the
// sending system, component, and signing link. A source address is only a local collision pin.
type ReplayKey = (u8, u8, u8);
type ReplayState = HashMap<ReplayKey, u64>;
type SourcePins = HashMap<(u8, u8), IpAddr>;

/// Settings for one passive `MAVLink` socket.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MavlinkSettings {
    /// Literal local interface on which heartbeats are received.
    pub bind_ip: IpAddr,
    /// UDP port, normally `14550`.
    #[serde(default = "default_port")]
    pub port: u16,
    /// Required vehicle system id. One adapter represents exactly this one vehicle.
    pub system_id: u8,
    /// Optional exact source-IP allowlist.
    #[serde(default)]
    pub allowed_source_ips: Vec<IpAddr>,
    /// Reject every unsigned packet when true.
    #[serde(default)]
    pub require_signing: bool,
    /// File containing exactly 32 raw `MAVLink` signing-key bytes.
    #[serde(default)]
    pub signing_key_path: Option<PathBuf>,
    /// Durable anti-replay state. Required whenever a signing key is configured.
    #[serde(default)]
    pub replay_state_path: Option<PathBuf>,
    /// Time spent receiving heartbeats during each poll.
    #[serde(default = "default_receive_window_ms")]
    pub receive_window_ms: u64,
    /// Freshness period assigned to returned observations.
    #[serde(default = "default_heartbeat_timeout_ms")]
    pub heartbeat_timeout_ms: i64,
    /// Maximum distinct components aggregated into the vehicle observation per poll.
    #[serde(default = "default_max_peers")]
    pub max_peers: usize,
    /// Maximum age of the first signed packet on a link.
    #[serde(default = "default_signature_max_age_ms")]
    pub signature_max_age_ms: u64,
    /// Maximum permitted future signature clock skew.
    #[serde(default = "default_signature_future_skew_ms")]
    pub signature_future_skew_ms: u64,
}

/// Passive `MAVLink` discovery adapter.
pub struct MavlinkAdapter {
    adapter_id: String,
    settings: MavlinkSettings,
    signing_key: Option<[u8; SIGNING_KEY_LEN]>,
    replay_timestamps: Mutex<ReplayState>,
    source_pins: Mutex<SourcePins>,
    // Kept for the adapter lifetime so the bounded OS receive queue spans supervisor poll gaps.
    // No send operation is exposed or used by this passive adapter.
    socket: OnceCell<UdpSocket>,
}

impl MavlinkAdapter {
    /// Parse and validate adapter settings without exposing secret key material.
    pub fn from_config(config: &AdapterConfig) -> Result<Self, AdapterError> {
        if config.kind != AdapterKind::Mavlink {
            return Err(AdapterError::configuration(
                "MAVLink adapter received the wrong protocol configuration",
            ));
        }
        let settings = serde_json::from_value(config.settings.clone()).map_err(|_error| {
            AdapterError::configuration("MAVLink adapter settings are invalid")
        })?;
        Self::new(config.id.clone(), settings)
    }

    fn new(adapter_id: String, settings: MavlinkSettings) -> Result<Self, AdapterError> {
        validate_adapter_id(&adapter_id)?;
        validate_settings(&settings)?;
        let signing_key = settings
            .signing_key_path
            .as_deref()
            .map(load_signing_key)
            .transpose()?;
        if settings.require_signing && signing_key.is_none() {
            return Err(AdapterError::configuration(
                "MAVLink signing is required but no signing key is configured",
            ));
        }
        let replay_timestamps = match (&settings.replay_state_path, signing_key.is_some()) {
            (Some(path), true) => {
                let loaded = load_replay_state(path, settings.system_id)?;
                if loaded.needs_migration {
                    // A successful startup must durably collapse the legacy source-address keys.
                    // Failing this write fails closed instead of running with restart-weak replay
                    // protection.
                    persist_replay_state(path, &loaded.timestamps, settings.system_id)?;
                }
                loaded.timestamps
            }
            (None, true) => {
                return Err(AdapterError::configuration(
                    "MAVLink signing requires a durable replay state path",
                ));
            }
            (Some(_), false) => {
                return Err(AdapterError::configuration(
                    "MAVLink replay state requires a signing key",
                ));
            }
            (None, false) => HashMap::new(),
        };
        Ok(Self {
            adapter_id,
            settings,
            signing_key,
            replay_timestamps: Mutex::new(replay_timestamps),
            source_pins: Mutex::new(HashMap::new()),
            socket: OnceCell::new(),
        })
    }

    async fn receive_socket(&self) -> Result<&UdpSocket, AdapterError> {
        self.socket
            .get_or_try_init(|| async {
                UdpSocket::bind(SocketAddr::new(self.settings.bind_ip, self.settings.port))
                    .await
                    .map_err(|_error| {
                        AdapterError::transient("MAVLink receive socket could not be opened")
                    })
            })
            .await
    }
}

#[async_trait]
impl Adapter for MavlinkAdapter {
    fn descriptor(&self) -> AdapterDescriptor {
        AdapterDescriptor {
            id: self.adapter_id.clone(),
            kind: AdapterKind::Mavlink,
            display_name: "MAVLink".to_owned(),
        }
    }

    async fn poll(&self, context: AdapterPollContext) -> Result<AdapterBatch, AdapterError> {
        if context.max_observations == 0 {
            return Ok(AdapterBatch::default());
        }
        if context.cancellation.is_cancelled() {
            return Err(AdapterError::cancelled());
        }

        let socket = self.receive_socket().await?;
        let window =
            Duration::from_millis(self.settings.receive_window_ms).min(context.remaining());
        if window.is_zero() {
            return Err(AdapterError::transient("MAVLink poll deadline elapsed"));
        }
        let deadline = tokio::time::Instant::now() + window;
        let maximum_components = self.settings.max_peers.min(MAX_PEERS);
        let mut peers: BTreeMap<(u8, u8), PeerObservation> = BTreeMap::new();
        let mut ambiguous_peers = BTreeSet::new();
        let mut invalid_datagrams = 0;
        let mut received_datagrams = 0;
        let mut signing_timestamps = self.replay_timestamps.lock().await;
        let mut source_pins = self.source_pins.lock().await;
        let oldest_timestamp = signing_timestamp(unix_now_ms()?)?
            .saturating_sub(self.settings.signature_max_age_ms.saturating_mul(100));
        signing_timestamps.retain(|_, timestamp| *timestamp >= oldest_timestamp);
        let mut buffer = vec![0_u8; MAX_DATAGRAM_BYTES];

        loop {
            tokio::select! {
                () = context.cancellation.cancelled() => return Err(AdapterError::cancelled()),
                () = tokio::time::sleep_until(deadline) => break,
                result = socket.recv_from(&mut buffer) => {
                    let (size, source) = result
                        .map_err(|_error| AdapterError::transient("MAVLink receive failed"))?;
                    received_datagrams += 1;
                    if !source_allowed(source.ip(), &self.settings.allowed_source_ips) {
                        if received_datagrams >= MAX_DATAGRAMS_PER_POLL {
                            break;
                        }
                        continue;
                    }
                    let now_ms = unix_now_ms()?;
                    let frames = match parse_datagram(
                        &buffer[..size],
                        self.settings.system_id,
                        self.signing_key.as_ref(),
                        self.settings.require_signing,
                        self.settings.signature_max_age_ms,
                        self.settings.signature_future_skew_ms,
                        &mut signing_timestamps,
                        now_ms,
                    ) {
                        Ok(frames) => frames,
                        Err(_untrusted_packet) => {
                            invalid_datagrams += 1;
                            if invalid_datagrams >= MAX_INVALID_DATAGRAMS_PER_POLL
                                || received_datagrams >= MAX_DATAGRAMS_PER_POLL
                            {
                                break;
                            }
                            continue;
                        }
                    };
                    if frames.iter().any(|frame| frame.authenticated)
                        && let Some(path) = &self.settings.replay_state_path
                    {
                        persist_replay_state(
                            path,
                            &signing_timestamps,
                            self.settings.system_id,
                        )?;
                    }
                    for frame in frames {
                        let identity = (frame.system_id, frame.component_id);
                        if ambiguous_peers.contains(&identity) {
                            continue;
                        }
                        if !pin_source(&mut source_pins, identity, source.ip()) {
                            peers.remove(&identity);
                            ambiguous_peers.insert(identity);
                            continue;
                        }
                        peers.insert(identity, PeerObservation { frame, now_ms });
                        if peers.len() >= maximum_components {
                            break;
                        }
                    }
                    if peers.len() >= maximum_components
                        || received_datagrams >= MAX_DATAGRAMS_PER_POLL
                    {
                        break;
                    }
                }
            }
        }

        let observations = aggregate_vehicle_observation(
            &peers,
            self.settings.system_id,
            self.settings.heartbeat_timeout_ms,
        )?
        .into_iter()
        .collect();
        Ok(AdapterBatch { observations })
    }
}

#[derive(Clone, Copy, Debug)]
struct HeartbeatFrame {
    system_id: u8,
    component_id: u8,
    custom_mode: u32,
    vehicle_type: u8,
    autopilot: u8,
    base_mode: u8,
    system_status: u8,
    protocol_version: u8,
    authenticated: bool,
}

#[derive(Clone, Copy)]
struct PeerObservation {
    frame: HeartbeatFrame,
    now_ms: i64,
}

impl PeerObservation {
    fn into_observation(
        self,
        timeout_ms: i64,
        component_ids: &str,
    ) -> Result<AdapterObservation, AdapterError> {
        let frame = self.frame;
        let mut metadata = BTreeMap::new();
        metadata.insert("autopilot".to_owned(), frame.autopilot.to_string());
        metadata.insert("base_mode".to_owned(), frame.base_mode.to_string());
        metadata.insert("component_id".to_owned(), frame.component_id.to_string());
        metadata.insert("component_ids".to_owned(), component_ids.to_owned());
        metadata.insert("custom_mode".to_owned(), frame.custom_mode.to_string());
        metadata.insert(
            "mavlink_version".to_owned(),
            frame.protocol_version.to_string(),
        );
        metadata.insert("system_id".to_owned(), frame.system_id.to_string());
        metadata.insert("system_status".to_owned(), frame.system_status.to_string());
        metadata.insert("transport".to_owned(), "udp_passive".to_owned());
        let observation = AdapterObservation {
            identity: format!("mavlink:{}", frame.system_id),
            display_name: format!("MAVLink system {}", frame.system_id),
            device_kind: device_kind(frame.vehicle_type).to_owned(),
            trust: if frame.authenticated {
                ObservationTrust::Authenticated
            } else {
                ObservationTrust::Observed
            },
            observed_at_ms: self.now_ms,
            expires_at_ms: self
                .now_ms
                .checked_add(timeout_ms)
                .ok_or_else(|| AdapterError::invalid_data("MAVLink freshness overflow"))?,
            sources: vec![ObservedSource {
                id: "mavlink-telemetry".to_owned(),
                label: "MAVLink telemetry".to_owned(),
                category: SourceCategory::Telemetry,
                protocol: "mavlink".to_owned(),
                status: SourceStatus::MetadataOnly,
                renderer_hint: Some(RendererHint::Plot),
                topics: vec![
                    topic("/mavlink/heartbeat", "Heartbeat", "HEARTBEAT"),
                    topic("/mavlink/mode", "Flight mode", "HEARTBEAT.custom_mode"),
                    topic(
                        "/mavlink/system_status",
                        "System status",
                        "HEARTBEAT.system_status",
                    ),
                ],
            }],
            metadata,
        };
        observation.validate()?;
        Ok(observation)
    }
}

fn aggregate_vehicle_observation(
    peers: &BTreeMap<(u8, u8), PeerObservation>,
    system_id: u8,
    timeout_ms: i64,
) -> Result<Option<AdapterObservation>, AdapterError> {
    let Some(mut representative) = peers
        .get(&(system_id, 1))
        .copied()
        .or_else(|| peers.values().next().copied())
    else {
        return Ok(None);
    };
    representative.now_ms = peers
        .values()
        .map(|peer| peer.now_ms)
        .max()
        .unwrap_or(representative.now_ms);
    // A vehicle-level observation is authenticated only if every component used to describe it
    // was authenticated. This prevents an unsigned auxiliary component from inheriting trust.
    representative.frame.authenticated = peers.values().all(|peer| peer.frame.authenticated);
    let component_ids = peers
        .keys()
        .map(|(_, component_id)| component_id.to_string())
        .collect::<Vec<_>>()
        .join(",");
    representative
        .into_observation(timeout_ms, &component_ids)
        .map(Some)
}

fn pin_source(pins: &mut SourcePins, identity: (u8, u8), source: IpAddr) -> bool {
    if let Some(pinned) = pins.get(&identity) {
        *pinned == source
    } else {
        pins.insert(identity, source);
        true
    }
}

fn topic(path: &str, label: &str, message_type: &str) -> ObservedTopic {
    ObservedTopic {
        path: path.to_owned(),
        label: label.to_owned(),
        message_type: Some(message_type.to_owned()),
        qos: None,
        renderer_hint: Some(RendererHint::State),
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "packet verification inputs are intentionally explicit security boundaries"
)]
fn parse_datagram(
    datagram: &[u8],
    expected_system_id: u8,
    signing_key: Option<&[u8; SIGNING_KEY_LEN]>,
    require_signing: bool,
    maximum_age_ms: u64,
    future_skew_ms: u64,
    timestamps: &mut ReplayState,
    now_ms: i64,
) -> Result<Vec<HeartbeatFrame>, AdapterError> {
    let mut frames = Vec::new();
    let mut cursor = 0;
    let mut packets = 0;
    while cursor < datagram.len() && packets < MAX_PACKETS_PER_DATAGRAM {
        let Some(relative) = datagram[cursor..]
            .iter()
            .position(|byte| matches!(*byte, V1_MAGIC | V2_MAGIC))
        else {
            break;
        };
        cursor += relative;
        let packet = parse_packet(&datagram[cursor..])?;
        cursor = cursor
            .checked_add(packet.consumed)
            .ok_or_else(|| AdapterError::invalid_data("MAVLink packet length overflow"))?;
        packets += 1;
        let Some(mut heartbeat) = packet.heartbeat else {
            continue;
        };
        if heartbeat.system_id != expected_system_id {
            continue;
        }
        if let Some(signature) = packet.signature {
            let key = signing_key.ok_or_else(|| {
                AdapterError::invalid_data(
                    "signed MAVLink packet has no configured verification key",
                )
            })?;
            verify_signature(packet.signed_bytes, signature, key)?;
            validate_signature_timestamp(
                heartbeat.system_id,
                heartbeat.component_id,
                signature,
                now_ms,
                maximum_age_ms,
                future_skew_ms,
                timestamps,
            )?;
            heartbeat.authenticated = true;
        } else if require_signing {
            return Err(AdapterError::invalid_data(
                "unsigned MAVLink packet was rejected",
            ));
        }
        frames.push(heartbeat);
    }
    if packets == MAX_PACKETS_PER_DATAGRAM && cursor < datagram.len() {
        return Err(AdapterError::invalid_data(
            "MAVLink datagram packet limit exceeded",
        ));
    }
    Ok(frames)
}

struct ParsedPacket<'a> {
    consumed: usize,
    heartbeat: Option<HeartbeatFrame>,
    signed_bytes: &'a [u8],
    signature: Option<PacketSignature<'a>>,
}

#[derive(Clone, Copy)]
struct PacketSignature<'a> {
    link_id: u8,
    timestamp: u64,
    timestamp_bytes: &'a [u8],
    digest: &'a [u8],
}

fn parse_packet(packet: &[u8]) -> Result<ParsedPacket<'_>, AdapterError> {
    match packet.first().copied() {
        Some(V1_MAGIC) => parse_v1(packet),
        Some(V2_MAGIC) => parse_v2(packet),
        _ => Err(AdapterError::invalid_data("unsupported MAVLink framing")),
    }
}

fn parse_v1(packet: &[u8]) -> Result<ParsedPacket<'_>, AdapterError> {
    if packet.len() < 8 {
        return Err(AdapterError::invalid_data("truncated MAVLink 1 packet"));
    }
    let payload_len = usize::from(packet[1]);
    let consumed = 8_usize
        .checked_add(payload_len)
        .ok_or_else(|| AdapterError::invalid_data("invalid MAVLink 1 length"))?;
    if packet.len() < consumed {
        return Err(AdapterError::invalid_data("truncated MAVLink 1 payload"));
    }
    let message_id = u32::from(packet[5]);
    verify_crc(
        &packet[1..6 + payload_len],
        &packet[6 + payload_len..consumed],
        message_id,
    )?;
    Ok(ParsedPacket {
        consumed,
        heartbeat: heartbeat(
            message_id,
            packet[3],
            packet[4],
            &packet[6..6 + payload_len],
        )?,
        signed_bytes: &packet[..consumed],
        signature: None,
    })
}

fn parse_v2(packet: &[u8]) -> Result<ParsedPacket<'_>, AdapterError> {
    if packet.len() < 12 {
        return Err(AdapterError::invalid_data("truncated MAVLink 2 packet"));
    }
    let payload_len = usize::from(packet[1]);
    let flags = packet[2];
    if flags & !V2_SIGNED != 0 {
        return Err(AdapterError::invalid_data(
            "unsupported MAVLink 2 incompatibility flags",
        ));
    }
    let unsigned_len = 12_usize
        .checked_add(payload_len)
        .ok_or_else(|| AdapterError::invalid_data("invalid MAVLink 2 length"))?;
    let signature_len = if flags & V2_SIGNED == 0 {
        0
    } else {
        SIGNATURE_LEN
    };
    let consumed = unsigned_len
        .checked_add(signature_len)
        .ok_or_else(|| AdapterError::invalid_data("invalid MAVLink 2 signature length"))?;
    if packet.len() < consumed {
        return Err(AdapterError::invalid_data("truncated MAVLink 2 payload"));
    }
    let message_id =
        u32::from(packet[7]) | (u32::from(packet[8]) << 8) | (u32::from(packet[9]) << 16);
    verify_crc(
        &packet[1..10 + payload_len],
        &packet[10 + payload_len..unsigned_len],
        message_id,
    )?;
    let signature = (signature_len != 0).then(|| {
        let bytes = &packet[unsigned_len..consumed];
        PacketSignature {
            link_id: bytes[0],
            timestamp: decode_u48(&bytes[1..7]),
            timestamp_bytes: &bytes[1..7],
            digest: &bytes[7..13],
        }
    });
    Ok(ParsedPacket {
        consumed,
        heartbeat: heartbeat(
            message_id,
            packet[5],
            packet[6],
            &packet[10..10 + payload_len],
        )?,
        signed_bytes: &packet[..unsigned_len],
        signature,
    })
}

fn heartbeat(
    message_id: u32,
    system_id: u8,
    component_id: u8,
    payload: &[u8],
) -> Result<Option<HeartbeatFrame>, AdapterError> {
    if message_id != HEARTBEAT_ID {
        return Ok(None);
    }
    if system_id == 0 || component_id == 0 || payload.len() != HEARTBEAT_LEN {
        return Err(AdapterError::invalid_data(
            "invalid MAVLink HEARTBEAT payload",
        ));
    }
    Ok(Some(HeartbeatFrame {
        system_id,
        component_id,
        custom_mode: u32::from_le_bytes(payload[0..4].try_into().expect("fixed slice")),
        vehicle_type: payload[4],
        autopilot: payload[5],
        base_mode: payload[6],
        system_status: payload[7],
        protocol_version: payload[8],
        authenticated: false,
    }))
}

fn verify_crc(data: &[u8], checksum: &[u8], message_id: u32) -> Result<(), AdapterError> {
    if checksum.len() != 2 {
        return Err(AdapterError::invalid_data(
            "invalid MAVLink checksum length",
        ));
    }
    if message_id != HEARTBEAT_ID {
        return Ok(());
    }
    let mut crc = 0xFFFF;
    for byte in data
        .iter()
        .copied()
        .chain(std::iter::once(HEARTBEAT_CRC_EXTRA))
    {
        crc_accumulate(byte, &mut crc);
    }
    if checksum != crc.to_le_bytes() {
        return Err(AdapterError::invalid_data(
            "MAVLink checksum validation failed",
        ));
    }
    Ok(())
}

fn crc_accumulate(byte: u8, crc: &mut u16) {
    let temporary = byte ^ (*crc as u8);
    let temporary = temporary ^ (temporary << 4);
    *crc = (*crc >> 8)
        ^ (u16::from(temporary) << 8)
        ^ (u16::from(temporary) << 3)
        ^ (u16::from(temporary) >> 4);
}

fn verify_signature(
    bytes: &[u8],
    signature: PacketSignature<'_>,
    key: &[u8; SIGNING_KEY_LEN],
) -> Result<(), AdapterError> {
    let mut digest = Sha256::new();
    digest.update(key);
    digest.update(bytes);
    digest.update([signature.link_id]);
    digest.update(signature.timestamp_bytes);
    let expected = digest.finalize();
    if expected[..6] != *signature.digest {
        return Err(AdapterError::invalid_data(
            "MAVLink signature validation failed",
        ));
    }
    Ok(())
}

fn validate_signature_timestamp(
    system_id: u8,
    component_id: u8,
    signature: PacketSignature<'_>,
    now_ms: i64,
    maximum_age_ms: u64,
    future_skew_ms: u64,
    seen: &mut ReplayState,
) -> Result<(), AdapterError> {
    let now = signing_timestamp(now_ms)?;
    let maximum_age = maximum_age_ms.saturating_mul(100);
    let future_skew = future_skew_ms.saturating_mul(100);
    if signature.timestamp.saturating_add(maximum_age) < now
        || signature.timestamp > now.saturating_add(future_skew)
    {
        return Err(AdapterError::invalid_data(
            "MAVLink signature timestamp is outside the accepted window",
        ));
    }
    let identity = (system_id, component_id, signature.link_id);
    let previous = seen.get(&identity).copied();
    if previous.is_some_and(|previous| signature.timestamp <= previous) {
        return Err(AdapterError::invalid_data(
            "MAVLink signature replay was rejected",
        ));
    }
    if previous.is_none() && seen.len() >= MAX_REPLAY_LINKS {
        return Err(AdapterError::invalid_data(
            "MAVLink replay identity limit was reached",
        ));
    }
    seen.insert(identity, signature.timestamp);
    Ok(())
}

fn signing_timestamp(unix_ms: i64) -> Result<u64, AdapterError> {
    let unix_ms = u64::try_from(unix_ms)
        .map_err(|_error| AdapterError::invalid_data("system clock is invalid"))?;
    unix_ms
        .checked_sub(SIGNING_EPOCH_UNIX_SECONDS * 1_000)
        .map(|elapsed| elapsed.saturating_mul(100))
        .ok_or_else(|| AdapterError::invalid_data("system clock predates MAVLink signing"))
}

fn decode_u48(bytes: &[u8]) -> u64 {
    bytes
        .iter()
        .copied()
        .enumerate()
        .fold(0, |value, (index, byte)| {
            value | (u64::from(byte) << (index * 8))
        })
}

fn load_signing_key(path: &Path) -> Result<[u8; SIGNING_KEY_LEN], AdapterError> {
    if !path.is_absolute() {
        return Err(AdapterError::configuration(
            "MAVLink signing key path must be absolute",
        ));
    }
    // `File::open` follows symlinks, matching the common Edge secret loader. Validate the opened
    // target rather than the link metadata so a symlink cannot bypass type or mode checks.
    let file = fs::File::open(path)
        .map_err(|_error| AdapterError::configuration("MAVLink signing key could not be read"))?;
    let metadata = file
        .metadata()
        .map_err(|_error| AdapterError::configuration("MAVLink signing key could not be read"))?;
    validate_private_regular_file(&metadata, "MAVLink signing key")?;
    if metadata.len() != SIGNING_KEY_LEN as u64 {
        return Err(AdapterError::configuration(
            "MAVLink signing key must contain 32 raw bytes",
        ));
    }
    let mut bytes = Vec::with_capacity(SIGNING_KEY_LEN + 1);
    file.take((SIGNING_KEY_LEN + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|_error| AdapterError::configuration("MAVLink signing key could not be read"))?;
    bytes.try_into().map_err(|_error| {
        AdapterError::configuration("MAVLink signing key must contain 32 raw bytes")
    })
}

fn validate_private_regular_file(metadata: &fs::Metadata, label: &str) -> Result<(), AdapterError> {
    if !metadata.is_file() {
        return Err(AdapterError::configuration(format!(
            "{label} must be a regular file"
        )));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        if metadata.permissions().mode() & 0o077 != 0 {
            return Err(AdapterError::configuration(format!(
                "{label} permissions must not grant group/other access"
            )));
        }
    }
    Ok(())
}

struct LoadedReplayState {
    timestamps: ReplayState,
    needs_migration: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct DurableReplayVersion {
    schema_version: u32,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct DurableReplayStateV1 {
    schema_version: u32,
    records: Vec<DurableReplayRecordV1>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct DurableReplayRecordV1 {
    source: IpAddr,
    system_id: u8,
    component_id: u8,
    link_id: u8,
    timestamp: u64,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct DurableReplayState {
    schema_version: u32,
    system_id: u8,
    records: Vec<DurableReplayRecord>,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct DurableReplayRecord {
    system_id: u8,
    component_id: u8,
    link_id: u8,
    timestamp: u64,
}

fn load_replay_state(
    path: &Path,
    expected_system_id: u8,
) -> Result<LoadedReplayState, AdapterError> {
    let Some((state_path, metadata)) = replay_state_target(path)? else {
        return Ok(LoadedReplayState {
            timestamps: HashMap::new(),
            needs_migration: false,
        });
    };
    if metadata.len() > 1024 * 1024 {
        return Err(AdapterError::configuration(
            "MAVLink replay state exceeds its size limit",
        ));
    }
    let bytes = fs::read(state_path)
        .map_err(|_error| AdapterError::configuration("MAVLink replay state could not be read"))?;
    let version: DurableReplayVersion = serde_json::from_slice(&bytes)
        .map_err(|_error| AdapterError::configuration("MAVLink replay state is invalid"))?;
    match version.schema_version {
        1 => load_legacy_replay_state(&bytes, expected_system_id),
        REPLAY_SCHEMA_VERSION => load_current_replay_state(&bytes, expected_system_id),
        _ => Err(AdapterError::configuration(
            "MAVLink replay state is incompatible",
        )),
    }
}

fn load_legacy_replay_state(
    bytes: &[u8],
    expected_system_id: u8,
) -> Result<LoadedReplayState, AdapterError> {
    let state: DurableReplayStateV1 = serde_json::from_slice(bytes)
        .map_err(|_error| AdapterError::configuration("MAVLink replay state is invalid"))?;
    if state.schema_version != 1 || state.records.len() > MAX_REPLAY_LINKS {
        return Err(AdapterError::configuration(
            "MAVLink replay state is incompatible",
        ));
    }
    let mut legacy_identities = HashSet::new();
    let mut timestamps = HashMap::new();
    for record in state.records {
        let replay_key = (record.system_id, record.component_id, record.link_id);
        if record.system_id == 0
            || record.component_id == 0
            || !is_local(record.source)
            || !legacy_identities.insert((record.source, replay_key))
        {
            return Err(AdapterError::configuration(
                "MAVLink replay state contains an invalid identity",
            ));
        }
        if record.system_id == expected_system_id {
            timestamps
                .entry(replay_key)
                .and_modify(|previous: &mut u64| *previous = (*previous).max(record.timestamp))
                .or_insert(record.timestamp);
        }
    }
    Ok(LoadedReplayState {
        timestamps,
        needs_migration: true,
    })
}

fn load_current_replay_state(
    bytes: &[u8],
    expected_system_id: u8,
) -> Result<LoadedReplayState, AdapterError> {
    let state: DurableReplayState = serde_json::from_slice(bytes)
        .map_err(|_error| AdapterError::configuration("MAVLink replay state is invalid"))?;
    if state.schema_version != REPLAY_SCHEMA_VERSION
        || state.system_id != expected_system_id
        || state.records.len() > MAX_REPLAY_LINKS
    {
        return Err(AdapterError::configuration(
            "MAVLink replay state is incompatible",
        ));
    }
    let mut timestamps = HashMap::new();
    for record in state.records {
        let identity = (record.system_id, record.component_id, record.link_id);
        if record.system_id != expected_system_id
            || record.component_id == 0
            || timestamps.insert(identity, record.timestamp).is_some()
        {
            return Err(AdapterError::configuration(
                "MAVLink replay state contains an invalid identity",
            ));
        }
    }
    Ok(LoadedReplayState {
        timestamps,
        needs_migration: false,
    })
}

fn persist_replay_state(
    path: &Path,
    timestamps: &ReplayState,
    system_id: u8,
) -> Result<(), AdapterError> {
    let state_path = replay_state_target(path)?
        .map(|(target, _metadata)| target)
        .unwrap_or_else(|| path.to_owned());
    if timestamps.len() > MAX_REPLAY_LINKS
        || timestamps
            .keys()
            .any(|(record_system_id, component_id, _)| {
                *record_system_id != system_id || *component_id == 0
            })
    {
        return Err(AdapterError::transient(
            "MAVLink replay state cannot be persisted safely",
        ));
    }
    let mut records = timestamps
        .iter()
        .map(
            |((system_id, component_id, link_id), timestamp)| DurableReplayRecord {
                system_id: *system_id,
                component_id: *component_id,
                link_id: *link_id,
                timestamp: *timestamp,
            },
        )
        .collect::<Vec<_>>();
    records.sort_by_key(|record| (record.system_id, record.component_id, record.link_id));
    let bytes = serde_json::to_vec(&DurableReplayState {
        schema_version: REPLAY_SCHEMA_VERSION,
        system_id,
        records,
    })
    .map_err(|_error| AdapterError::transient("MAVLink replay state could not be encoded"))?;
    AtomicFile::new(&state_path, AllowOverwrite)
        .write(|file| {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt as _;
                file.set_permissions(fs::Permissions::from_mode(0o600))?;
            }
            file.write_all(&bytes)?;
            file.sync_all()
        })
        .map_err(|_error| AdapterError::transient("MAVLink replay state could not be persisted"))
}

fn replay_state_target(path: &Path) -> Result<Option<(PathBuf, fs::Metadata)>, AdapterError> {
    if !path.is_absolute() {
        return Err(AdapterError::configuration(
            "MAVLink replay state path must use an existing absolute directory",
        ));
    }
    let Some(parent) = path.parent() else {
        return Err(AdapterError::configuration(
            "MAVLink replay state path must use an existing absolute directory",
        ));
    };
    let parent_metadata = fs::metadata(parent).map_err(|_error| {
        AdapterError::configuration(
            "MAVLink replay state path must use an existing absolute directory",
        )
    })?;
    if !parent_metadata.is_dir() {
        return Err(AdapterError::configuration(
            "MAVLink replay state path must use an existing absolute directory",
        ));
    }

    let link_metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_error) => {
            return Err(AdapterError::configuration(
                "MAVLink replay state could not be inspected",
            ));
        }
    };
    // Read-only secrets and writable replay state both follow valid symlinks. For atomic state
    // replacement, resolve the link and replace its regular-file target, preserving the link.
    let target = if link_metadata.file_type().is_symlink() {
        fs::canonicalize(path).map_err(|_error| {
            AdapterError::configuration("MAVLink replay state symlink target is invalid")
        })?
    } else {
        path.to_owned()
    };
    let metadata = fs::metadata(&target)
        .map_err(|_error| AdapterError::configuration("MAVLink replay state could not be read"))?;
    validate_private_regular_file(&metadata, "MAVLink replay state")?;
    Ok(Some((target, metadata)))
}

fn validate_adapter_id(value: &str) -> Result<(), AdapterError> {
    if value.is_empty()
        || value.len() > 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    {
        return Err(AdapterError::configuration("invalid MAVLink adapter id"));
    }
    Ok(())
}

fn validate_settings(settings: &MavlinkSettings) -> Result<(), AdapterError> {
    if settings.port == 0 || settings.system_id == 0 || !is_local(settings.bind_ip) {
        return Err(AdapterError::configuration(
            "MAVLink requires a nonzero system id and a private, link-local, or loopback bind interface",
        ));
    }
    if settings.allowed_source_ips.len() > MAX_ALLOWED_SOURCES
        || settings
            .allowed_source_ips
            .iter()
            .any(|address| !is_local(*address))
    {
        return Err(AdapterError::configuration(
            "MAVLink source allowlist is invalid",
        ));
    }
    if !(100..=30_000).contains(&settings.receive_window_ms)
        || !(1_000..=300_000).contains(&settings.heartbeat_timeout_ms)
        || !(1..=MAX_PEERS).contains(&settings.max_peers)
        || !(1_000..=3_600_000).contains(&settings.signature_max_age_ms)
        || settings.signature_future_skew_ms > 300_000
    {
        return Err(AdapterError::configuration(
            "MAVLink resource or freshness limits are invalid",
        ));
    }
    Ok(())
}

fn source_allowed(address: IpAddr, allowlist: &[IpAddr]) -> bool {
    is_local(address) && (allowlist.is_empty() || allowlist.contains(&address))
}

fn is_local(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(value) => value.is_private() || value.is_link_local() || value.is_loopback(),
        IpAddr::V6(value) => {
            value.is_loopback()
                || value.is_unicast_link_local()
                || (value.segments()[0] & 0xFE00) == 0xFC00
        }
    }
}

fn device_kind(vehicle_type: u8) -> &'static str {
    match vehicle_type {
        1 | 19..=29 => "vehicle",
        2..=8 | 13..=15 => "drone",
        10..=12 => "robot",
        _ => "gateway",
    }
}

fn unix_now_ms() -> Result<i64, AdapterError> {
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_error| AdapterError::transient("system clock is invalid"))?;
    i64::try_from(elapsed.as_millis())
        .map_err(|_error| AdapterError::transient("system clock is invalid"))
}

const fn default_port() -> u16 {
    14_550
}
const fn default_receive_window_ms() -> u64 {
    1_500
}
const fn default_heartbeat_timeout_ms() -> i64 {
    5_000
}
const fn default_max_peers() -> usize {
    64
}
const fn default_signature_max_age_ms() -> u64 {
    300_000
}
const fn default_signature_future_skew_ms() -> u64 {
    60_000
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio_util::sync::CancellationToken;

    const KEY: [u8; 32] = [0x5A; 32];

    fn payload() -> [u8; 9] {
        [7, 0, 0, 0, 2, 3, 0x81, 4, 3]
    }

    fn v1(system_id: u8, component_id: u8) -> Vec<u8> {
        let mut packet = vec![V1_MAGIC, 9, 1, system_id, component_id, 0];
        packet.extend_from_slice(&payload());
        append_crc(&mut packet);
        packet
    }

    fn v2(system_id: u8, component_id: u8, timestamp: u64) -> Vec<u8> {
        let mut packet = vec![
            V2_MAGIC,
            9,
            V2_SIGNED,
            0,
            1,
            system_id,
            component_id,
            0,
            0,
            0,
        ];
        packet.extend_from_slice(&payload());
        append_crc(&mut packet);
        let link_id = 9;
        let timestamp_bytes = timestamp.to_le_bytes();
        let timestamp_bytes = &timestamp_bytes[..6];
        let mut digest = Sha256::new();
        digest.update(KEY);
        digest.update(&packet);
        digest.update([link_id]);
        digest.update(timestamp_bytes);
        let digest = digest.finalize();
        packet.push(link_id);
        packet.extend_from_slice(timestamp_bytes);
        packet.extend_from_slice(&digest[..6]);
        packet
    }

    fn append_crc(packet: &mut Vec<u8>) {
        let mut crc = 0xFFFF;
        for byte in packet[1..]
            .iter()
            .copied()
            .chain(std::iter::once(HEARTBEAT_CRC_EXTRA))
        {
            crc_accumulate(byte, &mut crc);
        }
        packet.extend_from_slice(&crc.to_le_bytes());
    }

    #[cfg(unix)]
    fn set_private(path: &Path) {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
    }

    #[cfg(not(unix))]
    fn set_private(_path: &Path) {}

    #[test]
    fn signing_key_requires_an_exact_private_regular_file() {
        let directory = tempfile::tempdir().unwrap();
        assert!(load_signing_key(directory.path()).is_err());

        let path = directory.path().join("mavlink-signing.key");
        fs::write(&path, [0x5A; SIGNING_KEY_LEN - 1]).unwrap();
        set_private(&path);
        assert!(load_signing_key(&path).is_err());

        fs::write(&path, KEY).unwrap();
        set_private(&path);
        assert_eq!(load_signing_key(&path).unwrap(), KEY);
    }

    #[cfg(unix)]
    #[test]
    fn signing_key_follows_only_a_private_regular_symlink_target() {
        use std::os::unix::fs::{PermissionsExt as _, symlink};

        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("target.key");
        let link = directory.path().join("signing.key");
        fs::write(&target, KEY).unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(0o600)).unwrap();
        symlink(&target, &link).unwrap();
        assert_eq!(load_signing_key(&link).unwrap(), KEY);

        fs::set_permissions(&target, fs::Permissions::from_mode(0o640)).unwrap();
        assert!(load_signing_key(&link).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn permissive_signing_key_and_replay_state_fail_closed() {
        use std::os::unix::fs::PermissionsExt as _;

        let directory = tempfile::tempdir().unwrap();
        let key_path = directory.path().join("signing.key");
        fs::write(&key_path, KEY).unwrap();
        fs::set_permissions(&key_path, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(load_signing_key(&key_path).is_err());

        let state_path = directory.path().join("replay.json");
        persist_replay_state(&state_path, &HashMap::from([((42, 1, 9), 100)]), 42).unwrap();
        fs::set_permissions(&state_path, fs::Permissions::from_mode(0o640)).unwrap();
        assert!(load_replay_state(&state_path, 42).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn replay_state_atomic_update_preserves_a_private_symlink() {
        use std::os::unix::fs::symlink;

        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("replay-target.json");
        let link = directory.path().join("replay.json");
        persist_replay_state(&target, &HashMap::from([((42, 1, 9), 100)]), 42).unwrap();
        symlink(&target, &link).unwrap();

        persist_replay_state(&link, &HashMap::from([((42, 1, 9), 200)]), 42).unwrap();

        assert!(
            fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(
            load_replay_state(&link, 42).unwrap().timestamps,
            HashMap::from([((42, 1, 9), 200)])
        );
    }

    #[test]
    fn v1_crc_and_v2_signature_are_validated() {
        let now_ms = 2_000_000_000_000;
        let mut seen = HashMap::new();
        let unsigned = parse_datagram(
            &v1(1, 1),
            1,
            None,
            false,
            300_000,
            60_000,
            &mut seen,
            now_ms,
        )
        .expect("v1 parses");
        assert!(!unsigned[0].authenticated);
        let signed_packet = v2(1, 1, signing_timestamp(now_ms).unwrap());
        let signed = parse_datagram(
            &signed_packet,
            1,
            Some(&KEY),
            true,
            300_000,
            60_000,
            &mut seen,
            now_ms,
        )
        .expect("v2 signature verifies");
        assert!(signed[0].authenticated);
        assert!(
            parse_datagram(
                &signed_packet,
                1,
                Some(&KEY),
                true,
                300_000,
                60_000,
                &mut seen,
                now_ms,
            )
            .is_err()
        );
    }

    #[test]
    fn corrupt_and_unsigned_required_packets_fail_closed() {
        let mut corrupt = v1(1, 1);
        corrupt[8] ^= 1;
        let mut seen = HashMap::new();
        assert!(
            parse_datagram(
                &corrupt,
                1,
                None,
                false,
                300_000,
                60_000,
                &mut seen,
                2_000_000_000_000,
            )
            .is_err()
        );
        assert!(
            parse_datagram(
                &v1(1, 1),
                1,
                Some(&KEY),
                true,
                300_000,
                60_000,
                &mut seen,
                2_000_000_000_000,
            )
            .is_err()
        );
    }

    #[test]
    fn replay_state_survives_an_agent_restart() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("mavlink-replay.json");
        let now_ms = 2_000_000_000_000;
        let timestamp = signing_timestamp(now_ms).unwrap();
        let packet = v2(42, 1, timestamp);
        let mut before_restart = HashMap::new();
        parse_datagram(
            &packet,
            42,
            Some(&KEY),
            true,
            300_000,
            60_000,
            &mut before_restart,
            now_ms,
        )
        .expect("first delivery is accepted");
        persist_replay_state(&path, &before_restart, 42).expect("state persists atomically");

        let mut after_restart = load_replay_state(&path, 42)
            .expect("state reloads")
            .timestamps;
        assert!(
            parse_datagram(
                &packet,
                42,
                Some(&KEY),
                true,
                300_000,
                60_000,
                &mut after_restart,
                now_ms,
            )
            .is_err(),
            "a restarted agent must reject the prior packet"
        );
    }

    #[test]
    fn signed_replay_is_rejected_across_source_ip_changes() {
        let now_ms = 2_000_000_000_000;
        let packet = v2(42, 1, signing_timestamp(now_ms).unwrap());
        let first_source = "127.0.0.1".parse().unwrap();
        let routed_source = "127.0.0.2".parse().unwrap();
        let mut pins = HashMap::new();
        let mut seen = HashMap::new();

        assert!(pin_source(&mut pins, (42, 1), first_source));
        parse_datagram(
            &packet,
            42,
            Some(&KEY),
            true,
            300_000,
            60_000,
            &mut seen,
            now_ms,
        )
        .expect("first delivery is accepted");
        assert!(!pin_source(&mut pins, (42, 1), routed_source));
        assert!(
            parse_datagram(
                &packet,
                42,
                Some(&KEY),
                true,
                300_000,
                60_000,
                &mut seen,
                now_ms,
            )
            .is_err(),
            "source address must not partition anti-replay state"
        );
    }

    #[test]
    fn wrong_system_id_is_ignored_before_replay_state_changes() {
        let now_ms = 2_000_000_000_000;
        let mut seen = HashMap::new();
        let frames = parse_datagram(
            &v2(43, 1, signing_timestamp(now_ms).unwrap()),
            42,
            Some(&KEY),
            true,
            300_000,
            60_000,
            &mut seen,
            now_ms,
        )
        .expect("a different vehicle is ignored");
        assert!(frames.is_empty());
        assert!(seen.is_empty());
    }

    #[test]
    fn legacy_replay_state_migrates_by_taking_cross_ip_maximum() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("mavlink-replay.json");
        let legacy = serde_json::json!({
            "schemaVersion": 1,
            "records": [
                {
                    "source": "127.0.0.1",
                    "systemId": 42,
                    "componentId": 1,
                    "linkId": 9,
                    "timestamp": 100
                },
                {
                    "source": "127.0.0.2",
                    "systemId": 42,
                    "componentId": 1,
                    "linkId": 9,
                    "timestamp": 200
                },
                {
                    "source": "127.0.0.3",
                    "systemId": 99,
                    "componentId": 1,
                    "linkId": 9,
                    "timestamp": 300
                }
            ]
        });
        fs::write(&path, serde_json::to_vec(&legacy).unwrap()).unwrap();

        let migrated = load_replay_state(&path, 42).expect("legacy state loads safely");
        assert!(migrated.needs_migration);
        assert_eq!(migrated.timestamps, HashMap::from([((42, 1, 9), 200)]));
        persist_replay_state(&path, &migrated.timestamps, 42).unwrap();

        let encoded: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(encoded["schemaVersion"], REPLAY_SCHEMA_VERSION);
        assert_eq!(encoded["systemId"], 42);
        assert!(encoded["records"][0].get("source").is_none());
        assert!(!load_replay_state(&path, 42).unwrap().needs_migration);
        assert!(load_replay_state(&path, 43).is_err());
    }

    fn test_settings(port: u16, system_id: u8) -> MavlinkSettings {
        MavlinkSettings {
            bind_ip: "127.0.0.1".parse().unwrap(),
            port,
            system_id,
            allowed_source_ips: Vec::new(),
            require_signing: false,
            signing_key_path: None,
            replay_state_path: None,
            receive_window_ms: 150,
            heartbeat_timeout_ms: 5_000,
            max_peers: 4,
            signature_max_age_ms: 300_000,
            signature_future_skew_ms: 60_000,
        }
    }

    #[tokio::test]
    async fn udp_poll_returns_metadata_only() {
        let reservation = UdpSocket::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let port = reservation.local_addr().unwrap().port();
        drop(reservation);
        let adapter = MavlinkAdapter::new("mav-main".to_owned(), test_settings(port, 7)).unwrap();
        let sender = UdpSocket::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let task = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(25)).await;
            sender
                .send_to(&v1(7, 1), (std::net::Ipv4Addr::LOCALHOST, port))
                .await
                .unwrap();
        });
        let batch = adapter
            .poll(AdapterPollContext {
                cancellation: CancellationToken::new(),
                deadline: tokio::time::Instant::now() + Duration::from_secs(1),
                max_observations: 4,
            })
            .await
            .expect("poll succeeds");
        task.await.unwrap();
        assert_eq!(batch.observations.len(), 1);
        assert_eq!(batch.observations[0].identity, "mavlink:7");
        assert_eq!(
            batch.observations[0].sources[0].status,
            SourceStatus::MetadataOnly
        );
    }

    #[tokio::test]
    async fn socket_queues_heartbeats_between_poll_windows() {
        let reservation = UdpSocket::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let port = reservation.local_addr().unwrap().port();
        drop(reservation);
        let adapter = MavlinkAdapter::new("mav-main".to_owned(), test_settings(port, 7)).unwrap();

        let first = adapter
            .poll(AdapterPollContext {
                cancellation: CancellationToken::new(),
                deadline: tokio::time::Instant::now() + Duration::from_secs(1),
                max_observations: 1,
            })
            .await
            .expect("initial poll binds the lifetime socket");
        assert!(first.observations.is_empty());
        assert!(adapter.socket.get().is_some());

        let sender = UdpSocket::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        sender
            .send_to(&v1(7, 1), (std::net::Ipv4Addr::LOCALHOST, port))
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(25)).await;

        let second = adapter
            .poll(AdapterPollContext {
                cancellation: CancellationToken::new(),
                deadline: tokio::time::Instant::now() + Duration::from_secs(1),
                max_observations: 1,
            })
            .await
            .expect("next poll drains the retained socket queue");
        assert_eq!(second.observations.len(), 1);
    }

    #[tokio::test]
    async fn poll_aggregates_components_and_ignores_other_vehicles() {
        let reservation = UdpSocket::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let port = reservation.local_addr().unwrap().port();
        drop(reservation);
        let mut settings = test_settings(port, 7);
        settings.max_peers = 2;
        let adapter = MavlinkAdapter::new("mav-main".to_owned(), settings).unwrap();
        let sender = UdpSocket::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let task = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(25)).await;
            sender
                .send_to(&v1(8, 1), (std::net::Ipv4Addr::LOCALHOST, port))
                .await
                .unwrap();
            sender
                .send_to(&v1(7, 100), (std::net::Ipv4Addr::LOCALHOST, port))
                .await
                .unwrap();
            sender
                .send_to(&v1(7, 1), (std::net::Ipv4Addr::LOCALHOST, port))
                .await
                .unwrap();
        });
        let batch = adapter
            .poll(AdapterPollContext {
                cancellation: CancellationToken::new(),
                deadline: tokio::time::Instant::now() + Duration::from_secs(1),
                max_observations: 1,
            })
            .await
            .expect("poll succeeds");
        task.await.unwrap();

        assert_eq!(batch.observations.len(), 1);
        assert_eq!(batch.observations[0].identity, "mavlink:7");
        assert_eq!(batch.observations[0].metadata["component_ids"], "1,100");
        assert_eq!(batch.observations[0].metadata["component_id"], "1");
    }

    #[tokio::test]
    async fn cancellation_is_fail_fast() {
        let cancellation = CancellationToken::new();
        cancellation.cancel();
        let adapter = MavlinkAdapter::new("mav-main".to_owned(), test_settings(14_550, 7)).unwrap();
        assert!(
            adapter
                .poll(AdapterPollContext {
                    cancellation,
                    deadline: tokio::time::Instant::now() + Duration::from_secs(1),
                    max_observations: 4,
                })
                .await
                .is_err()
        );
    }
}
