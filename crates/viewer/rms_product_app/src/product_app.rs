use std::hash::{Hash as _, Hasher as _};
use std::rc::Rc;

use eframe::egui;
use re_sdk_types::blueprint::components::{LoopMode, PanelState, PlayState};
use re_viewer::{CommandSender, PanelStateOverrides, SystemCommand, SystemCommandSender as _};
use re_viewer_context::TimeControlCommand;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum ViewerPreset {
    Operations,
    Spatial,
    Camera,
    Diagnostics,
}

impl ViewerPreset {
    const ALL: [Self; 4] = [
        Self::Operations,
        Self::Spatial,
        Self::Camera,
        Self::Diagnostics,
    ];

    fn label(self) -> &'static str {
        match self {
            Self::Operations => "운영",
            Self::Spatial => "공간",
            Self::Camera => "카메라",
            Self::Diagnostics => "진단",
        }
    }
}

#[derive(Clone, Copy)]
struct TopicSummary {
    label: &'static str,
    value: &'static str,
}

const OPERATION_TOPICS: [TopicSummary; 5] = [
    TopicSummary {
        label: "위치",
        value: "정상",
    },
    TopicSummary {
        label: "전방 카메라",
        value: "30 FPS",
    },
    TopicSummary {
        label: "속도",
        value: "1.2 m/s",
    },
    TopicSummary {
        label: "배터리",
        value: "82%",
    },
    TopicSummary {
        label: "경로 계획",
        value: "활성",
    },
];

const CAMERA_TOPICS: [TopicSummary; 3] = [
    TopicSummary {
        label: "전방",
        value: "30 FPS",
    },
    TopicSummary {
        label: "후방",
        value: "30 FPS",
    },
    TopicSummary {
        label: "깊이",
        value: "15 FPS",
    },
];

const SPATIAL_TOPICS: [TopicSummary; 2] = [
    TopicSummary {
        label: "2D 지도",
        value: "대기 중",
    },
    TopicSummary {
        label: "3D 공간",
        value: "대기 중",
    },
];

const DIAGNOSTIC_TOPICS: [TopicSummary; 4] = [
    TopicSummary {
        label: "제어기",
        value: "정상",
    },
    TopicSummary {
        label: "네트워크",
        value: "24 ms",
    },
    TopicSummary {
        label: "GPU",
        value: "48%",
    },
    TopicSummary {
        label: "메모리",
        value: "3.1 GB",
    },
];

const DEFAULT_VISIBLE_TOPIC_COUNT: usize = 8;
const EMPTY_PRODUCT_QUERY: &str = "- /**";
const BLUEPRINT_RETRY_BASE_MS: f64 = 1_000.0;
const BLUEPRINT_RETRY_MAX_MS: f64 = 30_000.0;

#[derive(Clone, Debug, PartialEq, Eq)]
struct BlueprintPresetKey {
    store_id: re_log_types::StoreId,
    preset: ViewerPreset,
    topics_hash: u64,
}

impl BlueprintPresetKey {
    fn new(
        store_id: re_log_types::StoreId,
        preset: ViewerPreset,
        topics: &[RmsTopicContext],
    ) -> Self {
        Self {
            store_id,
            preset,
            topics_hash: product_topics_hash(preset, topics),
        }
    }
}

#[derive(Clone, Debug)]
struct BlueprintDispatchRetry {
    key: BlueprintPresetKey,
    attempts: u8,
    retry_after_ms: f64,
}

impl BlueprintDispatchRetry {
    fn blocks(&self, key: &BlueprintPresetKey, now_ms: f64) -> bool {
        self.key == *key && now_ms < self.retry_after_ms
    }
}

fn blueprint_retry_delay_ms(attempts: u8) -> f64 {
    let exponent = u32::from(attempts.saturating_sub(1).min(5));
    (BLUEPRINT_RETRY_BASE_MS * f64::from(1_u32 << exponent)).min(BLUEPRINT_RETRY_MAX_MS)
}

fn record_blueprint_dispatch_failure(
    dispatched_key: &mut Option<BlueprintPresetKey>,
    retry: &mut Option<BlueprintDispatchRetry>,
    key: BlueprintPresetKey,
    now_ms: f64,
) {
    *dispatched_key = None;
    let attempts = retry.as_ref().map_or(1, |previous| {
        if previous.key == key {
            previous.attempts.saturating_add(1)
        } else {
            1
        }
    });
    *retry = Some(BlueprintDispatchRetry {
        key,
        attempts,
        retry_after_ms: now_ms + blueprint_retry_delay_ms(attempts),
    });
}

fn log_blueprint_dispatch_error(err: &str) {
    #[cfg(not(target_arch = "wasm32"))]
    re_log::error!("Failed to dispatch RMS product blueprint: {err}");
    #[cfg(target_arch = "wasm32")]
    eprintln!("Failed to dispatch RMS product blueprint: {err}");
}

fn topic_matches_preset(preset: ViewerPreset, topic: &RmsTopicContext) -> bool {
    match preset {
        ViewerPreset::Operations => true,
        ViewerPreset::Spatial => matches!(
            topic.renderer.as_str(),
            "spatial" | "spatial2d" | "spatial3d" | "transform3d" | "map"
        ),
        ViewerPreset::Camera => topic.renderer == "camera",
        ViewerPreset::Diagnostics => {
            matches!(
                topic.renderer.as_str(),
                "state" | "log" | "raw" | "timeseries"
            )
        }
    }
}

fn canonical_product_topics(
    preset: ViewerPreset,
    topics: &[RmsTopicContext],
) -> Vec<&RmsTopicContext> {
    let mut topics = topics
        .iter()
        .filter(|topic| topic_matches_preset(preset, topic))
        .collect::<Vec<_>>();
    topics.sort_by(|left, right| {
        (&left.renderer, &left.path, &left.label).cmp(&(&right.renderer, &right.path, &right.label))
    });
    topics.dedup_by(|left, right| {
        left.renderer == right.renderer && left.path == right.path && left.label == right.label
    });
    topics
}

fn product_topics_hash(preset: ViewerPreset, topics: &[RmsTopicContext]) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    preset.hash(&mut hasher);
    for topic in canonical_product_topics(preset, topics) {
        topic.renderer.hash(&mut hasher);
        topic.path.hash(&mut hasher);
        topic.label.hash(&mut hasher);
    }
    hasher.finish()
}

#[derive(Debug, PartialEq, Eq)]
struct ProductViewSpec {
    name: String,
    class_identifier: &'static str,
    contents: Vec<String>,
    space_origin: String,
    transform_axes: Vec<String>,
}

fn topic_is_transform_tree(topic: &RmsTopicContext) -> bool {
    topic
        .path
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .is_some_and(|segment| {
            matches!(
                segment.to_ascii_lowercase().as_str(),
                "tf" | "tf_static" | "tf_drop"
            )
        })
}

fn product_view_specs(preset: ViewerPreset, topics: &[RmsTopicContext]) -> Vec<ProductViewSpec> {
    let topics = canonical_product_topics(preset, topics);
    let mut spatial_2d = Vec::new();
    let mut spatial_3d = Vec::new();
    let mut maps = Vec::new();
    let mut cameras = Vec::new();
    let mut timeseries = Vec::new();
    let mut states = Vec::new();
    let mut raw = Vec::new();
    let mut logs = Vec::new();
    for topic in topics {
        match topic.renderer.as_str() {
            "spatial2d" => spatial_2d.push(topic),
            "spatial" | "spatial3d" | "transform3d" => spatial_3d.push(topic),
            "map" => maps.push(topic),
            "camera" => cameras.push(topic),
            "timeseries" => timeseries.push(topic),
            "state" => states.push(topic),
            "log" => logs.push(topic),
            // Unknown or newly introduced renderers must remain inspectable instead of silently
            // becoming an empty TextLog view.
            _ => raw.push(topic),
        }
    }

    let paths = |topics: &[&RmsTopicContext]| {
        topics
            .iter()
            .map(|topic| topic.path.clone())
            .collect::<Vec<_>>()
    };
    let mut views = Vec::new();
    for spatial in spatial_2d {
        views.push(ProductViewSpec {
            name: format!("2D 공간 · {}", spatial.label),
            class_identifier: "2D",
            contents: vec![spatial.path.clone()],
            // Grid maps and other 2D spatial archetypes commonly log a CoordinateFrame on the
            // entity itself. Using that entity as the origin lets Rerun select the recorded frame
            // instead of the unrelated implicit root frame.
            space_origin: spatial.path.clone(),
            transform_axes: Vec::new(),
        });
    }
    let (transform_trees, spatial_geometry): (Vec<_>, Vec<_>) =
        spatial_3d.into_iter().partition(|topic| {
            topic.renderer == "transform3d"
                || (topic.renderer == "spatial" && topic_is_transform_tree(topic))
        });
    for spatial in spatial_geometry {
        views.push(ProductViewSpec {
            name: format!("3D 공간 · {}", spatial.label),
            class_identifier: "3D",
            contents: vec![spatial.path.clone()],
            // Explicit CoordinateFrame data is keyed by frame ID rather than by entity path.
            // Giving every independent geometry entity its own target frame guarantees that a
            // GridMap can render at the first sample, before a dynamic TF path to another frame
            // has arrived. It also prevents unrelated devices' frame graphs from poisoning one
            // shared spatial view.
            space_origin: spatial.path.clone(),
            transform_axes: Vec::new(),
        });
    }
    if !transform_trees.is_empty() {
        let transform_origin = transform_trees[0].path.clone();
        views.push(ProductViewSpec {
            name: "3D 좌표계".to_owned(),
            class_identifier: "3D",
            contents: paths(&transform_trees),
            space_origin: transform_origin,
            transform_axes: paths(&transform_trees),
        });
    }
    if !maps.is_empty() {
        views.push(ProductViewSpec {
            name: "지도".to_owned(),
            class_identifier: "Map",
            contents: paths(&maps),
            space_origin: "/".to_owned(),
            transform_axes: Vec::new(),
        });
    }
    for camera in cameras {
        views.push(ProductViewSpec {
            name: camera.label.clone(),
            class_identifier: "2D",
            contents: vec![camera.path.clone()],
            space_origin: "/".to_owned(),
            transform_axes: Vec::new(),
        });
    }
    if !timeseries.is_empty() {
        views.push(ProductViewSpec {
            name: "시계열".to_owned(),
            class_identifier: "TimeSeries",
            contents: paths(&timeseries),
            space_origin: "/".to_owned(),
            transform_axes: Vec::new(),
        });
    }
    if !states.is_empty() {
        views.push(ProductViewSpec {
            name: "상태".to_owned(),
            class_identifier: "StateTimeline",
            contents: paths(&states),
            space_origin: "/".to_owned(),
            transform_axes: Vec::new(),
        });
    }
    if !raw.is_empty() {
        views.push(ProductViewSpec {
            name: "원시 데이터".to_owned(),
            class_identifier: "Dataframe",
            contents: paths(&raw),
            space_origin: "/".to_owned(),
            transform_axes: Vec::new(),
        });
    }
    if !logs.is_empty() {
        views.push(ProductViewSpec {
            name: "로그".to_owned(),
            class_identifier: "TextLog",
            contents: paths(&logs),
            space_origin: "/".to_owned(),
            transform_axes: Vec::new(),
        });
    }

    if views.is_empty() {
        views.push(ProductViewSpec {
            name: "표시할 데이터 없음".to_owned(),
            class_identifier: "2D",
            contents: vec![EMPTY_PRODUCT_QUERY.to_owned()],
            space_origin: "/".to_owned(),
            transform_axes: Vec::new(),
        });
    }
    views
}

fn append_blueprint_archetype(
    messages: &mut Vec<re_log_types::LogMsg>,
    blueprint_id: &re_log_types::StoreId,
    entity_path: impl Into<re_log_types::EntityPath>,
    archetype: &dyn re_sdk_types::AsComponents,
) -> Result<(), String> {
    let timepoint = re_log_types::TimePoint::default().with(
        re_log_types::Timeline::new_sequence("blueprint"),
        re_log_types::TimeInt::new_temporal(0),
    );
    let chunk = re_chunk::Chunk::builder(entity_path)
        .with_archetype(re_chunk::RowId::new(), timepoint, archetype)
        .build()
        .map_err(|err| format!("Blueprint chunk creation failed: {err}"))?;
    let arrow_msg = chunk
        .to_arrow_msg()
        .map_err(|err| format!("Blueprint chunk serialization failed: {err}"))?;
    messages.push(re_log_types::LogMsg::ArrowMsg(
        blueprint_id.clone(),
        arrow_msg,
    ));
    Ok(())
}

