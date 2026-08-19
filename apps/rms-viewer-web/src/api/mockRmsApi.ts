import {
  OPERATOR_ID,
  type CommandReceipt,
  type ControlCommandRequest,
  type ControlLease,
  type DataSource,
  type Device,
  type Project,
  type RmsEvent,
  type Topic,
} from "../domain";
import type { RmsApi } from "./rmsApi";

const SAMPLE_BASE = "https://app.rerun.io/version/0.36.1/examples";

const projects: Project[] = [
  {
    id: "project-logistics",
    name: "물류 자동화",
    description: "A동 물류 로봇 운영",
    status: "active",
    deviceCount: 3,
    onlineDeviceCount: 2,
  },
  {
    id: "project-inspection",
    name: "시설 점검",
    description: "야외 설비 드론 점검",
    status: "standby",
    deviceCount: 2,
    onlineDeviceCount: 1,
  },
];

const devices: Device[] = [
  {
    id: "robot-07",
    projectId: "project-logistics",
    name: "Robot-07",
    kind: "robot",
    status: "online",
    health: "normal",
    operationMode: "자율 운행",
    batteryPercent: 78,
    taskName: "Bay 3 이동",
    taskProgress: 62,
    lastSeenAt: "2026-08-20T09:32:08+09:00",
    stateVersion: 142,
  },
  {
    id: "robot-12",
    projectId: "project-logistics",
    name: "Robot-12",
    kind: "robot",
    status: "degraded",
    health: "attention",
    operationMode: "대기",
    batteryPercent: 41,
    taskName: "충전 위치 이동",
    taskProgress: 18,
    lastSeenAt: "2026-08-20T09:32:05+09:00",
    stateVersion: 87,
  },
  {
    id: "robot-21",
    projectId: "project-logistics",
    name: "Robot-21",
    kind: "robot",
    status: "offline",
    health: "restricted",
    operationMode: "점검",
    batteryPercent: 0,
    taskName: "정비 중",
    taskProgress: 0,
    lastSeenAt: "2026-08-20T08:51:00+09:00",
    stateVersion: 31,
  },
  {
    id: "drone-03",
    projectId: "project-inspection",
    name: "Drone-03",
    kind: "drone",
    status: "online",
    health: "normal",
    operationMode: "임무 대기",
    batteryPercent: 86,
    taskName: "동측 패널 점검",
    taskProgress: 0,
    lastSeenAt: "2026-08-20T09:31:59+09:00",
    stateVersion: 55,
  },
  {
    id: "drone-08",
    projectId: "project-inspection",
    name: "Drone-08",
    kind: "drone",
    status: "offline",
    health: "restricted",
    operationMode: "보관",
    batteryPercent: 100,
    taskName: "할당 없음",
    taskProgress: 0,
    lastSeenAt: "2026-08-19T18:10:00+09:00",
    stateVersion: 19,
  },
];

const sources: DataSource[] = [
  {
    id: "robot-07-live",
    projectId: "project-logistics",
    deviceId: "robot-07",
    name: "실시간",
    kind: "live",
    status: "recording",
    rrdUrl: `${SAMPLE_BASE}/arkit_scenes.rrd`,
    capturedAt: "2026-08-20T09:32:08+09:00",
    topicIds: ["pose", "front-camera", "velocity", "battery", "planner"],
  },
  {
    id: "robot-07-incident",
    projectId: "project-logistics",
    deviceId: "robot-07",
    name: "08:42 경로 이탈",
    kind: "recording",
    status: "ready",
    rrdUrl: `${SAMPLE_BASE}/arkit_scenes.rrd`,
    capturedAt: "2026-08-20T08:42:10+09:00",
    durationLabel: "04:18",
    topicIds: ["pose", "front-camera", "velocity", "battery", "planner"],
  },
  {
    id: "robot-07-review",
    projectId: "project-logistics",
    deviceId: "robot-07",
    name: "어제 마지막 운행",
    kind: "recording",
    status: "ready",
    rrdUrl: `${SAMPLE_BASE}/objectron.rrd`,
    capturedAt: "2026-08-19T17:14:00+09:00",
    durationLabel: "21:06",
    topicIds: ["pose", "front-camera", "velocity", "battery"],
  },
  {
    id: "robot-12-live",
    projectId: "project-logistics",
    deviceId: "robot-12",
    name: "실시간",
    kind: "live",
    status: "recording",
    rrdUrl: `${SAMPLE_BASE}/arkit_scenes.rrd`,
    capturedAt: "2026-08-20T09:32:05+09:00",
    topicIds: ["pose", "front-camera", "velocity", "battery", "planner"],
  },
  {
    id: "robot-21-review",
    projectId: "project-logistics",
    deviceId: "robot-21",
    name: "정비 전 운행",
    kind: "recording",
    status: "ready",
    rrdUrl: `${SAMPLE_BASE}/arkit_scenes.rrd`,
    capturedAt: "2026-08-19T16:20:00+09:00",
    durationLabel: "10:44",
    topicIds: ["pose", "front-camera", "velocity", "battery"],
  },
  {
    id: "drone-03-live",
    projectId: "project-inspection",
    deviceId: "drone-03",
    name: "실시간",
    kind: "live",
    status: "recording",
    rrdUrl: `${SAMPLE_BASE}/objectron.rrd`,
    capturedAt: "2026-08-20T09:31:59+09:00",
    topicIds: ["pose", "front-camera", "altitude", "battery", "planner"],
  },
  {
    id: "drone-03-review",
    projectId: "project-inspection",
    deviceId: "drone-03",
    name: "서측 패널 점검",
    kind: "recording",
    status: "ready",
    rrdUrl: `${SAMPLE_BASE}/objectron.rrd`,
    capturedAt: "2026-08-19T14:12:00+09:00",
    durationLabel: "14:32",
    topicIds: ["pose", "front-camera", "altitude", "battery"],
  },
];

