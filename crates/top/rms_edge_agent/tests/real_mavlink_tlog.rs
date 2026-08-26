//! Opt-in integration test for an official `DroneKit` telemetry log.
//!
//! The external fixture is intentionally not redistributed with RMS.
//! Download `examples/flight_replay/flight.tlog` from the official `DroneKit` repository and set
//! `RMS_REAL_MAVLINK_TLOG` to its absolute path before running this ignored test.

use std::{
    env,
    fs::File,
    io::{Read as _, Take},
    net::Ipv4Addr,
    path::{Path, PathBuf},
    time::Duration,
};

use rms_edge_agent::{
    Adapter as _, AdapterKind, AdapterPollContext, ObservationTrust, SourceStatus,
    adapter::mavlink::MavlinkAdapter, config::AdapterConfig,
};
use serde_json::json;
use sha2::{Digest as _, Sha256};
use tokio::net::UdpSocket;
use tokio_util::sync::CancellationToken;

const FIXTURE_ENV: &str = "RMS_REAL_MAVLINK_TLOG";
const MAVLINK_V1_MAGIC: u8 = 0xFE;
const MAVLINK_V1_MIN_FRAME_BYTES: usize = 8;
const MAVLINK_V1_HEARTBEAT_MESSAGE_ID: u8 = 0;
const MAVLINK_V1_HEARTBEAT_PAYLOAD_BYTES: usize = 9;
const MAV_MODE_FLAG_SAFETY_ARMED: u8 = 0x80;
const TLOG_TIMESTAMP_BYTES: usize = 8;
const MAX_TLOG_BYTES: u64 = 16 * 1024 * 1024;
const MAX_TLOG_RECORDS: usize = 1_000_000;

const EXPECTED_FIXTURE_BYTES: usize = 2_723_840;
const EXPECTED_FIXTURE_SHA256: &str =
    "434c0492f77c3afe99bcfe05c03bd09a65e2125cb0c4407ba9e32577b161adce";
const EXPECTED_COMPLETE_FRAMES: usize = 76_791;
const EXPECTED_SYSTEM_ONE_HEARTBEATS: usize = 1_047;
const EXPECTED_ARMED_HEARTBEATS: usize = 424;
const EXPECTED_TRUNCATED_TAIL_BYTES: usize = 19;

struct ParsedTlog {
    complete_frames: usize,
    system_one_heartbeats: usize,
    armed_heartbeats: usize,
    first_armed_heartbeat: Vec<u8>,
    truncated_tail_bytes: usize,
}

#[tokio::test]
#[ignore = "requires RMS_REAL_MAVLINK_TLOG pointing to the official external DroneKit fixture"]
async fn official_dronekit_flight_tlog_is_observed_by_public_adapter() {
    let fixture_path = env::var_os(FIXTURE_ENV)
        .map(PathBuf::from)
        .expect("RMS_REAL_MAVLINK_TLOG must point to the downloaded DroneKit flight.tlog");
    assert!(
        Path::new(&fixture_path).is_absolute(),
        "RMS_REAL_MAVLINK_TLOG must be an absolute path"
    );

    let fixture = read_bounded_fixture(Path::new(&fixture_path));
    assert_eq!(fixture.len(), EXPECTED_FIXTURE_BYTES);
    assert_eq!(
        format!("{:x}", Sha256::digest(&fixture)),
        EXPECTED_FIXTURE_SHA256,
        "the external fixture does not match the reviewed official DroneKit log"
    );

    let parsed = parse_tlog(&fixture).expect("the reviewed DroneKit tlog must parse safely");
    assert_eq!(parsed.complete_frames, EXPECTED_COMPLETE_FRAMES);
    assert_eq!(parsed.system_one_heartbeats, EXPECTED_SYSTEM_ONE_HEARTBEATS);
    assert_eq!(parsed.armed_heartbeats, EXPECTED_ARMED_HEARTBEATS);
    assert_eq!(
        parsed.truncated_tail_bytes, EXPECTED_TRUNCATED_TAIL_BYTES,
        "the reviewed log ends with one bounded, incomplete final record"
    );

    let reservation = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0))
        .await
        .expect("a loopback UDP port is available");
    let port = reservation
        .local_addr()
        .expect("the loopback socket has an address")
        .port();
    drop(reservation);

    let adapter = MavlinkAdapter::from_config(&AdapterConfig {
        id: "real-dronekit-flight".to_owned(),
        kind: AdapterKind::Mavlink,
        enabled: true,
        poll_interval_ms: 1_000,
        settings: json!({
            "bindIp": Ipv4Addr::LOCALHOST,
            "port": port,
            "systemId": 1,
            "allowedSourceIps": [Ipv4Addr::LOCALHOST],
            "requireSigning": false,
            "receiveWindowMs": 750,
            "heartbeatTimeoutMs": 5_000,
            "maxPeers": 1,
        }),
    })
    .expect("the public MAVLink adapter accepts the test configuration");

    // Keep the exact source frame intact: the tlog timestamp prefix is not sent over MAVLink UDP.
    let replay_frame = parsed.first_armed_heartbeat;
    let sender = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(50)).await;
        let socket = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .expect("the replay sender binds on loopback");
        let sent = socket
            .send_to(&replay_frame, (Ipv4Addr::LOCALHOST, port))
            .await
            .expect("the original heartbeat frame is replayed on loopback");
        assert_eq!(sent, replay_frame.len());
    });

    let batch = adapter
        .poll(AdapterPollContext {
            cancellation: CancellationToken::new(),
            deadline: tokio::time::Instant::now() + Duration::from_secs(2),
            max_observations: 1,
        })
        .await
        .expect("the public adapter observes the real heartbeat");
    sender.await.expect("the replay task completes");

    assert_eq!(batch.observations.len(), 1);
    let observation = &batch.observations[0];
    assert_eq!(observation.identity, "mavlink:1");
    assert_eq!(observation.trust, ObservationTrust::Observed);
    assert_eq!(observation.metadata["component_id"], "1");
    assert_eq!(observation.metadata["component_ids"], "1");
    let base_mode = observation.metadata["base_mode"]
        .parse::<u8>()
        .expect("base_mode metadata is numeric");
    assert_ne!(
        base_mode & MAV_MODE_FLAG_SAFETY_ARMED,
        0,
        "the selected real heartbeat is armed"
    );
    assert_eq!(observation.sources[0].status, SourceStatus::MetadataOnly);
}

