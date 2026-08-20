use std::rc::Rc;

use eframe::egui;
use re_sdk_types::blueprint::components::{PanelState, PlayState};
use re_viewer::{CommandSender, PanelStateOverrides, SystemCommand, SystemCommandSender as _};
use re_viewer_context::TimeControlCommand;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ViewerPreset {
    Operations,
    Camera,
    Diagnostics,
}

impl ViewerPreset {
    const ALL: [Self; 3] = [Self::Operations, Self::Camera, Self::Diagnostics];

    fn label(self) -> &'static str {
        match self {
            Self::Operations => "운영",
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
    ToggleLease,
    RequestCommand(ControlCommand),
    ConfirmCommand(ControlCommand),
}

/// Product-owned RMS application that renders the operational shell and Rerun viewport together.
pub struct RmsProductApp {
    rerun_app: re_viewer::App,
    egui_ctx: egui::Context,
    command_sender: CommandSender,
    preset: ViewerPreset,
    viewer_context: Option<ActiveViewerContext>,
    retired_control_requests: RetiredControlRequests,
    next_request_number: u64,
    pending_play_state: Option<PlayState>,
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
            viewer_context: None,
            retired_control_requests: RetiredControlRequests::default(),
            next_request_number: 0,
            pending_play_state: source_url.map(|_| PlayState::Paused),
            waiting_for_source_change_from: None,
            host_event_sink,
            status_message: None,
            shutdown_requested: false,
        })
    }

    fn topics(&self) -> &'static [TopicSummary] {
        match self.preset {
            ViewerPreset::Operations => &OPERATION_TOPICS,
            ViewerPreset::Camera => &CAMERA_TOPICS,
            ViewerPreset::Diagnostics => &DIAGNOSTIC_TOPICS,
        }
    }

    fn topic_matches_preset(&self, topic: &RmsTopicContext) -> bool {
        match self.preset {
            ViewerPreset::Operations => true,
            ViewerPreset::Camera => topic.renderer == "camera",
            ViewerPreset::Diagnostics => {
                matches!(topic.renderer.as_str(), "state" | "log" | "timeseries")
            }
        }
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
        let Some(store_id) = self.rerun_app.active_recording_id().cloned() else {
            return;
        };

        self.command_sender
            .send_system(SystemCommand::TimeControlCommands {
                store_id,
                time_commands: vec![command],
            });
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

        self.clear_active_context();
        self.viewer_context = Some(ActiveViewerContext::Replay(Box::new(context)));
        self.open_source_if_changed(&source_url, source_changed || source_url_changed);
        self.pending_play_state = (!same_session).then_some(PlayState::Paused);
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
        self.waiting_for_source_change_from = None;
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

    fn synchronize_rerun_state(&mut self) {
        let Some(active_recording_id) = self.rerun_app.active_recording_id() else {
            return;
        };
        if self
            .waiting_for_source_change_from
            .as_ref()
            .is_some_and(|previous| previous == &active_recording_id.to_string())
        {
            return;
        }
        self.waiting_for_source_change_from = None;

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
        }
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
        ui.horizontal_wrapped(|ui| {
            for preset in ViewerPreset::ALL {
                ui.selectable_value(&mut self.preset, preset, preset.label());
            }
        });
        ui.separator();
        ui.strong("토픽");
        ui.add_space(4.0);

        if let Some(context) = self.matched_viewer_context() {
            for topic in context
                .topics()
                .iter()
                .filter(|topic| self.topic_matches_preset(topic))
            {
                ui.horizontal(|ui| {
                    ui.label(&topic.label);
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.weak(topic.value.as_deref().unwrap_or("—"));
                    });
                });
                ui.add_space(6.0);
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

    use re_sdk_types::blueprint::components::PlayState;

    use super::{
        ActiveViewerContext, ControlCapabilityUpdate, ControlLease, LiveControlCapability,
        LiveSession, LiveViewerContext, PendingControlKind, PendingControlRequest,
        ReplayViewerContext, RetiredControlRequests, RmsControlEvent, RmsTopicContext,
        SourceTransitionState, control_capability_was_revoked, control_is_allowed,
        source_transition_is_ready,
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
