use std::rc::Rc;

use eframe::egui;
use re_sdk_types::blueprint::components::{PanelState, PlayState};
use re_viewer::{CommandSender, PanelStateOverrides, SystemCommand, SystemCommandSender as _};
use re_viewer_context::TimeControlCommand;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SessionMode {
    Live,
    Paused,
    Replay,
}

impl SessionMode {
    fn badge(self) -> &'static str {
        match self {
            Self::Live => "LIVE",
            Self::Paused => "PAUSED",
            Self::Replay => "REPLAY",
        }
    }

    fn api_value(self) -> &'static str {
        match self {
            Self::Live => "live",
            Self::Paused => "paused",
            Self::Replay => "replay",
        }
    }
}

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
struct DeviceSummary {
    id: &'static str,
    name: &'static str,
    status: &'static str,
    battery: Option<u8>,
}

const DEVICES: [DeviceSummary; 3] = [
    DeviceSummary {
        id: "robot-07",
        name: "물류 로봇 07",
        status: "정상",
        battery: Some(82),
    },
    DeviceSummary {
        id: "robot-12",
        name: "물류 로봇 12",
        status: "주의",
        battery: Some(46),
    },
    DeviceSummary {
        id: "drone-03",
        name: "점검 드론 03",
        status: "정상",
        battery: Some(71),
    },
];

#[derive(Clone, Copy)]
struct TopicSummary {
    label: &'static str,
    path: &'static str,
    value: &'static str,
}

const OPERATION_TOPICS: [TopicSummary; 5] = [
    TopicSummary {
        label: "위치",
        path: "/localization/pose",
        value: "정상",
    },
    TopicSummary {
        label: "전방 카메라",
        path: "/camera/front/image",
        value: "30 FPS",
    },
    TopicSummary {
        label: "속도",
        path: "/vehicle/velocity",
        value: "1.2 m/s",
    },
    TopicSummary {
        label: "배터리",
        path: "/system/battery",
        value: "82%",
    },
    TopicSummary {
        label: "경로 계획",
        path: "/planning/trajectory",
        value: "활성",
    },
];

const CAMERA_TOPICS: [TopicSummary; 3] = [
    TopicSummary {
        label: "전방",
        path: "/camera/front/image",
        value: "30 FPS",
    },
    TopicSummary {
        label: "후방",
        path: "/camera/rear/image",
        value: "30 FPS",
    },
    TopicSummary {
        label: "깊이",
        path: "/camera/depth/image",
        value: "15 FPS",
    },
];

