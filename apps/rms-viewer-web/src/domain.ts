export type ProjectStatus = "active" | "standby";
export type DeviceStatus = "online" | "degraded" | "offline";
export type DeviceHealth = "normal" | "attention" | "restricted" | "critical";
export type DeviceKind = "robot" | "drone" | "vehicle";
export type DataSourceKind = "live" | "recording";
export type SessionMode = "live" | "paused" | "replay";
export type TopicRenderer = "spatial" | "camera" | "timeseries" | "state" | "log";
export type TopicQuality = "fresh" | "delayed" | "unavailable";
export type CommandRisk = "low" | "medium" | "high" | "emergency";

export interface Project {
  id: string;
  name: string;
  description: string;
  status: ProjectStatus;
  deviceCount: number;
  onlineDeviceCount: number;
}

export interface Device {
  id: string;
  projectId: string;
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

export interface DataSource {
  id: string;
  projectId: string;
  deviceId: string;
  name: string;
  kind: DataSourceKind;
  status: "ready" | "recording" | "processing";
  rrdUrl: string;
  capturedAt: string;
  durationLabel?: string;
  topicIds: string[];
}

export interface Topic {
  id: string;
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

export interface ControlLease {
  id: string;
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

export interface ControlCommandRequest {
  deviceId: string;
  commandType: string;
  expectedDeviceVersion: number;
  idempotencyKey: string;
  leaseId: string;
  leaseEpoch: number;
  sessionMode: SessionMode;
  issuedAt: string;
  expiresAt: string;
}

export interface CommandReceipt {
  commandId: string;
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

export interface ControlEligibilityInput {
  device: Device;
  source: DataSource;
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

export function sourceMode(source: DataSource): SessionMode {
  return source.kind === "live" ? "live" : "replay";
}
