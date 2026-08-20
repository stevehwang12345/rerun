use std::{collections::BTreeMap, sync::Arc, time::SystemTime};

use tokio::sync::{RwLock, watch};
use uuid::Uuid;

use crate::domain::{
    AccessMode, CommandReceipt, ControlLease, DataAssignment, DataSource, DataVisibility, Device,
    DeviceAssignment, Integration, LiveSession, Project, Recording, RecordingProjectSnapshot,
    ReplaySession, SendCommandRequest, Topic,
};

pub(crate) const SAMPLE_RRD_URL: &str = "/rerun/fixture/spatial3d.rrd";

#[derive(Clone)]
pub struct AppState {
    pub(crate) catalog: Arc<RwLock<Catalog>>,
}

pub(crate) struct Catalog {
    pub(crate) snapshot_version: u64,
    pub(crate) integrations: BTreeMap<String, Integration>,
    pub(crate) devices: BTreeMap<String, Device>,
    pub(crate) data_sources: BTreeMap<String, DataSource>,
    pub(crate) projects: BTreeMap<String, Project>,
    pub(crate) device_assignments: BTreeMap<String, DeviceAssignment>,
    pub(crate) data_assignments: BTreeMap<String, DataAssignment>,
    pub(crate) topics_by_data_source: BTreeMap<String, Vec<Topic>>,
    pub(crate) recordings: BTreeMap<String, Recording>,
    pub(crate) live_sessions: BTreeMap<String, LiveSession>,
    pub(crate) live_session_shutdown: BTreeMap<String, watch::Sender<bool>>,
    pub(crate) replay_sessions: BTreeMap<String, ReplaySession>,
    pub(crate) leases: BTreeMap<String, ControlLease>,
    pub(crate) lease_epochs: BTreeMap<String, u64>,
    pub(crate) command_receipts: BTreeMap<String, (SendCommandRequest, CommandReceipt)>,
}

impl AppState {
    /// Creates the deterministic local-development catalog.
    pub fn fixture() -> Self {
        Self {
            catalog: Arc::new(RwLock::new(Catalog::fixture())),
        }
    }

    /// Creates an empty catalog for embedding or focused tests.
    pub fn empty() -> Self {
        Self {
            catalog: Arc::new(RwLock::new(Catalog::empty())),
        }
    }
}

impl Default for AppState {
    fn default() -> Self {
        Self::fixture()
    }
}

impl Catalog {
    fn empty() -> Self {
        Self {
            snapshot_version: 0,
            integrations: BTreeMap::new(),
            devices: BTreeMap::new(),
            data_sources: BTreeMap::new(),
            projects: BTreeMap::new(),
            device_assignments: BTreeMap::new(),
            data_assignments: BTreeMap::new(),
            topics_by_data_source: BTreeMap::new(),
            recordings: BTreeMap::new(),
            live_sessions: BTreeMap::new(),
            live_session_shutdown: BTreeMap::new(),
            replay_sessions: BTreeMap::new(),
            leases: BTreeMap::new(),
            lease_epochs: BTreeMap::new(),
            command_receipts: BTreeMap::new(),
        }
    }

