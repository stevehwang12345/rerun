export type ProjectStatus = "active" | "standby" | "archived";
export type IntegrationKind = "ros2" | "mcap" | "rtsp" | "mavlink" | "autoware" | "rerun" | "rms_edge";
export type IntegrationStatus = "connected" | "degraded" | "disconnected" | "testing";
export type DeviceStatus = "online" | "degraded" | "offline";
export type DeviceHealth = "unknown" | "normal" | "attention" | "restricted" | "critical";
export type DeviceKind = "robot" | "drone" | "vehicle" | "camera" | "gateway";
export type DataSourceStatus = "pending" | "ready" | "recording" | "degraded" | "offline";
export type DataSourceKind = "live" | "recording";
export type SessionMode = "live" | "paused" | "replay";
export type TopicRenderer =
  | "spatial"
  | "spatial2d"
  | "spatial3d"
  | "transform3d"
  | "map"
  | "camera"
  | "timeseries"
  | "state"
  | "log"
  | "raw";
export type TopicQuality = "fresh" | "delayed" | "unavailable";
export type CommandRisk = "low" | "medium" | "high" | "emergency";
export type TimelineKind = "sequence" | "timestamp" | "duration";
export type ReplayPlayState = "paused" | "playing";
export type ReplayLoopMode = "off" | "all" | "selection";
export type RecordingImportFormat = "rrd" | "mcap" | "ros2-bag-zip" | "csv" | "video";
export type RecordingImportStatus =
  | "uploading"
  | "processing"
  | "ready"
  | "failed"
  | "cancelled";
export type DiscoverySessionStatus =
  | "searching"
  | "ready"
  | "cancelled"
  | "failed"
  | "expired";
export type DiscoveryCandidateStatus =
  | "found"
  | "verifying"
  | "verified"
  | "needs_attention"
  | "unavailable"
  | "already_linked";
export type DiscoveryCandidateCategory =
  | "robot"
  | "drone"
  | "vehicle"
  | "camera"
  | "gateway";
export type DiscoverySourceCategory =
  | "camera"
  | "spatial"
  | "telemetry"
  | "state"
  | "log";
export type DiscoverySourceStatus = "ready" | "unavailable";

export interface Integration {
  id: string;
  organizationId: string;
  name: string;
  kind: IntegrationKind;
  status: IntegrationStatus;
  endpointLabel: string;
  lastHealthAt: string;
  createdAt: string;
  resourceVersion: number;
}

export interface Device {
  id: string;
  organizationId: string;
  integrationId: string;
  name: string;
  kind: DeviceKind;
  status: DeviceStatus;
  health: DeviceHealth;
  operationMode: string;
  batteryPercent?: number;
  taskName: string;
  taskProgress: number;
  lastSeenAt: string;
  stateVersion: number;
}

/** A physical input registered by RMS Connect. Project ownership lives in DataAssignment. */
export interface DataSource {
  id: string;
  integrationId: string;
  deviceId: string;
  name: string;
  protocol: string;
  status: DataSourceStatus;
  liveUrl: string;
  topicIds: string[];
  mappingVersion: number;
  lastDataAt: string;
}

export interface Project {
  id: string;
  organizationId: string;
  name: string;
  description: string;
  status: ProjectStatus;
  deviceCount: number;
  onlineDeviceCount: number;
  createdAt: string;
  resourceVersion: number;
}

export interface DeviceAssignment {
  id: string;
  projectId: string;
  deviceId: string;
  accessMode: "control" | "observe";
  validFrom: string;
  validTo?: string;
  resourceVersion: number;
}

export interface DataAssignment {
  id: string;
  projectId: string;
  dataSourceId: string;
  visibility: "operator" | "analyst" | "restricted";
  validFrom: string;
  validTo?: string;
  resourceVersion: number;
}

export interface Topic {
  id: string;
  dataSourceId: string;
  deviceId: string;
  path: string;
  label: string;
  renderer: TopicRenderer;
  quality: TopicQuality;
  value?: string;
  unit?: string;
  message?: string;
  samples?: number[];
  updatedAt: string;
}

export interface LiveSession {
  id: string;
  projectId: string;
  deviceId: string;
  dataSourceId: string;
  openedBy: string;
  status: "open" | "closed";
  playState: "following" | "paused";
  sourceHealth: TopicQuality;
  streamUrl: string;
  startedAt: string;
  closedAt?: string;
  resourceVersion: number;
}

export interface RecordingProjectSnapshot {
  projectId: string;
  projectName: string;
  capturedAt: string;
  deviceAssignmentId: string;
  dataAssignmentId: string;
}