fn product_blueprint_messages(
    store_id: &re_log_types::StoreId,
    preset: ViewerPreset,
    topics: &[RmsTopicContext],
) -> Result<Vec<re_log_types::LogMsg>, String> {
    use re_sdk_types::archetypes::TransformAxes3D;
    use re_sdk_types::blueprint::archetypes::{
        ActiveVisualizers, ContainerBlueprint, ViewBlueprint, ViewContents, ViewportBlueprint,
        VisualizerInstruction,
    };
    use re_sdk_types::blueprint::components::{
        AutoLayout, AutoViews, ContainerKind, GridColumns, RootContainer, ViewClass,
        VisualizerInstructionId,
    };
    use re_sdk_types::components::Name;
    use re_sdk_types::datatypes::{Bool, Uuid};

    let blueprint_id = re_log_types::StoreId::random(
        re_log_types::StoreKind::Blueprint,
        store_id.application_id().clone(),
    );
    let mut messages = vec![re_log_types::LogMsg::SetStoreInfo(
        re_log_types::SetStoreInfo {
            row_id: *re_chunk::RowId::new(),
            info: re_log_types::StoreInfo::new(
                blueprint_id.clone(),
                re_log_types::StoreSource::Viewer,
            ),
        },
    )];

    let views = product_view_specs(preset, topics);
    let mut view_paths = Vec::with_capacity(views.len());
    for view in views {
        let view_id = Uuid::random();
        let view_path = format!("/view/{view_id}");
        let contents = ViewContents::new(view.contents);
        append_blueprint_archetype(
            &mut messages,
            &blueprint_id,
            format!("{view_path}/ViewContents"),
            &contents,
        )?;
        let view_blueprint = ViewBlueprint::new(ViewClass(view.class_identifier.into()))
            .with_display_name(Name(view.name.into()))
            .with_space_origin(view.space_origin);
        append_blueprint_archetype(
            &mut messages,
            &blueprint_id,
            view_path.clone(),
            &view_blueprint,
        )?;
        for entity_path in view.transform_axes {
            let entity_path = re_log_types::EntityPath::parse_strict(&entity_path)
                .map_err(|err| format!("Invalid transform topic entity path: {err}"))?;
            let override_base = ViewContents::blueprint_base_visualizer_path_for_entity(
                view_id.into(),
                &entity_path,
            );
            let visualizer_id = VisualizerInstructionId::new_deterministic(&entity_path, 0);
            let visualizer_path = override_base.join(
                &re_log_types::EntityPath::from_single_string(visualizer_id.to_string()),
            );
            append_blueprint_archetype(
                &mut messages,
                &blueprint_id,
                visualizer_path.clone(),
                &VisualizerInstruction::new("TransformAxes3D"),
            )?;
            append_blueprint_archetype(
                &mut messages,
                &blueprint_id,
                visualizer_path,
                &TransformAxes3D::new(0.25).with_show_frame(true),
            )?;
            append_blueprint_archetype(
                &mut messages,
                &blueprint_id,
                override_base,
                &ActiveVisualizers::new([visualizer_id]),
            )?;
        }
        view_paths.push(view_path);
    }

    let root_id = Uuid::random();
    let root_path = format!("/container/{root_id}");
    let grid_columns = if view_paths.len() > 1 { 2 } else { 1 };
    let root = ContainerBlueprint::new(ContainerKind::Grid)
        .with_contents(view_paths)
        .with_grid_columns(GridColumns(grid_columns.into()));
    append_blueprint_archetype(&mut messages, &blueprint_id, root_path, &root)?;

    let viewport = ViewportBlueprint::new()
        .with_root_container(RootContainer(root_id))
        .with_auto_layout(AutoLayout(Bool(false)))
        .with_auto_views(AutoViews(Bool(false)));
    append_blueprint_archetype(&mut messages, &blueprint_id, "/viewport", &viewport)?;
    messages.push(re_log_types::LogMsg::BlueprintActivationCommand(
        re_log_types::BlueprintActivationCommand {
            blueprint_id,
            make_active: true,
            make_default: false,
        },
    ));

    let activations = messages
        .iter()
        .filter_map(|message| match message {
            re_log_types::LogMsg::BlueprintActivationCommand(command) => Some(command),
            _ => None,
        })
        .collect::<Vec<_>>();
    if messages.is_empty()
        || activations.len() != 1
        || !activations[0].make_active
        || activations[0].make_default
    {
        return Err("Blueprint stream is missing its non-default activation command".to_owned());
    }
    Ok(messages)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ControlCommand {
    PauseMission,
    SafeStop,
}

impl ControlCommand {
    fn label(self) -> &'static str {
        match self {
            Self::PauseMission => "임무 일시정지",
            Self::SafeStop => "안전 정지",
        }
    }

    fn api_value(self) -> &'static str {
        match self {
            Self::PauseMission => "pause_mission",
            Self::SafeStop => "safe_stop",
        }
    }
}

/// Legacy backend source kind accepted by the compatibility `WebAssembly` method.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RmsSourceKind {
    Live,
    Recording,
}

/// A topic summary supplied by the RMS backend adapter.
#[derive(Clone, Debug, Deserialize)]
pub struct RmsTopicContext {
    pub label: String,
    pub path: String,
    pub renderer: String,
    pub value: Option<String>,
}

/// Live-service context.
///
/// Only this context can be paired with a [`RmsControlEventSink`].
#[derive(Clone, Debug, Deserialize)]
pub struct LiveViewerContext {
    pub project_id: String,
    pub project_name: String,
    pub device_id: String,
    pub device_name: String,
    pub device_status: String,
    pub device_health: String,
    pub device_state_version: u32,
    pub live_session_id: String,
    pub data_source_id: String,
    pub source_name: String,
    pub source_url: String,
    pub operator_id: String,
    #[serde(default)]
    pub control_enabled: bool,
    pub topics: Vec<RmsTopicContext>,
}

/// Semantic kind of a cursor value supplied by the Replay service.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ReplayTimeKind {
    Sequence,
    Timestamp,
    Duration,
}

/// Lossless integer cursor supplied as a decimal string.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct ReplayTimeValue {
    pub kind: ReplayTimeKind,
    pub value: String,
}

/// Initial Replay transport state.
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ReplayInitialPlayState {
    #[default]
    Paused,
    Playing,
}

/// Initial Replay loop mode.
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ReplayInitialLoopMode {
    #[default]
    Off,
    All,
    Selection,
}

/// Initial Replay loop policy.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
pub struct ReplayInitialLoop {
    #[serde(default)]
    pub mode: ReplayInitialLoopMode,
    #[serde(default)]
    pub start: Option<ReplayTimeValue>,
    #[serde(default)]
    pub end: Option<ReplayTimeValue>,
}

fn default_replay_speed() -> f32 {
    1.0
}

/// Replay-service context.
///
/// It intentionally contains neither operator identity nor a device-state version, because Replay
/// cannot acquire a control capability.
#[derive(Clone, Debug, Deserialize)]
pub struct ReplayViewerContext {
    pub project_id: String,
    pub project_name: String,
    pub device_id: String,
    pub device_name: String,
    pub recording_id: String,
    pub recording_name: String,
    pub replay_session_id: String,
    pub source_url: String,
    #[serde(default)]
    pub captured_at_label: Option<String>,
    #[serde(default)]
    pub initial_timeline: String,
    #[serde(default)]
    pub initial_fps: Option<f32>,
    #[serde(default)]
    pub initial_cursor: Option<ReplayTimeValue>,
    #[serde(default)]
    pub initial_play_state: ReplayInitialPlayState,
    #[serde(default = "default_replay_speed")]
    pub initial_speed: f32,
    #[serde(default)]
    pub initial_loop: ReplayInitialLoop,
    pub topics: Vec<RmsTopicContext>,
}

/// Tagged viewer-service context used by native adapters and the compatibility web bridge.
#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "service", rename_all = "snake_case")]
pub enum RmsViewerContext {
    Live(LiveViewerContext),
    Replay(ReplayViewerContext),
}

/// Navigation intent emitted to the RMS host.
#[derive(Clone, Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RmsHostEvent {
    SelectDevice {
        device_id: String,
    },
    OpenReplay {
        project_id: String,
        device_id: String,
        live_session_id: String,
    },
    OpenLive {
        project_id: String,
        device_id: String,
    },
}

/// Physical-control intent emitted only by a live viewer session.
#[derive(Clone, Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RmsControlEvent {
    RequestControlLease {
        request_id: String,
        device_id: String,
        expected_device_version: u32,
    },
    ReleaseControlLease {
        request_id: String,
        device_id: String,
        lease_id: String,
        lease_epoch: u32,
    },
    SendControlCommand {
        request_id: String,
        device_id: String,
        command_type: String,
        expected_device_version: u32,
        lease_id: String,
        lease_epoch: u32,
        session_mode: String,
    },
}

/// Result returned by the RMS transport adapter after a live control request.
#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RmsControlResponse {
    LeaseGranted {
        request_id: String,
        device_id: String,
        lease_id: String,
        lease_epoch: u32,
        holder_id: String,
        holder_name: String,
        expires_at_ms: f64,
    },
    LeaseReleased {
        request_id: String,
        device_id: String,
        lease_id: String,
        lease_epoch: u32,
    },
    CommandUpdated {
        request_id: String,
        device_id: String,
        message: String,
    },
    Failed {
        request_id: String,
        device_id: String,
        message: String,
    },
}

/// Callback boundary for project, device, Live, and Replay navigation.
pub type RmsHostEventSink = Rc<dyn Fn(RmsHostEvent)>;

/// Capability injected into a live session for physical-control intents.
pub type RmsControlEventSink = Rc<dyn Fn(RmsControlEvent)>;

#[derive(Clone, Debug)]
struct ControlLease {
    device_id: String,
    id: String,
    epoch: u32,
    holder_id: String,
    expires_at_ms: f64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PendingControlKind {
    RequestLease,
    ReleaseLease,
    Command,
}

#[derive(Clone, Debug)]
struct PendingControlRequest {
    id: String,
    device_id: String,
    kind: PendingControlKind,
    lease_id: Option<String>,
    lease_epoch: Option<u32>,
}

struct LiveControlCapability {
    event_sink: RmsControlEventSink,
    lease: Option<ControlLease>,
    pending: Option<PendingControlRequest>,
    pending_confirmation: Option<ControlCommand>,
}

impl LiveControlCapability {
    /// Records correlation state before calling out to the host.
    fn dispatch(&mut self, pending: PendingControlRequest, event: RmsControlEvent) {
        self.pending = Some(pending);
        (self.event_sink)(event);
    }

    fn release_lease(&mut self, request_id: String) -> bool {
        let Some(lease) = self.lease.take() else {
            return false;
        };
        let pending = PendingControlRequest {
            id: request_id.clone(),
            device_id: lease.device_id.clone(),
            kind: PendingControlKind::ReleaseLease,
            lease_id: Some(lease.id.clone()),
            lease_epoch: Some(lease.epoch),
        };
        let event = RmsControlEvent::ReleaseControlLease {
            request_id,
            device_id: lease.device_id,
            lease_id: lease.id,
            lease_epoch: lease.epoch,
        };
        self.dispatch(pending, event);
        true
    }
}

struct LiveSession {
    context: LiveViewerContext,
    control: Option<LiveControlCapability>,
}

enum ActiveViewerContext {
    Live(Box<LiveSession>),
    Replay(Box<ReplayViewerContext>),
}

impl ActiveViewerContext {
    fn project_id(&self) -> &str {
        match self {
            Self::Live(session) => &session.context.project_id,
            Self::Replay(context) => &context.project_id,
        }
    }

    fn project_name(&self) -> &str {
        match self {
            Self::Live(session) => &session.context.project_name,
            Self::Replay(context) => &context.project_name,
        }
    }

    fn device_id(&self) -> &str {
        match self {
            Self::Live(session) => &session.context.device_id,
            Self::Replay(context) => &context.device_id,
        }
    }

    fn device_name(&self) -> &str {
        match self {
            Self::Live(session) => &session.context.device_name,
            Self::Replay(context) => &context.device_name,
        }
    }

    fn source_id(&self) -> &str {
        match self {
            Self::Live(session) => &session.context.live_session_id,
            Self::Replay(context) => &context.replay_session_id,
        }
    }

    fn source_url(&self) -> &str {
        match self {
            Self::Live(session) => &session.context.source_url,
            Self::Replay(context) => &context.source_url,
        }
    }