    fn fixture() -> Self {
        let mut catalog = Self::empty();
        catalog.snapshot_version = 12;

        let observed_at = "2026-08-20T09:32:08+09:00".to_owned();
        let integration = Integration {
            id: "integration-logistics".to_owned(),
            organization_id: "org-rms".to_owned(),
            name: "물류 ROS 2".to_owned(),
            kind: "ros2".to_owned(),
            status: "connected".to_owned(),
            endpoint_label: "A동 Edge Gateway".to_owned(),
            last_health_at: observed_at.clone(),
            created_at: "2026-08-01T00:00:00+09:00".to_owned(),
            resource_version: 1,
        };
        catalog
            .integrations
            .insert(integration.id.clone(), integration);

        let drone_integration = Integration {
            id: "integration-inspection".to_owned(),
            organization_id: "org-rms".to_owned(),
            name: "점검 MAVLink".to_owned(),
            kind: "mavlink".to_owned(),
            status: "connected".to_owned(),
            endpoint_label: "야외 Drone Gateway".to_owned(),
            last_health_at: observed_at.clone(),
            created_at: "2026-08-02T00:00:00+09:00".to_owned(),
            resource_version: 1,
        };
        catalog
            .integrations
            .insert(drone_integration.id.clone(), drone_integration);

        for device in [
            Device {
                id: "robot-07".to_owned(),
                organization_id: "org-rms".to_owned(),
                integration_id: "integration-logistics".to_owned(),
                name: "Robot-07".to_owned(),
                kind: "robot".to_owned(),
                status: "online".to_owned(),
                health: "normal".to_owned(),
                operation_mode: "자율 운행".to_owned(),
                battery_percent: Some(78),
                task_name: "Bay 3 이동".to_owned(),
                task_progress: 62,
                last_seen_at: observed_at.clone(),
                state_version: 142,
            },
            Device {
                id: "robot-12".to_owned(),
                organization_id: "org-rms".to_owned(),
                integration_id: "integration-logistics".to_owned(),
                name: "Robot-12".to_owned(),
                kind: "robot".to_owned(),
                status: "degraded".to_owned(),
                health: "attention".to_owned(),
                operation_mode: "대기".to_owned(),
                battery_percent: Some(41),
                task_name: "충전 위치 이동".to_owned(),
                task_progress: 18,
                last_seen_at: "2026-08-20T09:32:05+09:00".to_owned(),
                state_version: 87,
            },
            Device {
                id: "robot-21".to_owned(),
                organization_id: "org-rms".to_owned(),
                integration_id: "integration-logistics".to_owned(),
                name: "Robot-21".to_owned(),
                kind: "robot".to_owned(),
                status: "offline".to_owned(),
                health: "restricted".to_owned(),
                operation_mode: "점검".to_owned(),
                battery_percent: Some(0),
                task_name: "정비 중".to_owned(),
                task_progress: 0,
                last_seen_at: "2026-08-20T08:51:00+09:00".to_owned(),
                state_version: 31,
            },
            Device {
                id: "drone-03".to_owned(),
                organization_id: "org-rms".to_owned(),
                integration_id: "integration-inspection".to_owned(),
                name: "Drone-03".to_owned(),
                kind: "drone".to_owned(),
                status: "online".to_owned(),
                health: "normal".to_owned(),
                operation_mode: "임무 대기".to_owned(),
                battery_percent: Some(86),
                task_name: "동측 패널 점검".to_owned(),
                task_progress: 0,
                last_seen_at: "2026-08-20T09:31:59+09:00".to_owned(),
                state_version: 55,
            },
        ] {
            catalog.devices.insert(device.id.clone(), device);
        }

        for source in [
            fixture_source(
                "robot-07-source",
                "integration-logistics",
                "robot-07",
                "Robot-07 실시간",
                &["pose", "front-camera", "velocity", "battery", "planner"],
            ),
            fixture_source(
                "robot-12-source",
                "integration-logistics",
                "robot-12",
                "Robot-12 실시간",
                &["pose", "front-camera", "velocity", "battery", "planner"],
            ),
            fixture_source(
                "robot-21-source",
                "integration-logistics",
                "robot-21",
                "Robot-21 실시간",
                &["pose", "front-camera", "velocity", "battery"],
            ),
            fixture_source(
                "drone-03-source",
                "integration-inspection",
                "drone-03",
                "Drone-03 실시간",
                &["pose", "front-camera", "altitude", "battery", "planner"],
            ),
        ] {
            catalog.topics_by_data_source.insert(
                source.id.clone(),
                topics_for_source(&source.id, &source.device_id, &source.topic_ids),
            );
            catalog.data_sources.insert(source.id.clone(), source);
        }

        for project in [
            Project {
                id: "project-logistics".to_owned(),
                organization_id: "org-rms".to_owned(),
                name: "물류 자동화".to_owned(),
                description: "A동 물류 로봇 운영".to_owned(),
                status: "active".to_owned(),
                device_count: 3,
                online_device_count: 1,
                created_at: "2026-08-01T00:00:00+09:00".to_owned(),
                resource_version: 1,
            },
            Project {
                id: "project-inspection".to_owned(),
                organization_id: "org-rms".to_owned(),
                name: "시설 점검".to_owned(),
                description: "야외 설비 드론 점검".to_owned(),
                status: "standby".to_owned(),
                device_count: 1,
                online_device_count: 1,
                created_at: "2026-08-01T00:00:00+09:00".to_owned(),
                resource_version: 1,
            },
        ] {
            catalog.projects.insert(project.id.clone(), project);
        }

        for (project_id, device_id, access_mode) in [
            ("project-logistics", "robot-07", AccessMode::Control),
            ("project-logistics", "robot-12", AccessMode::Observe),
            ("project-logistics", "robot-21", AccessMode::Observe),
            ("project-inspection", "drone-03", AccessMode::Control),
        ] {
            let assignment = fixture_device_assignment(project_id, device_id, access_mode);
            catalog
                .device_assignments
                .insert(assignment.id.clone(), assignment);
        }
        for (project_id, source_id) in [
            ("project-logistics", "robot-07-source"),
            ("project-logistics", "robot-12-source"),
            ("project-logistics", "robot-21-source"),
            ("project-inspection", "drone-03-source"),
        ] {
            let assignment = fixture_data_assignment(project_id, source_id);
            catalog
                .data_assignments
                .insert(assignment.id.clone(), assignment);
        }

        for recording in [
            fixture_recording(
                "recording-robot-07-incident",
                "project-logistics",
                "물류 자동화",
                "robot-07",
                "robot-07-source",
                "08:42 경로 이탈",
                10,
            ),
            fixture_recording(
                "recording-robot-21-maintenance",
                "project-logistics",
                "물류 자동화",
                "robot-21",
                "robot-21-source",
                "정비 전 운행",
                10,
            ),
            fixture_recording(
                "recording-drone-03-inspection",
                "project-inspection",
                "시설 점검",
                "drone-03",
                "drone-03-source",
                "서측 패널 점검",
                11,
            ),
        ] {
            catalog.recordings.insert(recording.id.clone(), recording);
        }

        catalog
    }