/** Lossless RRD timeline metadata. start/end remain decimal strings across the JSON boundary. */
export interface TimelineDescriptor {
  name: string;
  kind: TimelineKind;
  start: string;
  end: string;
  durationSeconds: number | null;
  fps: number | null;
}

export interface TimelineCursor {
  kind: TimelineKind;
  value: string;
}

export interface ReplayLoop {
  mode: ReplayLoopMode;
  start?: TimelineCursor;
  end?: TimelineCursor;
}

export interface Recording {
  id: string;
  organizationId: string;
  projectId: string;
  deviceId: string;
  dataSourceId: string;
  name: string;
  status: "finalizing" | "ready" | "failed";
  rrdUrl: string;
  capturedAt: string;
  durationLabel: string;
  timelines: TimelineDescriptor[];
  defaultTimeline: string;
  durationSeconds: number;
  rrdVersion: string;
  footerVerified: boolean;
  contentSha256: string;
  topicIds: string[];
  mappingVersion: number;
  projectSnapshot: RecordingProjectSnapshot;
  resourceVersion: number;
}

export interface ReplaySession {
  id: string;
  projectId: string;
  recordingId: string;
  deviceId: string;
  openedBy: string;
  status: "open" | "closed";
  streamUrl: string;
  /** @deprecated Use initialCursor with initialTimeline. */
  cursorSeconds: number;
  initialTimeline: string;
  initialCursor: TimelineCursor;
  initialPlayState: ReplayPlayState;
  initialSpeed: number;
  initialLoop: ReplayLoop;
  openedAt: string;
  closedAt?: string;
  resourceVersion: number;
}

export interface RecordingImport {
  id: string;
  projectId: string;
  deviceId: string;
  dataSourceId: string;
  fileName: string;
  format: RecordingImportFormat;
  status: RecordingImportStatus;
  progressPercent: number;
  sizeBytes: number;
  sourceSha256?: string;
  failureReason?: string;
  artifactUrl?: string;
  recordingId?: string;
  createdAt: string;
  updatedAt: string;
  resourceVersion: number;
}

/** A short-lived, user-initiated search. Discovered endpoints remain server-private. */
export interface NetworkDiscoverySession {
  id: string;
  status: DiscoverySessionStatus;
  candidateCount: number;
  startedAt: string;
  expiresAt: string;
  resourceVersion: number;
}

/** A sanitized candidate summary. It intentionally contains no address or protocol details. */
export interface DiscoveryCandidate {
  id: string;
  sessionId: string;
  displayName: string;
  category: DiscoveryCandidateCategory;
  status: DiscoveryCandidateStatus;
  lastSeenAt: string;
  sourceCount: number;
  supportsLive: boolean;
}

export interface NetworkDiscoverySnapshot {
  session: NetworkDiscoverySession;
  candidates: DiscoveryCandidate[];
}

export interface CandidateVerification {
  verificationToken: string;
  candidateId: string;
  status: "verified" | "needs_credentials" | "incompatible" | "unavailable";
  suggestedDevice: {
    name: string;
    kind: DeviceKind;
  };
  sources: Array<{
    id: string;
    label: string;
    category: DiscoverySourceCategory;
    status: DiscoverySourceStatus;
  }>;
  expiresAt: string;
}

export interface ApproveNetworkCandidateInput {
  verificationToken: string;
  projectId: string;
  expectedWorkspaceVersion: number;
  deviceName: string;
  selectedSourceIds: string[];
  accessMode: "observe";
  visibility: "operator";
}

export interface NetworkLinkReceipt {
  status: "linked";
  projectId: string;
  integrationId: string;
  deviceId: string;
  dataSourceIds: string[];
  workspaceVersion: number;
}

/** A single Project Service revision, used instead of stitching mutable list responses together. */
export interface WorkspaceSnapshot {
  snapshotVersion: number;
  capturedAt: string;
  project: Project;
  deviceAssignments: DeviceAssignment[];
  dataAssignments: DataAssignment[];
  devices: Device[];
  dataSources: DataSource[];
  recordings: Recording[];
  /** Immutable Topic descriptors captured with each Recording. Replay must never use live/source Topics. */
  topicsByRecording: Record<string, Topic[]>;
  topicsByDataSource: Record<string, Topic[]>;
}

export type ViewerSourceRef =
  | { kind: "live"; liveSessionId: string; dataSourceId: string }
  | { kind: "recording"; replaySessionId: string; recordingId: string };

/** Compatibility projection for the current one-canvas host while routes migrate to sessions. */
export interface ViewerDataSource {
  id: string;
  deviceId: string;
  name: string;
  kind: DataSourceKind;
  status: "ready" | "recording" | "processing";
  rrdUrl: string;
  capturedAt: string;
  durationLabel?: string;
  topicIds: string[];
}