    fn topics(&self) -> &[RmsTopicContext] {
        match self {
            Self::Live(session) => &session.context.topics,
            Self::Replay(context) => &context.topics,
        }
    }

    fn badge(&self) -> &'static str {
        match self {
            Self::Live(_) => "LIVE",
            Self::Replay(_) => "REPLAY",
        }
    }
}

enum RetiredControlRequest {
    LeaseRequest {
        request_id: String,
        device_id: String,
        event_sink: RmsControlEventSink,
    },
    LeaseRelease {
        request_id: String,
        device_id: String,
        lease_id: String,
        lease_epoch: u32,
    },
}

#[derive(Default)]
struct RetiredControlRequests(Vec<RetiredControlRequest>);

impl RetiredControlRequests {
    fn push_lease_request(
        &mut self,
        request_id: String,
        device_id: String,
        event_sink: RmsControlEventSink,
    ) {
        self.0.push(RetiredControlRequest::LeaseRequest {
            request_id,
            device_id,
            event_sink,
        });
    }

    fn push_lease_release(&mut self, pending: PendingControlRequest) {
        let (Some(lease_id), Some(lease_epoch)) = (pending.lease_id, pending.lease_epoch) else {
            return;
        };
        self.0.push(RetiredControlRequest::LeaseRelease {
            request_id: pending.id,
            device_id: pending.device_id,
            lease_id,
            lease_epoch,
        });
    }

    fn take_lease_request_sink(
        &mut self,
        request_id: &str,
        device_id: &str,
    ) -> Option<RmsControlEventSink> {
        let index = self.0.iter().position(|request| {
            matches!(
                request,
                RetiredControlRequest::LeaseRequest {
                    request_id: pending_request_id,
                    device_id: pending_device_id,
                    ..
                } if pending_request_id == request_id && pending_device_id == device_id
            )
        })?;
        let RetiredControlRequest::LeaseRequest { event_sink, .. } = self.0.remove(index) else {
            unreachable!("the matching retired request is a lease request");
        };
        Some(event_sink)
    }

    fn remove_lease_release(
        &mut self,
        request_id: &str,
        device_id: &str,
        lease_id: &str,
        lease_epoch: u32,
    ) -> bool {
        let Some(index) = self.0.iter().position(|request| {
            matches!(
                request,
                RetiredControlRequest::LeaseRelease {
                    request_id: pending_request_id,
                    device_id: pending_device_id,
                    lease_id: pending_lease_id,
                    lease_epoch: pending_lease_epoch,
                } if pending_request_id == request_id
                    && pending_device_id == device_id
                    && pending_lease_id == lease_id
                    && *pending_lease_epoch == lease_epoch
            )
        }) else {
            return false;
        };
        self.0.remove(index);
        true
    }

    fn remove_terminal_failure(&mut self, request_id: &str, device_id: &str) -> bool {
        let Some(index) = self.0.iter().position(|request| match request {
            RetiredControlRequest::LeaseRequest {
                request_id: pending_request_id,
                device_id: pending_device_id,
                ..
            }
            | RetiredControlRequest::LeaseRelease {
                request_id: pending_request_id,
                device_id: pending_device_id,
                ..
            } => pending_request_id == request_id && pending_device_id == device_id,
        }) else {
            return false;
        };
        self.0.remove(index);
        true
    }

    fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum UiAction {
    OpenReplay,
    OpenLive,
    ToggleReplayTimeline,
    ToggleLease,
    RequestCommand(ControlCommand),
    ConfirmCommand(ControlCommand),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ReplayInitializationStage {
    ApplyPlaybackPolicy,
    PlaybackPolicyApplied,
}

/// Product-owned RMS application that renders the operational shell and Rerun viewport together.
pub struct RmsProductApp {
    rerun_app: re_viewer::App,
    egui_ctx: egui::Context,
    command_sender: CommandSender,
    preset: ViewerPreset,
    show_all_topics: bool,
    dispatched_blueprint_key: Option<BlueprintPresetKey>,
    blueprint_dispatch_retry: Option<BlueprintDispatchRetry>,
    viewer_context: Option<ActiveViewerContext>,
    retired_control_requests: RetiredControlRequests,
    next_request_number: u64,
    pending_play_state: Option<PlayState>,
    pending_replay_initialization: Option<ReplayInitializationStage>,
    waiting_for_source_change_from: Option<String>,
    host_event_sink: Option<RmsHostEventSink>,
    status_message: Option<String>,
    shutdown_requested: bool,
}

impl RmsProductApp {
    /// Creates a product app and opens the initial replay source, when supplied.
    pub fn new(
        main_thread_token: re_viewer::MainThreadToken,
        creation_context: &eframe::CreationContext<'_>,
        source_url: Option<&str>,
        host_event_sink: Option<RmsHostEventSink>,
    ) -> Result<Self, re_renderer::RenderContextError> {
        re_viewer::customize_eframe_and_setup_renderer(creation_context)?;
        install_product_fonts(&creation_context.egui_ctx);

        creation_context
            .egui_ctx
            .options_mut(|options| options.theme_preference = egui::ThemePreference::Dark);

        let startup_options = re_viewer::StartupOptions {
            persist_state: false,
            hide_welcome_screen: true,
            panel_state_overrides: PanelStateOverrides {
                top: Some(PanelState::Hidden),
                blueprint: Some(PanelState::Hidden),
                selection: Some(PanelState::Hidden),
                time: Some(PanelState::Hidden),
            },
            ..Default::default()
        };

        let command_channel = re_viewer::command_channel();
        let command_sender = command_channel.0.clone();
        let rerun_app = re_viewer::App::with_commands(
            main_thread_token,
            re_viewer::build_info(),
            re_viewer::AppEnvironment::Custom("RMS Product Runtime".to_owned()),
            startup_options,
            creation_context,
            None,
            re_async::AsyncRuntimeHandle::from_current_tokio_runtime_or_wasmbindgen()
                .expect("an async runtime is always available on web and in the native launcher"),
            re_viewer::register_text_log_receiver(),
            command_channel,
        );

        if let Some(source_url) = source_url {
            rerun_app.open_url_or_file(source_url);
        }

        Ok(Self {
            rerun_app,
            egui_ctx: creation_context.egui_ctx.clone(),
            command_sender,
            preset: ViewerPreset::Operations,
            show_all_topics: false,
            dispatched_blueprint_key: None,
            blueprint_dispatch_retry: None,
            viewer_context: None,
            retired_control_requests: RetiredControlRequests::default(),
            next_request_number: 0,
            pending_play_state: source_url.map(|_| PlayState::Paused),
            pending_replay_initialization: None,
            waiting_for_source_change_from: None,
            host_event_sink,
            status_message: None,
            shutdown_requested: false,
        })
    }

    fn topics(&self) -> &'static [TopicSummary] {
        match self.preset {
            ViewerPreset::Operations => &OPERATION_TOPICS,
            ViewerPreset::Spatial => &SPATIAL_TOPICS,
            ViewerPreset::Camera => &CAMERA_TOPICS,
            ViewerPreset::Diagnostics => &DIAGNOSTIC_TOPICS,
        }
    }

    fn topic_matches_preset(&self, topic: &RmsTopicContext) -> bool {
        topic_matches_preset(self.preset, topic)
    }

    fn matched_viewer_context(&self) -> Option<&ActiveViewerContext> {
        self.viewer_context.as_ref()
    }

    fn matched_live_session(&self) -> Option<&LiveSession> {
        match self.matched_viewer_context()? {
            ActiveViewerContext::Live(session) => Some(session),
            ActiveViewerContext::Replay(_) => None,
        }
    }

    fn matched_live_session_mut(&mut self) -> Option<&mut LiveSession> {
        match self.viewer_context.as_mut()? {
            ActiveViewerContext::Live(session) => Some(session),
            ActiveViewerContext::Replay(_) => None,
        }
    }

    fn viewer_is_live_and_ready(&self) -> bool {
        self.matched_live_session().is_some_and(|session| {
            source_transition_is_ready(&SourceTransitionState {
                waiting_for_source_change: self.waiting_for_source_change_from.is_some(),
                pending_play_state: self.pending_play_state.is_some(),
                has_active_recording: self.rerun_app.active_recording_id().is_some(),
                play_state: self.rerun_app.active_play_state(),
                active_source_matches: self
                    .rerun_app
                    .active_recording_loaded_from_url(&session.context.source_url),
            }) && !session.context.live_session_id.is_empty()
                && !session.context.data_source_id.is_empty()
                && !session.context.source_url.is_empty()
        })
    }

    fn control_enabled(&self) -> bool {
        if self.shutdown_requested {
            return false;
        }
        let Some(session) = self.matched_live_session() else {
            return false;
        };
        let Some(control) = session.control.as_ref() else {
            return false;
        };
        let Some(lease) = control.lease.as_ref() else {
            return false;
        };

        control_is_allowed(
            self.viewer_is_live_and_ready() && control.pending.is_none(),
            &session.context,
            lease,
            current_time_ms(),
        )
    }

    fn lease_request_allowed(&self) -> bool {
        if self.shutdown_requested {
            return false;
        }
        let Some(session) = self.matched_live_session() else {
            return false;
        };
        let Some(control) = session.control.as_ref() else {
            return false;
        };

        self.viewer_is_live_and_ready()
            && control.pending.is_none()
            && control.lease.is_none()
            && session.context.device_status == "online"
            && !matches!(
                session.context.device_health.as_str(),
                "restricted" | "critical"
            )
            && session.context.device_state_version > 0
            && !session.context.operator_id.is_empty()
    }

    fn next_request_id(&mut self) -> String {
        self.next_request_number = self.next_request_number.wrapping_add(1);
        format!("rms-runtime-{}", self.next_request_number)
    }

    fn send_time_command(&self, command: TimeControlCommand) {
        self.send_time_commands(vec![command]);
    }

    fn send_time_commands(&self, commands: Vec<TimeControlCommand>) -> bool {
        let Some(store_id) = self.rerun_app.active_recording_id().cloned() else {
            return false;
        };

        self.command_sender
            .send_system(SystemCommand::TimeControlCommands {
                store_id,
                time_commands: commands,
            });
        true
    }

    fn dispatch_product_blueprint(
        &mut self,
        store_id: &re_log_types::StoreId,
        preset: ViewerPreset,
        topics: &[RmsTopicContext],
    ) -> Result<(), String> {
        let messages = product_blueprint_messages(store_id, preset, topics)?;
        let (sender, receiver) = re_log_channel::log_channel(re_log_channel::LogSource::Sdk);
        for message in messages {
            sender
                .send(re_log_channel::DataSourceMessage::LogMsg(message))
                .map_err(|_err| "Blueprint log channel closed before dispatch".to_owned())?;
        }
        drop(sender);
        self.rerun_app.add_log_receiver(receiver);
        self.egui_ctx.request_repaint();
        Ok(())
    }

    fn synchronize_product_blueprint(&mut self) {
        let Some(store_id) = self.rerun_app.active_recording_id().cloned() else {
            return;
        };
        let Some(context) = self.matched_viewer_context() else {
            return;
        };
        if !self
            .rerun_app
            .active_recording_loaded_from_url(context.source_url())
        {
            return;
        }
        let topics = context.topics().to_vec();
        let key = BlueprintPresetKey::new(store_id.clone(), self.preset, &topics);
        if self.dispatched_blueprint_key.as_ref() == Some(&key) {
            return;
        }
        let now_ms = current_time_ms();
        if self
            .blueprint_dispatch_retry
            .as_ref()
            .is_some_and(|retry| retry.blocks(&key, now_ms))
        {
            return;
        }

        // Claim the key before any channel callback can run, so one UI frame can never enqueue
        // the same product blueprint twice.
        self.dispatched_blueprint_key = Some(key.clone());
        if let Err(err) = self.dispatch_product_blueprint(&store_id, self.preset, &topics) {
            log_blueprint_dispatch_error(&err);
            record_blueprint_dispatch_failure(
                &mut self.dispatched_blueprint_key,
                &mut self.blueprint_dispatch_retry,
                key,
                now_ms,
            );
            self.status_message = Some("뷰 구성을 잠시 후 다시 적용합니다.".to_owned());
        } else {
            self.blueprint_dispatch_retry = None;
        }
    }

    fn open_source_if_changed(&mut self, source_url: &str, source_changed: bool) {
        if source_changed {
            self.waiting_for_source_change_from = self
                .rerun_app
                .active_recording_id()
                .map(ToString::to_string);
            self.rerun_app.open_url_or_file(source_url);
        }
    }

    fn emit_retired_release(&mut self, event_sink: &RmsControlEventSink, lease: ControlLease) {
        let request_id = self.next_request_id();
        let pending = PendingControlRequest {
            id: request_id.clone(),
            device_id: lease.device_id.clone(),
            kind: PendingControlKind::ReleaseLease,
            lease_id: Some(lease.id.clone()),
            lease_epoch: Some(lease.epoch),
        };
        self.retired_control_requests.push_lease_release(pending);
        event_sink(RmsControlEvent::ReleaseControlLease {
            request_id,
            device_id: lease.device_id,
            lease_id: lease.id,
            lease_epoch: lease.epoch,
        });
    }

    fn retire_active_live_session(&mut self) {
        let Some(ActiveViewerContext::Live(mut session)) = self.viewer_context.take() else {
            return;
        };
        let Some(mut control) = session.control.take() else {
            return;
        };

        if let Some(lease) = control.lease.take() {
            self.emit_retired_release(&control.event_sink, lease);
        }
        if let Some(pending) = control.pending.take() {
            match pending.kind {
                PendingControlKind::RequestLease => {
                    self.retired_control_requests.push_lease_request(
                        pending.id,
                        pending.device_id,
                        control.event_sink,
                    );
                }
                PendingControlKind::ReleaseLease => {
                    self.retired_control_requests.push_lease_release(pending);
                }
                PendingControlKind::Command => {}
            }
        }
    }

    fn clear_active_context(&mut self) {
        if matches!(self.viewer_context, Some(ActiveViewerContext::Live(_))) {
            self.retire_active_live_session();
        } else {
            self.viewer_context = None;
        }
    }

    /// Applies an authenticated Live service context and its optional control capability.
    pub fn apply_live_context(
        &mut self,
        context: LiveViewerContext,
        control_event_sink: Option<RmsControlEventSink>,
    ) {
        self.shutdown_requested = false;
        self.pending_replay_initialization = None;
        self.rerun_app
            .set_time_panel_override(Some(time_panel_state_for_live(true)));
        let control_event_sink = if context.control_enabled {
            control_event_sink
        } else {
            None
        };
        let same_session = self.viewer_context.as_ref().is_some_and(|current| {
            matches!(current, ActiveViewerContext::Live(session)
                if session.context.live_session_id == context.live_session_id
                    && session.context.data_source_id == context.data_source_id
                    && session.context.device_id == context.device_id
                    && session.context.operator_id == context.operator_id)
        });
        let source_changed = self
            .viewer_context
            .as_ref()
            .is_none_or(|current| current.source_id() != context.live_session_id);
        let source_url_changed = self
            .viewer_context
            .as_ref()
            .is_none_or(|current| current.source_url() != context.source_url);
        let capability_revoked = control_capability_was_revoked(&ControlCapabilityUpdate {
            same_session,
            had_control_capability: self
                .matched_live_session()
                .is_some_and(|session| session.control.is_some()),
            requested_control_capability: control_event_sink.is_some(),
        });

        if same_session && !capability_revoked {
            if let Some(ActiveViewerContext::Live(session)) = self.viewer_context.as_mut() {
                session.context = context;
                if session.control.is_none()
                    && let Some(event_sink) = control_event_sink
                {
                    session.control = Some(LiveControlCapability {
                        event_sink,
                        lease: None,
                        pending: None,
                        pending_confirmation: None,
                    });
                }
            }
        } else {
            self.clear_active_context();
            self.viewer_context = Some(ActiveViewerContext::Live(Box::new(LiveSession {
                context,
                control: control_event_sink.map(|event_sink| LiveControlCapability {
                    event_sink,
                    lease: None,
                    pending: None,
                    pending_confirmation: None,
                }),
            })));
        }

        let source_url = self
            .viewer_context
            .as_ref()
            .map_or("", ActiveViewerContext::source_url)
            .to_owned();
        self.open_source_if_changed(&source_url, source_changed || source_url_changed);
        if !same_session || source_changed || source_url_changed {
            self.pending_play_state = Some(PlayState::Following);
            self.status_message = None;
        }
        self.egui_ctx.request_repaint();
    }

    /// Applies an authenticated Replay service context.
    ///
    /// A Replay session has no control sink, lease, pending command, or confirmation UI.
    pub fn apply_replay_context(&mut self, context: ReplayViewerContext) {
        self.shutdown_requested = false;
        let same_session = self.viewer_context.as_ref().is_some_and(|current| {
            matches!(current, ActiveViewerContext::Replay(current)
                if current.replay_session_id == context.replay_session_id
                    && current.recording_id == context.recording_id
                    && current.device_id == context.device_id)
        });
        let source_changed = self
            .viewer_context
            .as_ref()
            .is_none_or(|current| current.source_id() != context.replay_session_id);
        let source_url_changed = self
            .viewer_context
            .as_ref()
            .is_none_or(|current| current.source_url() != context.source_url);
        let source_url = context.source_url.clone();
        let should_initialize = !same_session || source_changed || source_url_changed;

        self.clear_active_context();
        self.viewer_context = Some(ActiveViewerContext::Replay(Box::new(context)));
        self.open_source_if_changed(&source_url, source_changed || source_url_changed);
        self.pending_play_state = None;
        if should_initialize {
            self.rerun_app
                .set_time_panel_override(Some(time_panel_state_for_live(false)));
            self.pending_replay_initialization =
                Some(ReplayInitializationStage::ApplyPlaybackPolicy);
        }
        self.status_message = None;
        self.egui_ctx.request_repaint();
    }

    /// Applies either tagged service context.
    pub fn apply_viewer_context(
        &mut self,
        context: RmsViewerContext,
        control_event_sink: Option<RmsControlEventSink>,
    ) {
        match context {
            RmsViewerContext::Live(context) => {
                self.apply_live_context(context, control_event_sink);
            }
            RmsViewerContext::Replay(context) => self.apply_replay_context(context),
        }
    }

    /// Removes an active Live control capability while keeping the observation context open.
    pub fn revoke_control_capability(&mut self) {
        let Some(session) = self.matched_live_session() else {
            return;
        };
        if session.control.is_none() {
            return;
        }
        let mut context = session.context.clone();
        context.control_enabled = false;
        self.apply_live_context(context, None);
    }

    /// Fails closed when the RMS backend cannot resolve the selected viewer session.
    pub fn apply_viewer_error(&mut self, message: String) {
        self.clear_active_context();
        self.pending_play_state = None;
        self.pending_replay_initialization = None;
        self.waiting_for_source_change_from = None;
        self.rerun_app
            .set_time_panel_override(Some(PanelState::Hidden));
        self.status_message = Some(message);
        self.egui_ctx.request_repaint();
    }

    /// Starts a fail-closed shutdown and reports whether all control cleanup has completed.
    ///
    /// The host should keep delivering [`RmsControlResponse`] values and call this again until it
    /// returns `true` before destroying the runtime.
    pub fn prepare_for_shutdown(&mut self) -> bool {
        self.shutdown_requested = true;
        let has_active_lease = self
            .matched_live_session()
            .and_then(|session| session.control.as_ref())
            .is_some_and(|control| control.lease.is_some());
        if has_active_lease {
            self.release_active_lease("제어권을 반납한 뒤 종료합니다.");
        }

        let has_active_pending = self
            .matched_live_session()
            .and_then(|session| session.control.as_ref())
            .is_some_and(|control| control.pending.is_some());
        let ready = !has_active_pending && self.retired_control_requests.is_empty();
        self.egui_ctx.request_repaint();
        ready
    }

    fn request_open_replay(&mut self) {
        let Some(ActiveViewerContext::Live(session)) = self.matched_viewer_context() else {
            return;
        };
        let event = RmsHostEvent::OpenReplay {
            project_id: session.context.project_id.clone(),
            device_id: session.context.device_id.clone(),
            live_session_id: session.context.live_session_id.clone(),
        };
        if let Some(event_sink) = self.host_event_sink.clone() {
            event_sink(event);
            self.status_message = Some("Replay를 여는 중입니다.".to_owned());
        } else {
            self.status_message = Some("Replay 서비스가 연결되지 않았습니다.".to_owned());
        }
    }

    fn request_open_live(&mut self) {
        let Some(context) = self.matched_viewer_context() else {
            return;
        };
        let event = RmsHostEvent::OpenLive {
            project_id: context.project_id().to_owned(),
            device_id: context.device_id().to_owned(),
        };
        if let Some(event_sink) = self.host_event_sink.clone() {
            event_sink(event);
            self.status_message = Some("LIVE를 여는 중입니다.".to_owned());
        } else {
            self.status_message = Some("LIVE 서비스가 연결되지 않았습니다.".to_owned());
        }
    }

    fn release_active_lease(&mut self, message: &str) {
        let request_id = self.next_request_id();
        let released = self
            .matched_live_session_mut()
            .and_then(|session| session.control.as_mut())
            .is_some_and(|control| control.release_lease(request_id));
        if released {
            self.status_message = Some(message.to_owned());
        }
    }

    fn apply_action(&mut self, action: UiAction) {
        match action {
            UiAction::OpenReplay => self.request_open_replay(),
            UiAction::OpenLive => self.request_open_live(),
            UiAction::ToggleReplayTimeline => {
                if matches!(
                    self.matched_viewer_context(),
                    Some(ActiveViewerContext::Replay(_))
                ) {
                    let state =
                        toggled_replay_time_panel_state(self.rerun_app.time_panel_override());
                    self.rerun_app.set_time_panel_override(Some(state));
                }
            }
            UiAction::ToggleLease => {
                let has_lease = self
                    .matched_live_session()
                    .and_then(|session| session.control.as_ref())
                    .is_some_and(|control| control.lease.is_some());
                if has_lease {
                    self.release_active_lease("제어권을 반납하고 있습니다.");
                    return;
                }
                if !self.lease_request_allowed() {
                    self.status_message =
                        Some("LIVE 데이터와 장비 상태를 확인해 주세요.".to_owned());
                    return;
                }

                let Some(session) = self.matched_live_session() else {
                    return;
                };
                if session.control.is_none() {
                    return;
                }
                let device_id = session.context.device_id.clone();
                let expected_device_version = session.context.device_state_version;
                let request_id = self.next_request_id();
                let pending = PendingControlRequest {
                    id: request_id.clone(),
                    device_id: device_id.clone(),
                    kind: PendingControlKind::RequestLease,
                    lease_id: None,
                    lease_epoch: None,
                };
                let event = RmsControlEvent::RequestControlLease {
                    request_id: request_id.clone(),
                    device_id: device_id.clone(),
                    expected_device_version,
                };
                if let Some(session) = self.matched_live_session_mut()
                    && let Some(control) = session.control.as_mut()
                {
                    control.dispatch(pending, event);
                }
                self.status_message = Some("제어권을 요청하고 있습니다.".to_owned());
            }
            UiAction::RequestCommand(command) => {
                if let Some(session) = self.matched_live_session_mut()
                    && let Some(control) = session.control.as_mut()
                {
                    control.pending_confirmation = Some(command);
                }
            }
            UiAction::ConfirmCommand(command) => {
                if let Some(session) = self.matched_live_session_mut()
                    && let Some(control) = session.control.as_mut()
                {
                    control.pending_confirmation = None;
                }
                if !self.control_enabled() {
                    self.status_message =
                        Some("LIVE 상태와 제어권을 다시 확인해 주세요.".to_owned());
                    return;
                }

                let Some(session) = self.matched_live_session() else {
                    return;
                };
                let Some(control) = session.control.as_ref() else {
                    return;
                };
                let Some(lease) = control.lease.as_ref() else {
                    self.status_message = Some("제어권을 다시 확인해 주세요.".to_owned());
                    return;
                };
                let device_id = session.context.device_id.clone();
                let expected_device_version = session.context.device_state_version;
                let lease_id = lease.id.clone();
                let lease_epoch = lease.epoch;
                let request_id = self.next_request_id();
                let pending = PendingControlRequest {
                    id: request_id.clone(),
                    device_id: device_id.clone(),
                    kind: PendingControlKind::Command,
                    lease_id: Some(lease_id.clone()),
                    lease_epoch: Some(lease_epoch),
                };
                let event = RmsControlEvent::SendControlCommand {
                    request_id: request_id.clone(),
                    device_id: device_id.clone(),
                    command_type: command.api_value().to_owned(),
                    expected_device_version,
                    lease_id: lease_id.clone(),
                    lease_epoch,
                    session_mode: "live".to_owned(),
                };
                if let Some(session) = self.matched_live_session_mut()
                    && let Some(control) = session.control.as_mut()
                {
                    control.dispatch(pending, event);
                }
                self.status_message = Some(format!("{} 요청 중", command.label()));
            }
        }
    }

    /// Applies an authenticated control result returned by the RMS backend adapter.
    ///
    /// Replay ignores normal control responses. A late lease grant from a retired Live request is
    /// released immediately through the one-shot cleanup sink retained during the transition.
    pub fn apply_control_response(&mut self, response: RmsControlResponse) {
        match response {
            RmsControlResponse::LeaseGranted {
                request_id,
                device_id,
                lease_id,
                lease_epoch,
                holder_id,
                holder_name,
                expires_at_ms,
            } => {
                let lease = ControlLease {
                    device_id: device_id.clone(),
                    id: lease_id,
                    epoch: lease_epoch,
                    holder_id,
                    expires_at_ms,
                };
                let pending_matches = self.matched_live_session().is_some_and(|session| {
                    session.control.as_ref().is_some_and(|control| {
                        control.pending.as_ref().is_some_and(|pending| {
                            pending.id == request_id
                                && pending.device_id == device_id
                                && pending.kind == PendingControlKind::RequestLease
                        })
                    })
                });
                let context_matches = self.matched_live_session().is_some_and(|session| {
                    !self.shutdown_requested
                        && session.context.device_id == device_id
                        && session.context.operator_id == lease.holder_id
                        && self.viewer_is_live_and_ready()
                        && lease.expires_at_ms > current_time_ms()
                });
                if pending_matches && context_matches {
                    if let Some(session) = self.matched_live_session_mut()
                        && let Some(control) = session.control.as_mut()
                    {
                        control.pending = None;
                        control.lease = Some(lease);
                    }
                    self.status_message = Some(format!("{holder_name} 제어권 확보"));
                } else {
                    let retired_sink = self
                        .retired_control_requests
                        .take_lease_request_sink(&request_id, &device_id);
                    if pending_matches {
                        if let Some(session) = self.matched_live_session_mut()
                            && let Some(control) = session.control.as_mut()
                        {
                            control.lease = Some(lease);
                        }
                        self.release_active_lease(
                            "현재 화면과 다른 제어권을 안전하게 반납하고 있습니다.",
                        );
                    } else {
                        let cleanup_sink = retired_sink.or_else(|| {
                            self.matched_live_session()
                                .and_then(|session| session.control.as_ref())
                                .map(|control| control.event_sink.clone())
                        });
                        if let Some(event_sink) = cleanup_sink {
                            self.emit_retired_release(&event_sink, lease);
                        }
                        self.status_message = Some(
                            "현재 화면과 다른 제어권을 안전하게 반납하고 있습니다.".to_owned(),
                        );
                    }
                }
            }
            RmsControlResponse::LeaseReleased {
                request_id,
                device_id,
                lease_id,
                lease_epoch,
            } => {
                let pending_matches = self.matched_live_session().is_some_and(|session| {
                    session.control.as_ref().is_some_and(|control| {
                        control.pending.as_ref().is_some_and(|pending| {
                            pending.id == request_id
                                && pending.device_id == device_id
                                && pending.kind == PendingControlKind::ReleaseLease
                                && pending.lease_id.as_deref() == Some(lease_id.as_str())
                                && pending.lease_epoch == Some(lease_epoch)
                        })
                    })
                });
                let retired_matches = self.retired_control_requests.remove_lease_release(
                    &request_id,
                    &device_id,
                    &lease_id,
                    lease_epoch,
                );
                if pending_matches {
                    if let Some(session) = self.matched_live_session_mut()
                        && let Some(control) = session.control.as_mut()
                    {
                        control.pending = None;
                        control.lease = None;
                    }
                    self.status_message = Some("제어권을 반납했습니다.".to_owned());
                } else if retired_matches {
                    self.status_message = Some("이전 제어권을 반납했습니다.".to_owned());
                }
            }
            RmsControlResponse::CommandUpdated {
                request_id,
                device_id,
                message,
            } => {
                let pending_matches = self.matched_live_session().is_some_and(|session| {
                    session.control.as_ref().is_some_and(|control| {
                        control.pending.as_ref().is_some_and(|pending| {
                            pending.id == request_id
                                && pending.device_id == device_id
                                && pending.kind == PendingControlKind::Command
                        })
                    })
                });
                if pending_matches {
                    if let Some(session) = self.matched_live_session_mut()
                        && let Some(control) = session.control.as_mut()
                    {
                        control.pending = None;
                    }
                    self.status_message = Some(message);
                }
            }
            RmsControlResponse::Failed {
                request_id,
                device_id,
                message,
            } => {
                let pending_matches = self.matched_live_session().is_some_and(|session| {
                    session.control.as_ref().is_some_and(|control| {
                        control.pending.as_ref().is_some_and(|pending| {
                            pending.id == request_id && pending.device_id == device_id
                        })
                    })
                });
                let retired_matches = self
                    .retired_control_requests
                    .remove_terminal_failure(&request_id, &device_id);
                if pending_matches {
                    if let Some(session) = self.matched_live_session_mut()
                        && let Some(control) = session.control.as_mut()
                    {
                        control.pending = None;
                        control.lease = None;
                    }
                    self.status_message = Some(message);
                } else if retired_matches {
                    self.status_message = Some(message);
                }
            }
        }
        self.egui_ctx.request_repaint();
    }

    fn synchronize_replay_initialization(&mut self) -> bool {
        let Some(stage) = self.pending_replay_initialization else {
            return false;
        };
        let Some(ActiveViewerContext::Replay(context)) = self.matched_viewer_context() else {
            self.pending_replay_initialization = None;
            return false;
        };
        if !self
            .rerun_app
            .active_recording_loaded_from_url(&context.source_url)
        {
            return true;
        }

        match stage {
            ReplayInitializationStage::ApplyPlaybackPolicy => {
                let commands = replay_initialization_commands(context);
                if self.send_time_commands(commands) {
                    self.pending_replay_initialization =
                        Some(ReplayInitializationStage::PlaybackPolicyApplied);
                }
            }
            ReplayInitializationStage::PlaybackPolicyApplied => {
                // `rerun_app.logic` processed the ordered command batch before this callback.
                self.pending_replay_initialization = None;
            }
        }
        true
    }

    fn synchronize_rerun_state(&mut self) {
        let Some(_active_recording_id) = self.rerun_app.active_recording_id() else {
            return;
        };
        if self.matched_viewer_context().is_some_and(|context| {
            !self
                .rerun_app
                .active_recording_loaded_from_url(context.source_url())
        }) {
            return;
        }
        self.waiting_for_source_change_from = None;

        if self.synchronize_replay_initialization() {
            return;
        }

        if let Some(desired) = self.pending_play_state {
            if self.rerun_app.active_play_state() != Some(desired) {
                self.send_time_command(TimeControlCommand::SetPlayState(desired));
                return;
            }
            self.pending_play_state = None;
            if self
                .matched_live_session()
                .and_then(|session| session.control.as_ref())
                .is_none_or(|control| control.pending.is_none())
            {
                self.status_message = None;
            }
        }

        if matches!(self.viewer_context, Some(ActiveViewerContext::Live(_)))
            && self.rerun_app.active_play_state() != Some(PlayState::Following)
        {
            if self
                .matched_live_session()
                .and_then(|session| session.control.as_ref())
                .is_some_and(|control| control.lease.is_some())
            {
                self.release_active_lease("LIVE 상태가 변경되어 제어권을 반납합니다.");
            }
            self.pending_play_state = Some(PlayState::Following);
            self.send_time_command(TimeControlCommand::SetPlayState(PlayState::Following));
            return;
        }

        self.synchronize_product_blueprint();
    }

    fn expire_control_lease(&mut self) {
        let expired = self
            .matched_live_session()
            .and_then(|session| session.control.as_ref())
            .and_then(|control| control.lease.as_ref())
            .is_some_and(|lease| lease.expires_at_ms <= current_time_ms());
        if expired {
            self.release_active_lease("제어권이 만료되었습니다.");
        }
    }

    fn top_bar(&self, ui: &mut egui::Ui) {
        let context = self.matched_viewer_context();
        let project_name = context.map_or("프로젝트", ActiveViewerContext::project_name);
        let device_name = context.map_or("장비", ActiveViewerContext::device_name);
        let device_status = match context {
            Some(ActiveViewerContext::Live(session)) => device_health_label(
                &session.context.device_status,
                &session.context.device_health,
            ),
            Some(ActiveViewerContext::Replay(_)) => "기록",
            None => "연결 중",
        };
        let badge = context.map_or("—", ActiveViewerContext::badge);
        ui.horizontal(|ui| {
            ui.add_space(8.0);
            ui.strong("RMS");
            ui.separator();
            ui.label(project_name);
            ui.label("/");
            ui.strong(device_name);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.add_space(8.0);
                ui.strong(badge);
                ui.label(device_status);
            });
        });
    }

    fn context_panel(&mut self, ui: &mut egui::Ui) {
        ui.heading("프로젝트");
        ui.label(
            self.matched_viewer_context()
                .map_or("연결 중", ActiveViewerContext::project_name),
        );
        ui.add_space(12.0);
        ui.strong("디바이스");
        ui.add_space(4.0);
        ui.label(
            self.matched_viewer_context()
                .map_or("연결 중", ActiveViewerContext::device_name),
        );

        ui.add_space(14.0);
        ui.strong("서비스");
        ui.add_space(4.0);
        let is_live = matches!(
            self.matched_viewer_context(),
            Some(ActiveViewerContext::Live(_))
        );
        let is_replay = matches!(
            self.matched_viewer_context(),
            Some(ActiveViewerContext::Replay(_))
        );
        if ui.selectable_label(is_live, "실시간").clicked() && !is_live {
            self.apply_action(UiAction::OpenLive);
        }
        if ui.selectable_label(is_replay, "Replay").clicked() && !is_replay {
            self.apply_action(UiAction::OpenReplay);
        }
    }

    fn topic_panel(&mut self, ui: &mut egui::Ui) {
        ui.heading("뷰");
        let previous_preset = self.preset;
        ui.horizontal_wrapped(|ui| {
            for preset in ViewerPreset::ALL {
                ui.selectable_value(&mut self.preset, preset, preset.label());
            }
        });
        if self.preset != previous_preset {
            self.show_all_topics = false;
        }
        ui.separator();
        ui.strong("토픽");
        ui.add_space(4.0);

        if let Some(context) = self.matched_viewer_context() {
            let topics = context
                .topics()
                .iter()
                .filter(|topic| self.topic_matches_preset(topic))
                .cloned()
                .collect::<Vec<_>>();
            let visible_count = if self.show_all_topics {
                topics.len()
            } else {
                topics.len().min(DEFAULT_VISIBLE_TOPIC_COUNT)
            };
            egui::ScrollArea::vertical()
                .max_height(176.0)
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    for topic in &topics[..visible_count] {
                        ui.horizontal(|ui| {
                            ui.label(&topic.label);
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    ui.weak(topic.value.as_deref().unwrap_or("—"));
                                },
                            );
                        });
                        ui.add_space(6.0);
                    }
                });
            if topics.len() > DEFAULT_VISIBLE_TOPIC_COUNT {
                let label = if self.show_all_topics {
                    "간단히 보기".to_owned()
                } else {
                    format!("{}개 더보기", topics.len() - DEFAULT_VISIBLE_TOPIC_COUNT)
                };
                if ui.small_button(label).clicked() {
                    self.show_all_topics = !self.show_all_topics;
                }
            }
        } else {
            for topic in self.topics() {
                ui.horizontal(|ui| {
                    ui.label(topic.label);
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.weak(topic.value);
                    });
                });
                ui.add_space(6.0);
            }
        }
    }

    fn bottom_bar(&self, ui: &mut egui::Ui) -> Option<UiAction> {
        match self.matched_viewer_context() {
            Some(ActiveViewerContext::Live(session)) => self.live_control_bar(ui, session),
            Some(ActiveViewerContext::Replay(context)) => self.replay_bar(ui, context),
            None => {
                ui.horizontal(|ui| {
                    ui.add_space(8.0);
                    ui.weak(self.status_message.as_deref().unwrap_or("서비스 연결 중"));
                });
                None
            }
        }
    }

    fn live_control_bar(&self, ui: &mut egui::Ui, session: &LiveSession) -> Option<UiAction> {
        let mut action = None;
        ui.horizontal(|ui| {
            ui.add_space(8.0);
            if ui.button("과거 보기").clicked() {
                action = Some(UiAction::OpenReplay);
            }

            if let Some(control) = session.control.as_ref() {
                ui.separator();
                let lease_label = if control.pending.is_some() {
                    "처리 중"
                } else if control.lease.is_some() {
                    "제어권 반납"
                } else {
                    "제어권 요청"
                };
                let lease_toggle_enabled = if control.lease.is_some() {
                    control.pending.is_none()
                } else {
                    self.lease_request_allowed()
                };
                if ui
                    .add_enabled(lease_toggle_enabled, egui::Button::new(lease_label))
                    .clicked()
                {
                    action = Some(UiAction::ToggleLease);
                }

                let control_enabled = self.control_enabled();
                if ui
                    .add_enabled(control_enabled, egui::Button::new("임무 일시정지"))
                    .clicked()
                {
                    action = Some(UiAction::RequestCommand(ControlCommand::PauseMission));
                }
                if ui
                    .add_enabled(control_enabled, egui::Button::new("안전 정지"))
                    .clicked()
                {
                    action = Some(UiAction::RequestCommand(ControlCommand::SafeStop));
                }

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.add_space(8.0);
                    if let Some(message) = &self.status_message {
                        ui.label(message);
                    } else if !control_enabled {
                        ui.weak("제어권 필요");
                    }
                });
            } else {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.add_space(8.0);
                    ui.weak(self.status_message.as_deref().unwrap_or("관제 전용"));
                });
            }
        });
        action
    }

    fn replay_bar(&self, ui: &mut egui::Ui, context: &ReplayViewerContext) -> Option<UiAction> {
        let mut action = None;
        ui.horizontal(|ui| {
            ui.add_space(8.0);
            if ui.button("LIVE 열기").clicked() {
                action = Some(UiAction::OpenLive);
            }
            ui.separator();
            let timeline_expanded = self
                .rerun_app
                .time_panel_override()
                .is_some_and(|state| state.is_expanded());
            if ui
                .button(if timeline_expanded {
                    "타임라인 접기"
                } else {
                    "타임라인"
                })
                .clicked()
            {
                action = Some(UiAction::ToggleReplayTimeline);
            }
            ui.separator();
            ui.strong(&context.recording_name);
            if let Some(captured_at) = &context.captured_at_label {
                ui.weak(captured_at);
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.add_space(8.0);
                if let Some(message) = &self.status_message {
                    ui.label(message);
                }
            });
        });
        action
    }

    fn confirmation_window(&mut self, ctx: &egui::Context) -> Option<UiAction> {
        let command = self
            .matched_live_session()
            .and_then(|session| session.control.as_ref())
            .and_then(|control| control.pending_confirmation)?;
        let mut action = None;
        let mut open = true;
        egui::Window::new("명령 확인")
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .collapsible(false)
            .resizable(false)
            .open(&mut open)
            .show(ctx, |ui| {
                ui.strong(command.label());
                ui.label(format!(
                    "대상: {}",
                    self.matched_viewer_context()
                        .map_or("장비", ActiveViewerContext::device_name)
                ));
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button("취소").clicked()
                        && let Some(session) = self.matched_live_session_mut()
                        && let Some(control) = session.control.as_mut()
                    {
                        control.pending_confirmation = None;
                    }
                    if ui.button("실행").clicked() {
                        action = Some(UiAction::ConfirmCommand(command));
                    }
                });
            });
        if !open
            && let Some(session) = self.matched_live_session_mut()
            && let Some(control) = session.control.as_mut()
        {
            control.pending_confirmation = None;
        }
        action
    }
}

