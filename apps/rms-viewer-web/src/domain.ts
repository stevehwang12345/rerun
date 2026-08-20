export type ProjectStatus = "active" | "standby" | "archived";
export type IntegrationKind = "ros2" | "mcap" | "rtsp" | "mavlink" | "autoware" | "rerun";
export type IntegrationStatus = "connected" | "degraded" | "disconnected" | "testing";
export type DeviceStatus = "online" | "degraded" | "offline";
export type DeviceHealth = "normal" | "attention" | "restricted" | "critical";
export type DeviceKind = "robot" | "drone" | "vehicle";
export type DataSourceStatus = "ready" | "recording" | "degraded" | "offline";
export type DataSourceKind = "live" | "recording";
export type SessionMode = "live" | "paused" | "replay";
export type TopicRenderer = "spatial" | "camera" | "timeseries" | "state" | "log";
export type TopicQuality = "fresh" | "delayed" | "unavailable";
export type CommandRisk = "low" | "medium" | "high" | "emergency";

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
  cursorSeconds: number;
  openedAt: string;
  closedAt?: string;
  resourceVersion: number;
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
  if (device.health === "critical" || device.health === "restricted") {
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