export interface ControlLease {
  id: string;
  liveSessionId: string;
  deviceId: string;
  holderId: string;
  holderName: string;
  expiresAt: string;
  epoch: number;
}

export interface ControlCommandDefinition {
  type: string;
  label: string;
  risk: CommandRisk;
  description: string;
}

/** The Control Service contract always carries its authorized Live Session. */
export interface LiveControlCommandRequest {
  liveSessionId: string;
  deviceId: string;
  commandType: string;
  expectedDeviceVersion: number;
  idempotencyKey: string;
  leaseId: string;
  leaseEpoch: number;
  sessionMode: "live";
  issuedAt: string;
  expiresAt: string;
}

export interface CommandReceipt {
  commandId: string;
  liveSessionId: string;
  commandType: string;
  status: "accepted" | "executing" | "succeeded" | "rejected";
  message: string;
  createdAt: string;
}

interface RmsEventBase {
  eventId: string;
  occurredAt: string;
  projectId: string;
  deviceId: string;
  liveSessionId: string;
  resourceVersion: number;
}

export type RmsEvent =
  | (RmsEventBase & {
      type: "topic.value.changed";
      data: Pick<Topic, "id" | "value" | "samples" | "quality" | "updatedAt">;
    })
  | (RmsEventBase & {
      type: "device.state.changed";
      data: Partial<Device> & Pick<Device, "id">;
    })
  | (RmsEventBase & {
      type: "control.lease.changed";
      data: { lease: ControlLease | null };
    })
  | (RmsEventBase & {
      type: "command.state.changed";
      data: CommandReceipt;
    });

export type WorkspaceEvent = {
  eventId: string;
  type: "project.workspace.changed";
  occurredAt: string;
  projectId: string;
  snapshotVersion: number;
};

export interface ControlEligibilityInput {
  device: Device;
  source: Pick<ViewerDataSource, "kind">;
  mode: SessionMode;
  lease: ControlLease | null;
  operatorId: string;
  viewerReady: boolean;
}

export interface ControlEligibility {
  allowed: boolean;
  reason: string;
}

export const OPERATOR_ID = "operator-01";

export function deriveControlEligibility({
  device,
  source,
  mode,
  lease,
  operatorId,
  viewerReady,
}: ControlEligibilityInput): ControlEligibility {
  if (source.kind !== "live" || mode === "replay") {
    return { allowed: false, reason: "Replay에서는 제어할 수 없습니다." };
  }
  if (mode === "paused") {
    return { allowed: false, reason: "LIVE 화면이 일시정지되었습니다." };
  }
  if (device.status !== "online") {
    return { allowed: false, reason: "장비 연결을 확인해야 합니다." };
  }
  if (
    device.health === "unknown" ||
    device.health === "critical" ||
    device.health === "restricted"
  ) {
    return { allowed: false, reason: "안전 상태를 먼저 확인해야 합니다." };
  }
  if (!viewerReady) {
    return { allowed: false, reason: "실시간 화면을 준비하고 있습니다." };
  }
  if (!lease || lease.holderId !== operatorId) {
    return { allowed: false, reason: "제어권이 필요합니다." };
  }
  if (Date.parse(lease.expiresAt) <= Date.now()) {
    return { allowed: false, reason: "제어권이 만료되었습니다." };
  }
  return { allowed: true, reason: "제어 가능" };
}

export function healthLabel(health: DeviceHealth): string {
  return {
    unknown: "확인 필요",
    normal: "정상",
    attention: "주의",
    restricted: "제한",
    critical: "위험",
  }[health];
}

export function statusLabel(status: DeviceStatus): string {
  return {
    online: "연결됨",
    degraded: "지연됨",
    offline: "연결 끊김",
  }[status];
}

export function deviceKindLabel(kind: DeviceKind): string {
  return {
    robot: "로봇",
    drone: "드론",
    vehicle: "차량",
    camera: "카메라",
    gateway: "게이트웨이",
  }[kind];
}

export function compactTimestamp(value: string): string {
  const date = new Date(value);
  if (Number.isNaN(date.getTime())) {
    return "시간 미상";
  }
  return new Intl.DateTimeFormat("ko-KR", {
    month: "short",
    day: "numeric",
    hour: "2-digit",
    minute: "2-digit",
    hour12: false,
  }).format(date);
}

export function sourceMode(source: Pick<ViewerDataSource, "kind">): SessionMode {
  return source.kind === "live" ? "live" : "replay";
}