fn time_panel_state_for_live(is_live: bool) -> PanelState {
    if is_live {
        PanelState::Hidden
    } else {
        PanelState::Collapsed
    }
}

fn toggled_replay_time_panel_state(current: Option<PanelState>) -> PanelState {
    if current.is_some_and(|state| state.is_expanded()) {
        PanelState::Collapsed
    } else {
        PanelState::Expanded
    }
}

fn replay_time_value(value: &ReplayTimeValue) -> Option<i64> {
    value.value.parse().ok()
}

fn replay_initialization_commands(context: &ReplayViewerContext) -> Vec<TimeControlCommand> {
    use re_viewer_context::external::re_log_types::{AbsoluteTimeRange, TimeReal, TimelineName};

    let mut commands = Vec::with_capacity(8);
    if let Ok(timeline) = TimelineName::try_new(&context.initial_timeline) {
        commands.push(TimeControlCommand::SetActiveTimeline(timeline));
    }
    let initial_fps = context
        .initial_fps
        .filter(|fps| fps.is_finite() && *fps > 0.0);
    if let Some(fps) = initial_fps {
        commands.push(TimeControlCommand::SetFps(fps));
    }
    if let Some(cursor) = context.initial_cursor.as_ref().and_then(replay_time_value) {
        commands.push(TimeControlCommand::SetTime(TimeReal::from(cursor)));
    }

    let sequence_without_valid_fps = context
        .initial_cursor
        .as_ref()
        .is_some_and(|cursor| cursor.kind == ReplayTimeKind::Sequence)
        && initial_fps.is_none();
    commands.push(TimeControlCommand::SetPlayState(
        match (context.initial_play_state, sequence_without_valid_fps) {
            (_, true) | (ReplayInitialPlayState::Paused, false) => PlayState::Paused,
            (ReplayInitialPlayState::Playing, false) => PlayState::Playing,
        },
    ));
    let speed = if context.initial_speed.is_finite() && context.initial_speed > 0.0 {
        context.initial_speed
    } else {
        default_replay_speed()
    };
    commands.push(TimeControlCommand::SetSpeed(speed));

    match context.initial_loop.mode {
        ReplayInitialLoopMode::Off => {
            commands.push(TimeControlCommand::SetLoopMode(LoopMode::Off));
        }
        ReplayInitialLoopMode::All => {
            commands.push(TimeControlCommand::SetLoopMode(LoopMode::All));
        }
        ReplayInitialLoopMode::Selection => {
            let selection = context
                .initial_loop
                .start
                .as_ref()
                .zip(context.initial_loop.end.as_ref())
                .filter(|(start, end)| start.kind == end.kind)
                .and_then(|(start, end)| replay_time_value(start).zip(replay_time_value(end)))
                .filter(|(start, end)| start <= end);
            if let Some((start, end)) = selection {
                commands.push(TimeControlCommand::SetTimeSelection(
                    AbsoluteTimeRange::new(start, end),
                ));
                commands.push(TimeControlCommand::SetLoopMode(LoopMode::Selection));
            } else {
                commands.push(TimeControlCommand::SetLoopMode(LoopMode::Off));
            }
        }
    }

    commands
}