    pub(crate) fn bump_version(&mut self) -> u64 {
        self.snapshot_version = self.snapshot_version.saturating_add(1);
        self.snapshot_version
    }

    pub(crate) fn refresh_project_counts(&mut self, project_id: &str) {
        let assigned_device_ids = self
            .device_assignments
            .values()
            .filter(|assignment| assignment.project_id == project_id)
            .map(|assignment| assignment.device_id.as_str())
            .collect::<Vec<_>>();
        let online_device_count = assigned_device_ids
            .iter()
            .filter(|device_id| {
                self.devices
                    .get(**device_id)
                    .is_some_and(|device| device.status == "online")
            })
            .count();
        if let Some(project) = self.projects.get_mut(project_id) {
            project.device_count = assigned_device_ids.len();
            project.online_device_count = online_device_count;
        }
    }
}

pub(crate) fn new_id(prefix: &str) -> String {
    format!("{prefix}-{}", Uuid::new_v4())
}

pub(crate) fn now_iso() -> String {
    jiff::Timestamp::now().to_string()
}

pub(crate) fn now_ms() -> i64 {
    let millis = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_millis());
    i64::try_from(millis).unwrap_or(i64::MAX)
}

pub(crate) fn iso_from_millis(millis: i64) -> String {
    jiff::Timestamp::from_millisecond(millis)
        .map_or_else(|_| now_iso(), |timestamp| timestamp.to_string())
}

fn fixture_source(
    id: &str,
    integration_id: &str,
    device_id: &str,
    name: &str,
    topic_ids: &[&str],
) -> DataSource {
    DataSource {
        id: id.to_owned(),
        integration_id: integration_id.to_owned(),
        device_id: device_id.to_owned(),
        name: name.to_owned(),
        protocol: if integration_id.contains("mavlink") {
            "mavlink"
        } else {
            "ros2"
        }
        .to_owned(),
        status: "recording".to_owned(),
        live_url: SAMPLE_RRD_URL.to_owned(),
        topic_ids: topic_ids.iter().map(|topic| (*topic).to_owned()).collect(),
        mapping_version: 1,
        last_data_at: "2026-08-20T09:32:08+09:00".to_owned(),
    }
}