const topicTemplates: Omit<Topic, "deviceId" | "updatedAt">[] = [
  {
    id: "pose",
    path: "/localization/pose",
    label: "위치와 주변",
    renderer: "spatial",
    quality: "fresh",
    value: "Bay 2 → Bay 3",
  },
  {
    id: "front-camera",
    path: "/camera/front/image",
    label: "전방 카메라",
    renderer: "camera",
    quality: "fresh",
    value: "30 fps",
  },
  {
    id: "velocity",
    path: "/vehicle/velocity",
    label: "속도",
    renderer: "timeseries",
    quality: "fresh",
    value: "1.2",
    unit: "m/s",
    samples: [0.4, 0.6, 0.8, 1.1, 1.3, 1.2, 1.2, 1.18, 1.2],
  },
  {
    id: "altitude",
    path: "/flight/altitude",
    label: "고도",
    renderer: "timeseries",
    quality: "fresh",
    value: "0",
    unit: "m",
    samples: [0, 0, 0.1, 0, 0, 0, 0.1, 0, 0],
  },
  {
    id: "battery",
    path: "/power/battery",
    label: "배터리",
    renderer: "state",
    quality: "fresh",
    value: "78",
    unit: "%",
  },
  {
    id: "planner",
    path: "/planning/status",
    label: "경로 계획",
    renderer: "log",
    quality: "fresh",
    message: "전방 통로를 확인했습니다.",
  },
];

function clone<T>(value: T): T {
  return structuredClone(value);
}

function wait(ms = 90): Promise<void> {
  return new Promise((resolve) => globalThis.setTimeout(resolve, ms));
}

export class MockRmsApi implements RmsApi {
  private readonly leases = new Map<string, ControlLease>();
  private readonly receipts = new Map<string, CommandReceipt>();

  async listProjects(): Promise<Project[]> {
    await wait();
    return clone(projects);
  }

  async listDevices(projectId: string): Promise<Device[]> {
    await wait();
    return clone(devices.filter((device) => device.projectId === projectId));
  }

  async listDataSources(projectId: string, deviceId: string): Promise<DataSource[]> {
    await wait();
    return clone(
      sources.filter(
        (source) => source.projectId === projectId && source.deviceId === deviceId,
      ),
    );
  }

  async listTopics(deviceId: string, dataSourceId: string): Promise<Topic[]> {
    await wait();
    const source = sources.find((candidate) => candidate.id === dataSourceId);
    if (!source) {
      return [];
    }
    const device = devices.find((candidate) => candidate.id === deviceId);
    return topicTemplates
      .filter((topic) => source.topicIds.includes(topic.id))
      .map((topic) => ({
        ...clone(topic),
        deviceId,
        value:
          topic.id === "battery" && device?.batteryPercent != null
            ? String(device.batteryPercent)
            : topic.value,
        quality: device?.status === "degraded" ? "delayed" : topic.quality,
        updatedAt: source.capturedAt,
      }));
  }

  async getControlLease(deviceId: string): Promise<ControlLease | null> {
    await wait(40);
    return clone(this.leases.get(deviceId) ?? null);
  }