#[expect(
    clippy::large_include_file,
    reason = "the embedded Korean operator UI font must be available offline in native and web builds"
)]
fn install_product_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    fonts.font_data.insert(
        "Inter-Medium".into(),
        std::sync::Arc::new(egui::FontData::from_static(include_bytes!(
            "../../re_ui/data/Inter-Medium.otf"
        ))),
    );
    fonts.font_data.insert(
        "NotoSansKR-RMS".into(),
        std::sync::Arc::new(egui::FontData::from_static(include_bytes!(
            "../data/NotoSansKR-RMS.ttf"
        ))),
    );

    let proportional = fonts
        .families
        .get_mut(&egui::FontFamily::Proportional)
        .expect("egui always defines a proportional font family");
    proportional.insert(0, "Inter-Medium".into());
    proportional.insert(1, "NotoSansKR-RMS".into());

    fonts
        .families
        .get_mut(&egui::FontFamily::Monospace)
        .expect("egui always defines a monospace font family")
        .push("NotoSansKR-RMS".into());
    ctx.set_fonts(fonts);
}

fn control_is_allowed(
    runtime_is_ready: bool,
    context: &LiveViewerContext,
    lease: &ControlLease,
    now_ms: f64,
) -> bool {
    runtime_is_ready
        && context.device_status == "online"
        && !matches!(context.device_health.as_str(), "restricted" | "critical")
        && !context.device_id.is_empty()
        && context.device_state_version > 0
        && lease.device_id == context.device_id
        && lease.holder_id == context.operator_id
        && lease.expires_at_ms > now_ms
}

struct SourceTransitionState {
    waiting_for_source_change: bool,
    pending_play_state: bool,
    has_active_recording: bool,
    play_state: Option<PlayState>,
    active_source_matches: bool,
}

fn source_transition_is_ready(state: &SourceTransitionState) -> bool {
    !state.waiting_for_source_change
        && !state.pending_play_state
        && state.has_active_recording
        && state.play_state == Some(PlayState::Following)
        && state.active_source_matches
}

struct ControlCapabilityUpdate {
    same_session: bool,
    had_control_capability: bool,
    requested_control_capability: bool,
}

fn control_capability_was_revoked(update: &ControlCapabilityUpdate) -> bool {
    update.same_session && update.had_control_capability && !update.requested_control_capability
}

fn device_health_label(status: &str, health: &str) -> &'static str {
    match (status, health) {
        ("offline", _) => "연결 끊김",
        ("degraded", _) => "지연",
        (_, "critical") => "위험",
        (_, "restricted") => "제한",
        (_, "attention") => "주의",
        _ => "정상",
    }
}

#[cfg(target_arch = "wasm32")]
fn current_time_ms() -> f64 {
    js_sys::Date::now()
}

#[cfg(not(target_arch = "wasm32"))]
fn current_time_ms() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0.0, |duration| duration.as_secs_f64() * 1_000.0)
}