fn fixture_device_assignment(
    project_id: &str,
    device_id: &str,
    access_mode: AccessMode,
) -> DeviceAssignment {
    DeviceAssignment {
        id: format!("device-assignment-{project_id}-{device_id}"),
        project_id: project_id.to_owned(),
        device_id: device_id.to_owned(),
        access_mode,
        valid_from: "2026-08-20T00:00:00Z".to_owned(),
        valid_to: None,
        resource_version: 1,
    }
}

fn fixture_data_assignment(project_id: &str, data_source_id: &str) -> DataAssignment {
    DataAssignment {
        id: format!("data-assignment-{project_id}-{data_source_id}"),
        project_id: project_id.to_owned(),
        data_source_id: data_source_id.to_owned(),
        visibility: DataVisibility::Operator,
        valid_from: "2026-08-20T00:00:00Z".to_owned(),
        valid_to: None,
        resource_version: 1,
    }
}

fn fixture_recording(
    id: &str,
    project_id: &str,
    project_name: &str,
    device_id: &str,
    data_source_id: &str,
    name: &str,
    snapshot_version: u64,
) -> Recording {
    Recording {
        id: id.to_owned(),
        organization_id: "org-rms".to_owned(),
        project_id: project_id.to_owned(),
        device_id: device_id.to_owned(),
        data_source_id: data_source_id.to_owned(),
        name: name.to_owned(),
        status: "ready".to_owned(),
        rrd_url: SAMPLE_RRD_URL.to_owned(),
        captured_at: "2026-08-20T08:42:10+09:00".to_owned(),
        duration_label: "04:18".to_owned(),
        topic_ids: vec![
            "pose".to_owned(),
            "front-camera".to_owned(),
            "velocity".to_owned(),
            "battery".to_owned(),
        ],
        mapping_version: 1,
        project_snapshot: RecordingProjectSnapshot {
            project_id: project_id.to_owned(),
            project_name: project_name.to_owned(),
            captured_at: "2026-08-20T08:46:28+09:00".to_owned(),
            device_assignment_id: format!("device-assignment-{project_id}-{device_id}"),
            data_assignment_id: format!("data-assignment-{project_id}-{data_source_id}"),
        },
        resource_version: snapshot_version,
        manifest_hash: format!("sha256:{id}-immutable-fixture"),
    }
}

pub(crate) fn topics_for_source(
    data_source_id: &str,
    device_id: &str,
    topic_ids: &[String],
) -> Vec<Topic> {
    topic_ids
        .iter()
        .map(|topic_id| {
            let (path, label, renderer, value, unit, message) = match topic_id.as_str() {
                "pose" => (
                    "/localization/pose",
                    "위치와 주변",
                    "spatial",
                    Some("Bay 2 → Bay 3"),
                    None,
                    None,
                ),
                "front-camera" => (
                    "/camera/front/image",
                    "전방 카메라",
                    "camera",
                    Some("30 fps"),
                    None,
                    None,
                ),
                "velocity" => (
                    "/vehicle/velocity",
                    "속도",
                    "timeseries",
                    Some("1.2"),
                    Some("m/s"),
                    None,
                ),
                "altitude" => (
                    "/flight/altitude",
                    "고도",
                    "timeseries",
                    Some("0"),
                    Some("m"),
                    None,
                ),
                "battery" => (
                    "/power/battery",
                    "배터리",
                    "state",
                    Some("78"),
                    Some("%"),
                    None,
                ),
                _ => (
                    "/planning/status",
                    "경로 계획",
                    "log",
                    None,
                    None,
                    Some("전방 통로를 확인했습니다."),
                ),
            };
            Topic {
                id: topic_id.clone(),
                data_source_id: data_source_id.to_owned(),
                device_id: device_id.to_owned(),
                path: path.to_owned(),
                label: label.to_owned(),
                renderer: renderer.to_owned(),
                quality: "fresh".to_owned(),
                value: value.map(ToOwned::to_owned),
                unit: unit.map(ToOwned::to_owned),
                message: message.map(ToOwned::to_owned),
                samples: None,
                updated_at: "2026-08-20T09:32:08+09:00".to_owned(),
            }
        })
        .collect()
}