fn read_bounded_fixture(path: &Path) -> Vec<u8> {
    let file = File::open(path).expect("the external DroneKit fixture is readable");
    let file_length = file
        .metadata()
        .expect("the external fixture has metadata")
        .len();
    assert!(file_length > 0, "the external fixture must not be empty");
    assert!(
        file_length <= MAX_TLOG_BYTES,
        "the external fixture exceeds the integration-test size cap"
    );

    let mut bytes = Vec::with_capacity(
        usize::try_from(file_length).expect("the bounded fixture length fits in memory"),
    );
    let mut reader: Take<File> = file.take(MAX_TLOG_BYTES + 1);
    reader
        .read_to_end(&mut bytes)
        .expect("the external fixture can be read");
    assert!(
        bytes.len() as u64 <= MAX_TLOG_BYTES,
        "the fixture grew while it was being read"
    );
    assert_eq!(
        bytes.len() as u64,
        file_length,
        "the fixture changed while it was being read"
    );
    bytes
}

fn parse_tlog(bytes: &[u8]) -> Result<ParsedTlog, String> {
    let mut offset = 0;
    let mut previous_timestamp = None;
    let mut complete_frames = 0;
    let mut system_one_heartbeats = 0;
    let mut armed_heartbeats = 0;
    let mut first_armed_heartbeat = None;
    let mut truncated_tail_bytes = 0;

    while offset < bytes.len() {
        if complete_frames >= MAX_TLOG_RECORDS {
            return Err("the tlog exceeds the record-count cap".to_owned());
        }
        let record_start = offset;
        let timestamp_end = offset
            .checked_add(TLOG_TIMESTAMP_BYTES)
            .ok_or_else(|| "the timestamp offset overflowed".to_owned())?;
        let timestamp_bytes = bytes
            .get(offset..timestamp_end)
            .ok_or_else(|| "the final tlog timestamp prefix is incomplete".to_owned())?;
        let timestamp = u64::from_be_bytes(
            timestamp_bytes
                .try_into()
                .map_err(|_error| "the timestamp prefix has the wrong width".to_owned())?,
        );
        if previous_timestamp.is_some_and(|previous| timestamp < previous) {
            return Err("the tlog timestamps are not monotonic".to_owned());
        }
        previous_timestamp = Some(timestamp);
        offset = timestamp_end;

        if bytes.get(offset).copied() != Some(MAVLINK_V1_MAGIC) {
            return Err("a tlog record does not start with MAVLink v1 magic".to_owned());
        }
        let payload_bytes = usize::from(
            *bytes
                .get(offset + 1)
                .ok_or_else(|| "the final MAVLink header is incomplete".to_owned())?,
        );
        let frame_bytes = payload_bytes
            .checked_add(MAVLINK_V1_MIN_FRAME_BYTES)
            .ok_or_else(|| "the MAVLink frame length overflowed".to_owned())?;
        let frame_end = offset
            .checked_add(frame_bytes)
            .ok_or_else(|| "the MAVLink frame offset overflowed".to_owned())?;
        let Some(frame) = bytes.get(offset..frame_end) else {
            // Real telemetry capture can stop between writes. Only an incomplete final frame is
            // tolerated; the parser never searches for a later magic byte or processes a prefix.
            truncated_tail_bytes = bytes.len() - record_start;
            break;
        };

        let system_id = frame[3];
        let message_id = frame[5];
        if system_id == 1 && message_id == MAVLINK_V1_HEARTBEAT_MESSAGE_ID {
            if payload_bytes != MAVLINK_V1_HEARTBEAT_PAYLOAD_BYTES {
                return Err("a HEARTBEAT frame has an invalid payload length".to_owned());
            }
            system_one_heartbeats += 1;
            let base_mode = frame[12];
            if base_mode & MAV_MODE_FLAG_SAFETY_ARMED != 0 {
                armed_heartbeats += 1;
                first_armed_heartbeat.get_or_insert_with(|| frame.to_vec());
            }
        }

        complete_frames += 1;
        offset = frame_end;
    }

    let first_armed_heartbeat = first_armed_heartbeat
        .ok_or_else(|| "the fixture contains no armed system-1 HEARTBEAT".to_owned())?;
    Ok(ParsedTlog {
        complete_frames,
        system_one_heartbeats,
        armed_heartbeats,
        first_armed_heartbeat,
        truncated_tail_bytes,
    })
}