impl eframe::App for RmsProductApp {
    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        self.rerun_app.save(storage);
    }

    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        egui::Panel::top("rms_status_bar")
            .exact_size(42.0)
            .show(ui, |ui| self.top_bar(ui));

        egui::Panel::left("rms_context_panel")
            .default_size(210.0)
            .min_size(180.0)
            .max_size(260.0)
            .resizable(true)
            .show(ui, |ui| self.context_panel(ui));

        egui::Panel::right("rms_topic_panel")
            .default_size(230.0)
            .min_size(190.0)
            .max_size(300.0)
            .resizable(true)
            .show(ui, |ui| self.topic_panel(ui));

        let mut action = None;
        egui::Panel::bottom("rms_service_bar")
            .exact_size(48.0)
            .show(ui, |ui| action = self.bottom_bar(ui));

        self.rerun_app.ui(ui, frame);

        if let Some(action) = action.or_else(|| self.confirmation_window(ui.ctx())) {
            self.apply_action(action);
        }
    }

    fn logic(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        self.rerun_app.logic(ctx, frame);
        self.synchronize_rerun_state();
        self.expire_control_lease();
    }

    #[cfg(target_arch = "wasm32")]
    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(&mut *self)
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;

    use re_sdk_types::blueprint::components::{LoopMode, PanelState, PlayState};

    use super::{
        ActiveViewerContext, BlueprintPresetKey, ControlCapabilityUpdate, ControlLease,
        EMPTY_PRODUCT_QUERY, LiveControlCapability, LiveSession, LiveViewerContext,
        PendingControlKind, PendingControlRequest, ReplayInitialLoop, ReplayInitialLoopMode,
        ReplayInitialPlayState, ReplayTimeKind, ReplayTimeValue, ReplayViewerContext,
        RetiredControlRequests, RmsControlEvent, RmsTopicContext, SourceTransitionState,
        TimeControlCommand, ViewerPreset, blueprint_retry_delay_ms, control_capability_was_revoked,
        control_is_allowed, product_blueprint_messages, product_topics_hash, product_view_specs,
        record_blueprint_dispatch_failure, replay_initialization_commands,
        source_transition_is_ready, time_panel_state_for_live, toggled_replay_time_panel_state,
    };

    fn live_context() -> LiveViewerContext {
        LiveViewerContext {
            project_id: "project-1".to_owned(),
            project_name: "Test".to_owned(),
            device_id: "robot-07".to_owned(),
            device_name: "Robot 07".to_owned(),
            device_status: "online".to_owned(),
            device_health: "normal".to_owned(),
            device_state_version: 142,
            live_session_id: "live-session-1".to_owned(),
            data_source_id: "robot-07-live".to_owned(),
            source_name: "Live".to_owned(),
            source_url: "https://example.invalid/live.rrd".to_owned(),
            operator_id: "operator-01".to_owned(),
            control_enabled: true,
            topics: vec![RmsTopicContext {
                label: "Pose".to_owned(),
                path: "/pose".to_owned(),
                renderer: "spatial".to_owned(),
                value: None,
            }],
        }
    }

    fn replay_context() -> ReplayViewerContext {
        ReplayViewerContext {
            project_id: "project-1".to_owned(),
            project_name: "Test".to_owned(),
            device_id: "robot-07".to_owned(),
            device_name: "Robot 07".to_owned(),
            recording_id: "recording-1".to_owned(),
            recording_name: "Incident".to_owned(),
            replay_session_id: "replay-session-1".to_owned(),
            source_url: "https://example.invalid/replay.rrd".to_owned(),
            captured_at_label: None,
            initial_timeline: "tick".to_owned(),
            initial_fps: Some(2.0),
            initial_cursor: Some(ReplayTimeValue {
                kind: ReplayTimeKind::Sequence,
                value: "0".to_owned(),
            }),
            initial_play_state: ReplayInitialPlayState::Paused,
            initial_speed: 1.0,
            initial_loop: ReplayInitialLoop::default(),
            topics: Vec::new(),
        }
    }

    fn lease() -> ControlLease {
        ControlLease {
            device_id: "robot-07".to_owned(),
            id: "lease-1".to_owned(),
            epoch: 1,
            holder_id: "operator-01".to_owned(),
            expires_at_ms: 2_000.0,
        }
    }

    #[test]
    fn replay_context_has_no_live_control_capability() {
        let context = ActiveViewerContext::Replay(Box::new(replay_context()));
        assert!(matches!(context, ActiveViewerContext::Replay(_)));
        assert_eq!(context.badge(), "REPLAY");
    }

    #[test]
    fn product_blueprint_key_is_order_independent_and_source_scoped() {
        let store_id =
            re_log_types::StoreId::random(re_log_types::StoreKind::Recording, "rms-blueprint-test");
        let topics = vec![
            RmsTopicContext {
                label: "Camera".to_owned(),
                path: "/camera/front".to_owned(),
                renderer: "camera".to_owned(),
                value: None,
            },
            RmsTopicContext {
                label: "Pose".to_owned(),
                path: "/pose".to_owned(),
                renderer: "spatial".to_owned(),
                value: None,
            },
        ];
        let mut reversed = topics.clone();
        reversed.reverse();
        let key = BlueprintPresetKey::new(store_id.clone(), ViewerPreset::Operations, &topics);
        assert_eq!(
            key,
            BlueprintPresetKey::new(store_id.clone(), ViewerPreset::Operations, &reversed)
        );
        assert_ne!(
            key,
            BlueprintPresetKey::new(store_id, ViewerPreset::Camera, &topics)
        );
        assert_ne!(
            key,
            BlueprintPresetKey::new(
                re_log_types::StoreId::random(
                    re_log_types::StoreKind::Recording,
                    "rms-blueprint-test"
                ),
                ViewerPreset::Operations,
                &topics,
            )
        );
    }

    #[test]
    fn spatial_preset_separates_2d_content_from_3d_geometry_and_accepts_legacy_alias() {
        let topics = vec![
            RmsTopicContext {
                label: "Map".to_owned(),
                path: "/map".to_owned(),
                renderer: "spatial2d".to_owned(),
                value: None,
            },
            RmsTopicContext {
                label: "Costmap".to_owned(),
                path: "/local_costmap/costmap".to_owned(),
                renderer: "spatial2d".to_owned(),
                value: None,
            },
            RmsTopicContext {
                label: "Transforms".to_owned(),
                path: "/tf".to_owned(),
                renderer: "spatial".to_owned(),
                value: None,
            },
            RmsTopicContext {
                label: "Legacy pose".to_owned(),
                path: "/pose".to_owned(),
                renderer: "spatial".to_owned(),
                value: None,
            },
            RmsTopicContext {
                label: "Camera".to_owned(),
                path: "/camera/front".to_owned(),
                renderer: "camera".to_owned(),
                value: None,
            },
        ];

        let views = product_view_specs(ViewerPreset::Spatial, &topics);
        assert_eq!(views.len(), 4);
        assert_eq!(views[0].class_identifier, "2D");
        assert_eq!(views[0].space_origin, "/local_costmap/costmap");
        assert_eq!(views[0].contents, ["/local_costmap/costmap"]);
        assert_eq!(views[1].class_identifier, "2D");
        assert_eq!(views[1].space_origin, "/map");
        assert_eq!(views[1].contents, ["/map"]);
        assert_eq!(views[2].class_identifier, "3D");
        assert_eq!(views[2].space_origin, "/pose");
        assert_eq!(views[2].contents, ["/pose"]);
        assert!(views[2].transform_axes.is_empty());
        assert_eq!(views[3].class_identifier, "3D");
        assert_eq!(views[3].space_origin, "/tf");
        assert_eq!(views[3].contents, ["/tf"]);
        assert_eq!(views[3].transform_axes, ["/tf"]);
        assert!(
            views
                .iter()
                .flat_map(|view| &view.contents)
                .all(|path| path != "/camera/front")
        );
    }

    #[test]
    fn grid_maps_use_independent_3d_origins_before_dynamic_tf_arrives() {
        let topics = [
            RmsTopicContext {
                label: "costmap".to_owned(),
                path: "/local_costmap/costmap".to_owned(),
                renderer: "spatial3d".to_owned(),
                value: None,
            },
            RmsTopicContext {
                label: "map".to_owned(),
                path: "/map".to_owned(),
                renderer: "spatial3d".to_owned(),
                value: None,
            },
            RmsTopicContext {
                label: "tf drop".to_owned(),
                path: "/tf_drop".to_owned(),
                renderer: "transform3d".to_owned(),
                value: None,
            },
            RmsTopicContext {
                label: "tf static".to_owned(),
                path: "/tf_static".to_owned(),
                renderer: "transform3d".to_owned(),
                value: None,
            },
        ];

        let views = product_view_specs(ViewerPreset::Spatial, &topics);

        assert_eq!(views.len(), 3);
        assert_eq!(views[0].class_identifier, "3D");
        assert_eq!(views[0].contents, ["/local_costmap/costmap"]);
        assert_eq!(views[0].space_origin, "/local_costmap/costmap");
        assert_eq!(views[1].class_identifier, "3D");
        assert_eq!(views[1].contents, ["/map"]);
        assert_eq!(views[1].space_origin, "/map");
        assert_eq!(views[2].class_identifier, "3D");
        assert_eq!(views[2].contents, ["/tf_drop", "/tf_static"]);
        assert_eq!(views[2].space_origin, "/tf_drop");
        assert_eq!(views[2].transform_axes, ["/tf_drop", "/tf_static"]);
    }

    #[test]
    fn raw_ros_archetypes_use_dataframe_instead_of_an_empty_text_log_view() {
        let topics = [
            RmsTopicContext {
                label: "odometry".to_owned(),
                path: "/direct_laser_odometry/odom".to_owned(),
                renderer: "raw".to_owned(),
                value: None,
            },
            RmsTopicContext {
                label: "laser scan".to_owned(),
                path: "/scan_raw".to_owned(),
                renderer: "raw".to_owned(),
                value: None,
            },
            RmsTopicContext {
                label: "rosout".to_owned(),
                path: "/rosout".to_owned(),
                renderer: "log".to_owned(),
                value: None,
            },
        ];

        let views = product_view_specs(ViewerPreset::Diagnostics, &topics);

        assert_eq!(views.len(), 2);
        assert_eq!(views[0].class_identifier, "Dataframe");
        assert_eq!(
            views[0].contents,
            ["/direct_laser_odometry/odom", "/scan_raw"]
        );
        assert_eq!(views[1].class_identifier, "TextLog");
        assert_eq!(views[1].contents, ["/rosout"]);
    }

    #[test]
    fn map_and_unknown_renderers_keep_data_visible_in_safe_views() {
        let topics = [
            RmsTopicContext {
                label: "GPS".to_owned(),
                path: "/fix".to_owned(),
                renderer: "map".to_owned(),
                value: None,
            },
            RmsTopicContext {
                label: "Future payload".to_owned(),
                path: "/future".to_owned(),
                renderer: "future_renderer".to_owned(),
                value: None,
            },
        ];

        let views = product_view_specs(ViewerPreset::Operations, &topics);

        assert_eq!(views.len(), 2);
        assert_eq!(views[0].class_identifier, "Map");
        assert_eq!(views[0].contents, ["/fix"]);
        assert_eq!(views[1].class_identifier, "Dataframe");
        assert_eq!(views[1].contents, ["/future"]);
    }

    #[test]
    fn spatial_geometry_under_a_transforms_path_is_not_mistaken_for_tf() {
        let points = RmsTopicContext {
            label: "Point cloud".to_owned(),
            path: "/transforms/points".to_owned(),
            renderer: "spatial3d".to_owned(),
            value: None,
        };

        let views = product_view_specs(ViewerPreset::Spatial, &[points]);

        assert_eq!(views.len(), 1);
        assert_eq!(views[0].contents, ["/transforms/points"]);
        assert_eq!(views[0].space_origin, "/transforms/points");
        assert!(views[0].transform_axes.is_empty());
    }

    #[test]
    fn a_single_3d_geometry_topic_uses_its_entity_as_space_origin() {
        let points = RmsTopicContext {
            label: "Point cloud".to_owned(),
            path: "/lidar/points".to_owned(),
            renderer: "spatial3d".to_owned(),
            value: None,
        };
        let views = product_view_specs(ViewerPreset::Spatial, &[points]);

        assert_eq!(views.len(), 1);
        assert_eq!(views[0].class_identifier, "3D");
        assert_eq!(views[0].space_origin, "/lidar/points");
        assert!(views[0].transform_axes.is_empty());
    }

    #[test]
    fn transform_tree_blueprint_explicitly_enables_transform_axes() -> Result<(), String> {
        let store_id =
            re_log_types::StoreId::random(re_log_types::StoreKind::Recording, "rms-spatial-test");
        let tf = RmsTopicContext {
            label: "TF".to_owned(),
            path: "/robot/base".to_owned(),
            renderer: "transform3d".to_owned(),
            value: None,
        };
        let messages = product_blueprint_messages(&store_id, ViewerPreset::Spatial, &[tf])?;
        let entity_paths = messages
            .iter()
            .filter_map(|message| match message {
                re_log_types::LogMsg::ArrowMsg(_, arrow_msg) => {
                    re_chunk::Chunk::from_arrow_msg(arrow_msg).ok()
                }
                _ => None,
            })
            .map(|chunk| chunk.entity_path().to_string())
            .collect::<Vec<_>>();

        let visualizer_path_fragment = "/ViewContents/overrides/robot/base/visualizers/";
        assert_eq!(
            entity_paths
                .iter()
                .filter(|path| path.contains(visualizer_path_fragment))
                .count(),
            2,
            "the instruction and its TransformAxes3D overrides must both be logged"
        );
        assert!(
            entity_paths
                .iter()
                .any(|path| { path.ends_with("/ViewContents/overrides/robot/base/visualizers") })
        );
        Ok(())
    }

    #[test]
    fn blueprint_channel_contains_only_blueprint_messages_and_non_default_activation()
    -> Result<(), String> {
        let store_id =
            re_log_types::StoreId::random(re_log_types::StoreKind::Recording, "rms-blueprint-test");
        let messages = product_blueprint_messages(&store_id, ViewerPreset::Camera, &[])?;
        assert!(
            messages
                .iter()
                .all(|message| { message.store_id().kind() == re_log_types::StoreKind::Blueprint })
        );
        assert!(
            messages
                .iter()
                .any(|message| { matches!(message, re_log_types::LogMsg::ArrowMsg(..)) }),
            "zero-topic preset must still log an explicit empty view"
        );
        let activation = messages
            .iter()
            .find_map(|message| match message {
                re_log_types::LogMsg::BlueprintActivationCommand(command) => Some(command),
                _ => None,
            })
            .ok_or_else(|| "missing activation".to_owned())?;
        assert!(activation.make_active);
        assert!(!activation.make_default);
        Ok(())
    }

    #[test]
    fn zero_topic_presets_do_not_fall_back_to_unmatched_topics() {
        let spatial = RmsTopicContext {
            label: "Pose".to_owned(),
            path: "/pose".to_owned(),
            renderer: "spatial".to_owned(),
            value: None,
        };
        assert_eq!(
            product_topics_hash(ViewerPreset::Camera, &[spatial]),
            product_topics_hash(ViewerPreset::Camera, &[])
        );

        let filter = re_log_types::EntityPathFilter::parse_strict(EMPTY_PRODUCT_QUERY)
            .expect("empty product query must use valid filter grammar")
            .resolve_without_substitutions();
        assert!(!filter.matches(
            &re_log_types::EntityPath::parse_strict("/pose").expect("valid test entity path")
        ));
        assert!(
            !filter.matches(
                &re_log_types::EntityPath::parse_strict("/camera/front")
                    .expect("valid test entity path")
            )
        );
    }

    #[test]
    fn blueprint_dispatch_failure_rolls_back_claim_and_uses_bounded_backoff() {
        let store_id = re_log_types::StoreId::random(
            re_log_types::StoreKind::Recording,
            "rms-blueprint-retry-test",
        );
        let key = BlueprintPresetKey::new(store_id, ViewerPreset::Operations, &[]);
        let mut dispatched_key = Some(key.clone());
        let mut retry = None;

        record_blueprint_dispatch_failure(&mut dispatched_key, &mut retry, key.clone(), 10_000.0);
        assert!(dispatched_key.is_none());
        let first = retry.as_ref().expect("failure must install a retry latch");
        assert_eq!(first.attempts, 1);
        assert!(first.blocks(&key, 10_999.0));
        assert!(!first.blocks(&key, 11_000.0));

        dispatched_key = Some(key.clone());
        record_blueprint_dispatch_failure(&mut dispatched_key, &mut retry, key.clone(), 11_000.0);
        assert!(dispatched_key.is_none());
        let second = retry.as_ref().expect("retry latch remains installed");
        assert_eq!(second.attempts, 2);
        assert_eq!(second.retry_after_ms, 13_000.0);
        assert_eq!(blueprint_retry_delay_ms(u8::MAX), 30_000.0);

        let other_key =
            BlueprintPresetKey::new(key.store_id.clone(), ViewerPreset::Diagnostics, &[]);
        assert!(!second.blocks(&other_key, 11_001.0));
    }

    #[test]
    fn live_control_requires_ready_viewer_and_current_lease() {
        let context = live_context();
        let lease = lease();
        assert!(control_is_allowed(true, &context, &lease, 1_000.0));
        assert!(!control_is_allowed(false, &context, &lease, 1_000.0));
        assert!(!control_is_allowed(true, &context, &lease, 3_000.0));
    }

    #[test]
    fn live_session_without_injected_sink_is_observation_only() {
        let session = ActiveViewerContext::Live(Box::new(LiveSession {
            context: live_context(),
            control: None,
        }));
        let ActiveViewerContext::Live(session) = session else {
            unreachable!();
        };
        assert!(session.control.is_none());
    }

    #[test]
    fn restricted_live_device_never_allows_control() {
        let mut context = live_context();
        context.device_health = "restricted".to_owned();
        assert!(!control_is_allowed(true, &context, &lease(), 1_000.0));
    }

    #[test]
    fn source_transition_is_fail_closed_until_following_is_confirmed() {
        assert!(!source_transition_is_ready(&SourceTransitionState {
            waiting_for_source_change: true,
            pending_play_state: false,
            has_active_recording: true,
            play_state: Some(PlayState::Following),
            active_source_matches: true,
        }));
        assert!(!source_transition_is_ready(&SourceTransitionState {
            waiting_for_source_change: false,
            pending_play_state: true,
            has_active_recording: true,
            play_state: Some(PlayState::Following),
            active_source_matches: true,
        }));
        assert!(!source_transition_is_ready(&SourceTransitionState {
            waiting_for_source_change: false,
            pending_play_state: false,
            has_active_recording: true,
            play_state: Some(PlayState::Paused),
            active_source_matches: true,
        }));
        assert!(!source_transition_is_ready(&SourceTransitionState {
            waiting_for_source_change: false,
            pending_play_state: false,
            has_active_recording: true,
            play_state: Some(PlayState::Following),
            active_source_matches: false,
        }));
        assert!(source_transition_is_ready(&SourceTransitionState {
            waiting_for_source_change: false,
            pending_play_state: false,
            has_active_recording: true,
            play_state: Some(PlayState::Following),
            active_source_matches: true,
        }));
    }

    #[test]
    fn time_panel_transitions_replay_collapsed_to_expanded_then_live_hidden() {
        let mut state = time_panel_state_for_live(false);
        assert_eq!(state, PanelState::Collapsed);
        state = toggled_replay_time_panel_state(Some(state));
        assert_eq!(state, PanelState::Expanded);
        state = time_panel_state_for_live(true);
        assert_eq!(state, PanelState::Hidden);
    }

    #[test]
    fn replay_initialization_commands_preserve_contract_order() {
        let commands = replay_initialization_commands(&replay_context());
        assert_eq!(commands.len(), 6);
        assert!(matches!(
            &commands[0],
            TimeControlCommand::SetActiveTimeline(timeline) if timeline.as_str() == "tick"
        ));
        assert!(matches!(&commands[1], TimeControlCommand::SetFps(2.0)));
        assert!(matches!(
            &commands[2],
            TimeControlCommand::SetTime(time) if time.floor().as_i64() == 0
        ));
        assert!(matches!(
            &commands[3],
            TimeControlCommand::SetPlayState(PlayState::Paused)
        ));
        assert!(matches!(&commands[4], TimeControlCommand::SetSpeed(1.0)));
        assert!(matches!(
            &commands[5],
            TimeControlCommand::SetLoopMode(LoopMode::Off)
        ));
    }

    #[test]
    fn invalid_replay_selection_and_speed_fail_safe() {
        let mut context = replay_context();
        context.initial_speed = f32::NAN;
        context.initial_loop = ReplayInitialLoop {
            mode: ReplayInitialLoopMode::Selection,
            start: Some(ReplayTimeValue {
                kind: ReplayTimeKind::Sequence,
                value: "100".to_owned(),
            }),
            end: Some(ReplayTimeValue {
                kind: ReplayTimeKind::Timestamp,
                value: "0".to_owned(),
            }),
        };

        let commands = replay_initialization_commands(&context);
        assert!(matches!(&commands[4], TimeControlCommand::SetSpeed(1.0)));
        assert!(matches!(
            &commands[5],
            TimeControlCommand::SetLoopMode(LoopMode::Off)
        ));
    }

    #[test]
    fn invalid_sequence_fps_prevents_autoplay_at_the_rerun_default() {
        for invalid_fps in [None, Some(f32::NAN), Some(0.0), Some(-2.0)] {
            let mut context = replay_context();
            context.initial_fps = invalid_fps;
            context.initial_play_state = ReplayInitialPlayState::Playing;

            let commands = replay_initialization_commands(&context);
            assert!(
                !commands
                    .iter()
                    .any(|command| matches!(command, TimeControlCommand::SetFps(_)))
            );
            assert!(commands.iter().any(|command| matches!(
                command,
                TimeControlCommand::SetPlayState(PlayState::Paused)
            )));
        }
    }

    #[test]
    fn timestamp_replay_can_play_without_an_fps_override() {
        let mut context = replay_context();
        context.initial_fps = None;
        context.initial_cursor = Some(ReplayTimeValue {
            kind: ReplayTimeKind::Timestamp,
            value: "1767225600000000000".to_owned(),
        });
        context.initial_play_state = ReplayInitialPlayState::Playing;

        let commands = replay_initialization_commands(&context);
        assert!(commands.iter().any(|command| matches!(
            command,
            TimeControlCommand::SetPlayState(PlayState::Playing)
        )));
    }

    #[test]
    fn duration_replay_preserves_nanoseconds_and_can_play_without_fps() {
        let mut context = replay_context();
        context.initial_fps = None;
        context.initial_cursor = Some(ReplayTimeValue {
            kind: ReplayTimeKind::Duration,
            value: "1500000000".to_owned(),
        });
        context.initial_play_state = ReplayInitialPlayState::Playing;
        context.initial_loop = ReplayInitialLoop {
            mode: ReplayInitialLoopMode::Selection,
            start: Some(ReplayTimeValue {
                kind: ReplayTimeKind::Duration,
                value: "500000000".to_owned(),
            }),
            end: Some(ReplayTimeValue {
                kind: ReplayTimeKind::Duration,
                value: "2000000000".to_owned(),
            }),
        };

        let commands = replay_initialization_commands(&context);
        assert!(commands.iter().any(|command| matches!(
            command,
            TimeControlCommand::SetTime(time) if time.floor().as_i64() == 1_500_000_000
        )));
        assert!(commands.iter().any(|command| matches!(
            command,
            TimeControlCommand::SetPlayState(PlayState::Playing)
        )));
        assert!(commands.iter().any(|command| matches!(
            command,
            TimeControlCommand::SetTimeSelection(range)
                if range.min().as_i64() == 500_000_000
                    && range.max().as_i64() == 2_000_000_000
        )));
        assert!(commands.iter().any(|command| matches!(
            command,
            TimeControlCommand::SetLoopMode(LoopMode::Selection)
        )));
    }

    #[test]
    fn replay_selection_is_applied_before_selection_loop_mode() {
        let mut context = replay_context();
        context.initial_loop = ReplayInitialLoop {
            mode: ReplayInitialLoopMode::Selection,
            start: Some(ReplayTimeValue {
                kind: ReplayTimeKind::Sequence,
                value: "10".to_owned(),
            }),
            end: Some(ReplayTimeValue {
                kind: ReplayTimeKind::Sequence,
                value: "20".to_owned(),
            }),
        };

        let commands = replay_initialization_commands(&context);
        assert!(matches!(
            &commands[5],
            TimeControlCommand::SetTimeSelection(range)
                if range.min().as_i64() == 10 && range.max().as_i64() == 20
        ));
        assert!(matches!(
            &commands[6],
            TimeControlCommand::SetLoopMode(LoopMode::Selection)
        ));
    }

    #[test]
    #[expect(
        clippy::disallowed_methods,
        reason = "the test must observe state after a deliberately panicking synchronous callback"
    )]
    fn pending_is_recorded_before_control_callback() {
        let event_sink = Rc::new(|_: RmsControlEvent| panic!("synchronous host callback"));
        let mut capability = LiveControlCapability {
            event_sink,
            lease: None,
            pending: None,
            pending_confirmation: None,
        };
        let pending = PendingControlRequest {
            id: "request-1".to_owned(),
            device_id: "robot-07".to_owned(),
            kind: PendingControlKind::RequestLease,
            lease_id: None,
            lease_epoch: None,
        };
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            capability.dispatch(
                pending,
                RmsControlEvent::RequestControlLease {
                    request_id: "request-1".to_owned(),
                    device_id: "robot-07".to_owned(),
                    expected_device_version: 142,
                },
            );
        }));
        assert!(result.is_err());
        assert!(capability.pending.is_some_and(|pending| {
            pending.id == "request-1" && pending.kind == PendingControlKind::RequestLease
        }));
    }

    #[test]
    fn observation_only_update_revokes_existing_capability() {
        assert!(control_capability_was_revoked(&ControlCapabilityUpdate {
            same_session: true,
            had_control_capability: true,
            requested_control_capability: false,
        }));
        assert!(!control_capability_was_revoked(&ControlCapabilityUpdate {
            same_session: true,
            had_control_capability: false,
            requested_control_capability: false,
        }));
        assert!(!control_capability_was_revoked(&ControlCapabilityUpdate {
            same_session: true,
            had_control_capability: true,
            requested_control_capability: true,
        }));
        assert!(!control_capability_was_revoked(&ControlCapabilityUpdate {
            same_session: false,
            had_control_capability: true,
            requested_control_capability: false,
        }));
    }

    #[test]
    fn shutdown_release_is_dispatched_with_pending_correlation() {
        let events = Rc::new(RefCell::new(Vec::new()));
        let captured_events = events.clone();
        let mut capability = LiveControlCapability {
            event_sink: Rc::new(move |event| captured_events.borrow_mut().push(event)),
            lease: Some(lease()),
            pending: None,
            pending_confirmation: None,
        };

        assert!(capability.release_lease("shutdown-1".to_owned()));
        assert!(capability.lease.is_none());
        assert!(capability.pending.as_ref().is_some_and(|pending| {
            pending.id == "shutdown-1" && pending.kind == PendingControlKind::ReleaseLease
        }));
        assert!(matches!(
            events.borrow().as_slice(),
            [RmsControlEvent::ReleaseControlLease { request_id, .. }]
                if request_id == "shutdown-1"
        ));
    }

    #[test]
    fn retired_lease_requests_are_not_dropped() {
        let event_sink = Rc::new(|_: RmsControlEvent| {});
        let mut requests = RetiredControlRequests::default();
        for index in 0..64 {
            requests.push_lease_request(
                format!("request-{index}"),
                "robot-07".to_owned(),
                event_sink.clone(),
            );
        }
        assert_eq!(requests.0.len(), 64);
        for index in 0..64 {
            assert!(
                requests
                    .take_lease_request_sink(&format!("request-{index}"), "robot-07")
                    .is_some()
            );
        }
        assert!(requests.is_empty());
    }
}