const DIAGNOSTIC_TOPICS: [TopicSummary; 4] = [
    TopicSummary {
        label: "제어기",
        path: "/diagnostics/controller",
        value: "정상",
    },
    TopicSummary {
        label: "네트워크",
        path: "/diagnostics/network",
        value: "24 ms",
    },
    TopicSummary {
        label: "GPU",
        path: "/diagnostics/gpu",
        value: "48%",
    },
    TopicSummary {
        label: "메모리",
        path: "/diagnostics/memory",
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

/// Backend-owned source kind used to decide whether physical control is possible.
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

/// Authenticated workspace and source identity supplied by the RMS backend adapter.
#[derive(Clone, Debug, Deserialize)]
pub struct RmsViewerContext {
    pub project_name: String,
    pub device_id: String,
    pub device_name: String,
    pub device_status: String,
    pub device_health: String,
    pub device_state_version: u32,
    pub source_id: String,
    pub source_name: String,
    pub source_kind: RmsSourceKind,
    pub source_url: String,
    pub operator_id: String,
    pub topics: Vec<RmsTopicContext>,
}

/// Product intent emitted to the RMS transport adapter.
#[derive(Clone, Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RmsProductEvent {
    SelectDevice {
        device_id: String,
    },
    SelectSourceKind {
        device_id: String,
        source_kind: String,
    },
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

/// Result returned by the RMS transport adapter after a control request.
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

/// Callback boundary between the product runtime and its HTTP/SSE transport adapter.
pub type RmsProductEventSink = Rc<dyn Fn(RmsProductEvent)>;

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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum UiAction {
    SetMode(SessionMode),
    ToggleLease,
    RequestCommand(ControlCommand),
    ConfirmCommand(ControlCommand),
}

/// Product-owned RMS application that renders the operational shell and Rerun viewport together.
pub struct RmsProductApp {
    rerun_app: re_viewer::App,
    egui_ctx: egui::Context,
    command_sender: CommandSender,
    selected_device: usize,
    session_mode: SessionMode,
    preset: ViewerPreset,
    viewer_context: Option<RmsViewerContext>,
    control_lease: Option<ControlLease>,
    pending_control: Option<PendingControlRequest>,
    next_request_number: u64,
    pending_play_state: Option<PlayState>,
    waiting_for_source_change_from: Option<String>,
    event_sink: Option<RmsProductEventSink>,
    pending_confirmation: Option<ControlCommand>,
    status_message: Option<String>,
}

impl RmsProductApp {
    /// Creates a product app and opens the initial Rerun source.
    pub fn new(
        main_thread_token: re_viewer::MainThreadToken,
        creation_context: &eframe::CreationContext<'_>,
        source_url: Option<&str>,
        event_sink: Option<RmsProductEventSink>,
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
            selected_device: 0,
            session_mode: SessionMode::Replay,
            preset: ViewerPreset::Operations,
            viewer_context: None,
            control_lease: None,
            pending_control: None,
            next_request_number: 0,
            pending_play_state: source_url.map(|_| PlayState::Paused),
            waiting_for_source_change_from: None,
            event_sink,
            pending_confirmation: None,
            status_message: None,
        })
    }

    fn selected_device(&self) -> DeviceSummary {
        DEVICES[self.selected_device]
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

    fn control_enabled(&self) -> bool {
        let Some(context) = self.matched_viewer_context() else {
            return false;
        };
        let Some(lease) = self.control_lease.as_ref() else {
            return false;
        };

        control_is_allowed(
            self.session_mode,
            self.viewer_is_live_and_ready() && self.pending_control.is_none(),
            context,
            lease,
            current_time_ms(),
        )
    }

    fn lease_request_allowed(&self) -> bool {
        let Some(context) = self.matched_viewer_context() else {
            return false;
        };

        self.session_mode == SessionMode::Live
            && self.viewer_is_live_and_ready()
            && self.pending_control.is_none()
            && self.control_lease.is_none()
            && context.device_status == "online"
            && !matches!(context.device_health.as_str(), "restricted" | "critical")
            && context.device_state_version > 0
            && !context.operator_id.is_empty()
    }

    fn matched_viewer_context(&self) -> Option<&RmsViewerContext> {
        let context = self.viewer_context.as_ref()?;
        (context.device_id == self.selected_device().id).then_some(context)
    }

    fn viewer_is_live_and_ready(&self) -> bool {
        self.matched_viewer_context().is_some_and(|context| {
            context.source_kind == RmsSourceKind::Live
                && !context.source_id.is_empty()
                && !context.source_url.is_empty()
                && self.rerun_app.active_recording_id().is_some()
                && self.rerun_app.active_play_state() == Some(PlayState::Following)
        })
    }

    fn next_request_id(&mut self) -> String {
        self.next_request_number = self.next_request_number.wrapping_add(1);
        format!("rms-runtime-{}", self.next_request_number)
    }

    fn release_lease(&mut self, lease: ControlLease, message: &str) {
        let request_id = self.next_request_id();
        let event = RmsProductEvent::ReleaseControlLease {
            request_id: request_id.clone(),
            device_id: lease.device_id.clone(),
            lease_id: lease.id.clone(),
            lease_epoch: lease.epoch,
        };
        if let Some(event_sink) = self.event_sink.clone() {
            self.pending_control = Some(PendingControlRequest {
                id: request_id,
                device_id: lease.device_id,
                kind: PendingControlKind::ReleaseLease,
                lease_id: Some(lease.id),
                lease_epoch: Some(lease.epoch),
            });
            event_sink(event);
            self.status_message = Some(message.to_owned());
        } else {
            self.pending_control = None;
            self.status_message = Some("제어 백엔드가 연결되지 않았습니다.".to_owned());
        }
    }

    fn release_stale_lease(&mut self, lease: ControlLease) {
        let request_id = self.next_request_id();
        if let Some(event_sink) = self.event_sink.clone() {
            event_sink(RmsProductEvent::ReleaseControlLease {
                request_id,
                device_id: lease.device_id,
                lease_id: lease.id,
                lease_epoch: lease.epoch,
            });
        }
    }

    /// Applies the authenticated project, device, source, and topic context from the RMS backend.
    pub fn apply_viewer_context(&mut self, context: RmsViewerContext) {
        if context.device_id != self.selected_device().id {
            return;
        }

        let source_changed = self
            .viewer_context
            .as_ref()
            .is_none_or(|current| current.source_id != context.source_id);
        let source_url_changed = self
            .viewer_context
            .as_ref()
            .is_none_or(|current| current.source_url != context.source_url);
        if source_changed && let Some(lease) = self.control_lease.take() {
            self.release_lease(lease, "데이터 변경으로 제어권을 반납하고 있습니다.");
        }
        if source_url_changed {
            self.waiting_for_source_change_from = self
                .rerun_app
                .active_recording_id()
                .map(ToString::to_string);
            self.rerun_app.open_url_or_file(&context.source_url);
        } else {
            self.waiting_for_source_change_from = None;
        }

        self.session_mode = match context.source_kind {
            RmsSourceKind::Live => SessionMode::Live,
            RmsSourceKind::Recording => SessionMode::Replay,
        };
        self.pending_play_state = Some(match context.source_kind {
            RmsSourceKind::Live => PlayState::Following,
            RmsSourceKind::Recording => PlayState::Paused,
        });
        self.viewer_context = Some(context);
        self.status_message = None;
        self.egui_ctx.request_repaint();
    }

    /// Fails closed when the RMS backend cannot resolve the selected workspace source.
    pub fn apply_viewer_error(&mut self, message: String) {
        if let Some(lease) = self.control_lease.take() {
            self.release_lease(lease, "제어권을 안전하게 반납하고 있습니다.");
        }
        self.viewer_context = None;
        self.pending_play_state = None;
        self.waiting_for_source_change_from = None;
        self.pending_confirmation = None;
        self.session_mode = SessionMode::Replay;
        self.status_message = Some(message);
        self.egui_ctx.request_repaint();
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

    fn request_source_kind(&mut self, source_kind: RmsSourceKind) {
        if let Some(lease) = self.control_lease.take() {
            self.release_lease(lease, "제어권을 안전하게 반납하고 있습니다.");
        }
        self.viewer_context = None;
        self.pending_play_state = None;
        self.waiting_for_source_change_from = None;
        self.session_mode = match source_kind {
            RmsSourceKind::Live => SessionMode::Paused,
            RmsSourceKind::Recording => SessionMode::Replay,
        };
        if let Some(event_sink) = self.event_sink.clone() {
            event_sink(RmsProductEvent::SelectSourceKind {
                device_id: self.selected_device().id.to_owned(),
                source_kind: match source_kind {
                    RmsSourceKind::Live => "live",
                    RmsSourceKind::Recording => "recording",
                }
                .to_owned(),
            });
            self.status_message = Some("데이터를 여는 중입니다.".to_owned());
        } else {
            self.status_message = Some("데이터 백엔드가 연결되지 않았습니다.".to_owned());
        }
    }

    fn select_device(&mut self, index: usize) {
        if index == self.selected_device {
            return;
        }
        if let Some(lease) = self.control_lease.take() {
            self.release_lease(lease, "제어권을 안전하게 반납하고 있습니다.");
        }
        self.selected_device = index;
        self.viewer_context = None;
        self.pending_play_state = None;
        self.waiting_for_source_change_from = None;
        self.pending_confirmation = None;
        self.session_mode = SessionMode::Replay;
        if let Some(event_sink) = self.event_sink.clone() {
            event_sink(RmsProductEvent::SelectDevice {
                device_id: self.selected_device().id.to_owned(),
            });
            self.status_message = Some("장비 데이터를 여는 중입니다.".to_owned());
        } else {
            self.status_message = Some("데이터 백엔드가 연결되지 않았습니다.".to_owned());
        }
    }

    fn apply_action(&mut self, action: UiAction) {
        match action {
            UiAction::SetMode(mode) => {
                if mode == SessionMode::Live
                    && self
                        .matched_viewer_context()
                        .is_some_and(|context| context.source_kind == RmsSourceKind::Recording)
                {
                    self.request_source_kind(RmsSourceKind::Live);
                    return;
                }

                self.session_mode = mode;
                let play_state = match mode {
                    SessionMode::Live => PlayState::Following,
                    SessionMode::Paused | SessionMode::Replay => PlayState::Paused,
                };
                self.pending_play_state = Some(play_state);
                self.send_time_command(TimeControlCommand::SetPlayState(play_state));
                if mode != SessionMode::Live {
                    if let Some(lease) = self.control_lease.take() {
                        self.release_lease(lease, "제어권을 안전하게 반납하고 있습니다.");
                    } else if self.pending_control.is_none() {
                        self.status_message = None;
                    }
                } else {
                    self.status_message = None;
                }
            }
            UiAction::ToggleLease => {
                if self.pending_control.is_some() {
                    return;
                }
                if let Some(lease) = self.control_lease.take() {
                    self.release_lease(lease, "제어권을 반납하고 있습니다.");
                    return;
                }
                if !self.lease_request_allowed() {
                    self.status_message =
                        Some("LIVE 데이터와 장비 상태를 확인해 주세요.".to_owned());
                    return;
                }
                let Some(context) = self.matched_viewer_context().cloned() else {
                    return;
                };
                let request_id = self.next_request_id();
                let event = RmsProductEvent::RequestControlLease {
                    request_id: request_id.clone(),
                    device_id: context.device_id.clone(),
                    expected_device_version: context.device_state_version,
                };
                if let Some(event_sink) = self.event_sink.clone() {
                    self.pending_control = Some(PendingControlRequest {
                        id: request_id,
                        device_id: context.device_id,
                        kind: PendingControlKind::RequestLease,
                        lease_id: None,
                        lease_epoch: None,
                    });
                    event_sink(event);
                    self.status_message = Some("제어권을 요청하고 있습니다.".to_owned());
                } else {
                    self.status_message = Some("제어 백엔드가 연결되지 않았습니다.".to_owned());
                }
            }
            UiAction::RequestCommand(command) => {
                self.pending_confirmation = Some(command);
            }
            UiAction::ConfirmCommand(command) => {
                self.pending_confirmation = None;
                if !self.control_enabled() {
                    self.status_message =
                        Some("LIVE 상태와 제어권을 다시 확인해 주세요.".to_owned());
                    return;
                }
                let Some(context) = self.matched_viewer_context().cloned() else {
                    return;
                };
                let Some(lease) = self.control_lease.clone() else {
                    self.status_message = Some("제어권을 다시 확인해 주세요.".to_owned());
                    return;
                };
                let request_id = self.next_request_id();
                let event = RmsProductEvent::SendControlCommand {
                    request_id: request_id.clone(),
                    device_id: context.device_id.clone(),
                    command_type: command.api_value().to_owned(),
                    expected_device_version: context.device_state_version,
                    lease_id: lease.id.clone(),
                    lease_epoch: lease.epoch,
                    session_mode: self.session_mode.api_value().to_owned(),
                };
                if let Some(event_sink) = self.event_sink.clone() {
                    self.pending_control = Some(PendingControlRequest {
                        id: request_id,
                        device_id: context.device_id,
                        kind: PendingControlKind::Command,
                        lease_id: Some(lease.id),
                        lease_epoch: Some(lease.epoch),
                    });
                    event_sink(event);
                    self.status_message = Some(format!("{} 요청 중", command.label()));
                } else {
                    self.status_message = Some("제어 백엔드가 연결되지 않았습니다.".to_owned());
                }
            }
        }
    }

    /// Applies an authenticated control result returned by the RMS backend adapter.
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
                let pending_matches = self.pending_control.as_ref().is_some_and(|pending| {
                    pending.id == request_id
                        && pending.device_id == device_id
                        && pending.kind == PendingControlKind::RequestLease
                });
                let context_matches = self.matched_viewer_context().is_some_and(|context| {
                    context.device_id == device_id
                        && context.operator_id == lease.holder_id
                        && self.session_mode == SessionMode::Live
                        && self.viewer_is_live_and_ready()
                        && lease.expires_at_ms > current_time_ms()
                });
                if pending_matches && context_matches {
                    self.pending_control = None;
                    self.control_lease = Some(lease);
                    self.status_message = Some(format!("{holder_name} 제어권 확보"));
                } else {
                    if pending_matches {
                        self.pending_control = None;
                    }
                    self.release_stale_lease(lease);
                    self.status_message =
                        Some("현재 화면과 다른 제어권을 안전하게 반납했습니다.".to_owned());
                }
            }
            RmsControlResponse::LeaseReleased {
                request_id,
                device_id,
                lease_id,
                lease_epoch,
            } => {
                let pending_matches = self.pending_control.as_ref().is_some_and(|pending| {
                    pending.id == request_id
                        && pending.device_id == device_id
                        && pending.kind == PendingControlKind::ReleaseLease
                        && pending.lease_id.as_deref() == Some(lease_id.as_str())
                        && pending.lease_epoch == Some(lease_epoch)
                });
                if pending_matches {
                    self.pending_control = None;
                    self.control_lease = None;
                    self.status_message = Some("제어권을 반납했습니다.".to_owned());
                }
            }
            RmsControlResponse::CommandUpdated {
                request_id,
                device_id,
                message,
            } => {
                let pending_matches = self.pending_control.as_ref().is_some_and(|pending| {
                    pending.id == request_id
                        && pending.device_id == device_id
                        && pending.kind == PendingControlKind::Command
                });
                if pending_matches {
                    self.pending_control = None;
                    self.status_message = Some(message);
                }
            }
            RmsControlResponse::Failed {
                request_id,
                device_id,
                message,
            } => {
                let pending_matches = self.pending_control.as_ref().is_some_and(|pending| {
                    pending.id == request_id && pending.device_id == device_id
                });
                if pending_matches {
                    self.pending_control = None;
                    self.control_lease = None;
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
            if self.pending_control.is_none() {
                self.status_message = None;
            }
        }

        let Some(play_state) = self.rerun_app.active_play_state() else {
            return;
        };
        if self
            .matched_viewer_context()
            .is_some_and(|context| context.source_kind == RmsSourceKind::Recording)
        {
            if play_state != PlayState::Paused {
                self.session_mode = SessionMode::Replay;
                self.pending_play_state = Some(PlayState::Paused);
                self.send_time_command(TimeControlCommand::Pause);
            }
            return;
        }

        let actual_mode = match play_state {
            PlayState::Following => SessionMode::Live,
            PlayState::Playing => SessionMode::Replay,
            PlayState::Paused if self.session_mode == SessionMode::Replay => SessionMode::Replay,
            PlayState::Paused => SessionMode::Paused,
        };
        if actual_mode != self.session_mode {
            self.session_mode = actual_mode;
            if actual_mode != SessionMode::Live
                && let Some(lease) = self.control_lease.take()
            {
                self.release_lease(lease, "재생 상태가 변경되어 제어권을 반납합니다.");
            }
        }
    }

    fn expire_control_lease(&mut self) {
        let expired = self
            .control_lease
            .as_ref()
            .is_some_and(|lease| lease.expires_at_ms <= current_time_ms());
        if expired && let Some(lease) = self.control_lease.take() {
            self.release_lease(lease, "제어권이 만료되었습니다.");
        }
    }

    fn top_bar(&self, ui: &mut egui::Ui) {
        let device = self.selected_device();
        let context = self.matched_viewer_context();
        let project_name = context.map_or("프로젝트", |context| context.project_name.as_str());
        let device_name = context.map_or(device.name, |context| context.device_name.as_str());
        let device_status = context.map_or(device.status, |context| {
            device_health_label(&context.device_status, &context.device_health)
        });
        ui.horizontal(|ui| {
            ui.add_space(8.0);
            ui.strong("RMS");
            ui.separator();
            ui.label(project_name);
            ui.label("/");
            ui.strong(device_name);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.add_space(8.0);
                ui.strong(self.session_mode.badge());
                ui.label(device_status);
            });
        });
    }

    fn context_panel(&mut self, ui: &mut egui::Ui) {
        ui.heading("프로젝트");
        ui.label(
            self.matched_viewer_context()
                .map_or("연결 중", |context| context.project_name.as_str()),
        );
        ui.add_space(12.0);
        ui.strong("디바이스");
        ui.add_space(4.0);

        let mut selected_index = None;
        for (index, device) in DEVICES.iter().enumerate() {
            let label = match device.battery {
                Some(battery) => format!("{}  ·  {battery}%", device.name),
                None => device.name.to_owned(),
            };
            if ui
                .selectable_label(index == self.selected_device, label)
                .clicked()
            {
                selected_index = Some(index);
            }
        }
        if let Some(index) = selected_index {
            self.select_device(index);
        }

        ui.add_space(14.0);
        ui.strong("데이터");
        ui.add_space(4.0);
        let active_source_kind = self
            .matched_viewer_context()
            .map(|context| context.source_kind);
        if ui
            .selectable_label(active_source_kind == Some(RmsSourceKind::Live), "실시간")
            .clicked()
        {
            if active_source_kind == Some(RmsSourceKind::Live) {
                self.apply_action(UiAction::SetMode(SessionMode::Live));
            } else {
                self.request_source_kind(RmsSourceKind::Live);
            }
        }
        if ui
            .selectable_label(
                active_source_kind == Some(RmsSourceKind::Recording),
                "최근 기록",
            )
            .clicked()
        {
            if active_source_kind == Some(RmsSourceKind::Recording) {
                self.apply_action(UiAction::SetMode(SessionMode::Replay));
            } else {
                self.request_source_kind(RmsSourceKind::Recording);
            }
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
                .topics
                .iter()
                .filter(|topic| self.topic_matches_preset(topic))
            {
                ui.horizontal(|ui| {
                    ui.label(&topic.label);
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.weak(topic.value.as_deref().unwrap_or("—"));
                    });
                });
                ui.small(&topic.path);
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
                ui.small(topic.path);
                ui.add_space(6.0);
            }
        }
    }

    fn control_bar(&self, ui: &mut egui::Ui) -> Option<UiAction> {
        let mut action = None;
        ui.horizontal(|ui| {
            ui.add_space(8.0);
            if self.session_mode == SessionMode::Live {
                if ui.button("일시정지").clicked() {
                    action = Some(UiAction::SetMode(SessionMode::Paused));
                }
            } else if ui.button("LIVE 복귀").clicked() {
                action = Some(UiAction::SetMode(SessionMode::Live));
            }

            ui.separator();
            let lease_label = if self.pending_control.is_some() {
                "처리 중"
            } else if self.control_lease.is_some() {
                "제어권 반납"
            } else {
                "제어권 요청"
            };
            let lease_toggle_enabled = if self.control_lease.is_some() {
                self.pending_control.is_none()
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
                if self.session_mode != SessionMode::Live {
                    ui.weak("Replay 제어 차단");
                } else if let Some(message) = &self.status_message {
                    ui.label(message);
                } else if !control_enabled {
                    ui.weak("제어권 필요");
                }
            });
        });
        action
    }

    fn confirmation_window(&mut self, ctx: &egui::Context) -> Option<UiAction> {
        let command = self.pending_confirmation?;
        let mut action = None;
        let mut open = true;
        egui::Window::new("명령 확인")
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .collapsible(false)
            .resizable(false)
            .open(&mut open)
            .show(ctx, |ui| {
                ui.strong(command.label());
                ui.label(format!("대상: {}", self.selected_device().name));
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button("취소").clicked() {
                        self.pending_confirmation = None;
                    }
                    if ui.button("실행").clicked() {
                        action = Some(UiAction::ConfirmCommand(command));
                    }
                });
            });
        if !open {
            self.pending_confirmation = None;
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
    session_mode: SessionMode,
    runtime_is_ready: bool,
    context: &RmsViewerContext,
    lease: &ControlLease,
    now_ms: f64,
) -> bool {
    session_mode == SessionMode::Live
        && runtime_is_ready
        && context.source_kind == RmsSourceKind::Live
        && context.device_status == "online"
        && !matches!(context.device_health.as_str(), "restricted" | "critical")
        && !context.device_id.is_empty()
        && context.device_state_version > 0
        && lease.device_id == context.device_id
        && lease.holder_id == context.operator_id
        && lease.expires_at_ms > now_ms
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
        egui::Panel::bottom("rms_control_bar")
            .exact_size(48.0)
            .show(ui, |ui| action = self.control_bar(ui));

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
    use super::{ControlLease, RmsSourceKind, RmsViewerContext, SessionMode, control_is_allowed};

    fn viewer_context() -> RmsViewerContext {
        RmsViewerContext {
            project_name: "Test".to_owned(),
            device_id: "robot-07".to_owned(),
            device_name: "Robot 07".to_owned(),
            device_status: "online".to_owned(),
            device_health: "normal".to_owned(),
            device_state_version: 142,
            source_id: "robot-07-live".to_owned(),
            source_name: "Live".to_owned(),
            source_kind: RmsSourceKind::Live,
            source_url: "https://example.invalid/live.rrd".to_owned(),
            operator_id: "operator-01".to_owned(),
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
    fn replay_and_paused_sessions_never_allow_control() {
        let context = viewer_context();
        let lease = lease();
        assert!(!control_is_allowed(
            SessionMode::Replay,
            true,
            &context,
            &lease,
            1_000.0,
        ));
        assert!(!control_is_allowed(
            SessionMode::Paused,
            true,
            &context,
            &lease,
            1_000.0,
        ));
    }

    #[test]
    fn live_control_requires_ready_viewer_and_current_lease() {
        let context = viewer_context();
        let lease = lease();
        assert!(control_is_allowed(
            SessionMode::Live,
            true,
            &context,
            &lease,
            1_000.0,
        ));
        assert!(!control_is_allowed(
            SessionMode::Live,
            false,
            &context,
            &lease,
            1_000.0,
        ));
        assert!(!control_is_allowed(
            SessionMode::Live,
            true,
            &context,
            &lease,
            3_000.0,
        ));
    }
}