  async requestControlLease(
    deviceId: string,
    expectedDeviceVersion: number,
  ): Promise<ControlLease> {
    await wait(180);
    const device = devices.find((candidate) => candidate.id === deviceId);
    if (!device || device.status !== "online") {
      throw new Error("현재 장비의 제어권을 받을 수 없습니다.");
    }
    if (device.health === "critical" || device.health === "restricted") {
      throw new Error("장비 안전 상태를 먼저 확인해야 합니다.");
    }
    if (device.stateVersion !== expectedDeviceVersion) {
      throw new Error("장비 상태가 변경되었습니다. 상태를 다시 확인해 주세요.");
    }
    const existing = this.leases.get(deviceId);
    if (existing && existing.holderId !== OPERATOR_ID) {
      throw new Error(`${existing.holderName}님이 제어 중입니다.`);
    }
    const lease: ControlLease = {
      id: `lease-${deviceId}-${crypto.randomUUID()}`,
      deviceId,
      holderId: OPERATOR_ID,
      holderName: "나",
      expiresAt: new Date(Date.now() + 120_000).toISOString(),
      epoch: (existing?.epoch ?? 0) + 1,
    };
    this.leases.set(deviceId, lease);
    return clone(lease);
  }

  async releaseControlLease(deviceId: string, leaseId: string): Promise<void> {
    await wait(100);
    if (this.leases.get(deviceId)?.id === leaseId) {
      this.leases.delete(deviceId);
    }
  }

  async sendControlCommand(request: ControlCommandRequest): Promise<CommandReceipt> {
    await wait(240);
    const previous = this.receipts.get(request.idempotencyKey);
    if (previous) {
      return clone(previous);
    }
    const device = devices.find((candidate) => candidate.id === request.deviceId);
    const lease = this.leases.get(request.deviceId);
    if (request.sessionMode !== "live") {
      throw new Error("Replay 또는 일시정지 상태에서는 명령을 실행할 수 없습니다.");
    }
    if (!device || device.status !== "online") {
      throw new Error("장비 연결을 확인해야 합니다.");
    }
    if (device.stateVersion !== request.expectedDeviceVersion) {
      throw new Error("장비 상태가 변경되었습니다. 상태를 다시 확인해 주세요.");
    }
    if (!lease || lease.id !== request.leaseId || lease.holderId !== OPERATOR_ID) {
      throw new Error("유효한 제어권이 없습니다.");
    }
    if (Date.parse(lease.expiresAt) <= Date.now()) {
      throw new Error("제어권이 만료되었습니다.");
    }
    if (lease.epoch !== request.leaseEpoch) {
      throw new Error("이전 제어권으로 보낸 명령입니다.");
    }
    if (Date.parse(request.expiresAt) <= Date.now()) {
      throw new Error("명령 유효시간이 지났습니다.");
    }

    const message =
      request.commandType === "safe_stop"
        ? "안전 정지 요청을 장비가 받았습니다."
        : request.commandType === "pause_mission"
          ? "Mission을 일시정지했습니다."
          : "Mission을 계속 진행합니다.";
    const receipt: CommandReceipt = {
      commandId: `cmd-${crypto.randomUUID()}`,
      commandType: request.commandType,
      status: "accepted",
      message,
      createdAt: new Date().toISOString(),
    };
    this.receipts.set(request.idempotencyKey, receipt);
    return clone(receipt);
  }

  subscribeEvents(
    projectId: string,
    deviceId: string,
    dataSourceId: string,
    onEvent: (event: RmsEvent) => void,
  ): () => void {
    const device = devices.find((candidate) => candidate.id === deviceId);
    const source = sources.find((candidate) => candidate.id === dataSourceId);
    if (!source || source.kind !== "live") {
      return () => undefined;
    }
    const topicId = device?.kind === "drone" ? "altitude" : "velocity";
    let tick = 0;
    const timer = globalThis.setInterval(() => {
      tick += 1;
      const base = topicId === "altitude" ? 0 : 1.18;
      const amplitude = topicId === "altitude" ? 0.04 : 0.12;
      const value = base + Math.sin(tick / 2.4) * amplitude;
      const samples = Array.from({ length: 9 }, (_, index) => {
        return base + Math.sin((tick - 8 + index) / 2.4) * amplitude;
      });
      onEvent({
        eventId: `mock-event-${tick}`,
        type: "topic.value.changed",
        occurredAt: new Date().toISOString(),
        projectId,
        deviceId,
        resourceVersion: (device?.stateVersion ?? 0) + tick,
        data: {
          id: topicId,
          value: topicId === "altitude" ? value.toFixed(1) : value.toFixed(2),
          samples,
          quality: device?.status === "degraded" ? "delayed" : "fresh",
          updatedAt: new Date().toISOString(),
        },
      });
    }, 1_200);
    return () => globalThis.clearInterval(timer);
  }
}
