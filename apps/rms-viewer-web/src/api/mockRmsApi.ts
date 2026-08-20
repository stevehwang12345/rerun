import {
  OPERATOR_ID,
  type CommandReceipt,
  type ControlLease,
  type DataAssignment,
  type DataSource,
  type Device,
  type DeviceAssignment,
  type Integration,
  type LiveControlCommandRequest,
  type LiveSession,
  type Project,
  type Recording,
  type ReplaySession,
  type RmsEvent,
  type Topic,
  type WorkspaceEvent,
  type WorkspaceSnapshot,
} from "../domain";
import type {
  AssignDataSourceInput,
  AssignDeviceInput,
  ControlApi,
  CreateIntegrationInput,
  CreateLiveSessionInput,
  CreateProjectInput,
  CreateReplaySessionInput,
  IntegrationApi,
  LiveApi,
  ProjectApi,
  RegisterDataSourceInput,
  RegisterDeviceInput,
  ReplayApi,
  RmsApi,
} from "./rmsApi";

const SAMPLE_BASE = "https://app.rerun.io/version/0.36.1/examples";
const ORGANIZATION_ID = "organization-rms";

function clone<T>(value: T): T {
  return structuredClone(value);
}

function now(): string {
  return new Date().toISOString();
}

function makeId(prefix: string): string {
  return `${prefix}-${crypto.randomUUID()}`;
}

interface MockRmsApiOptions {
  seed?: boolean;
  latencyMs?: number;
  eventIntervalMs?: number;
}

class MockStore {
  revision = 1;
  readonly integrations = new Map<string, Integration>();
  readonly devices = new Map<string, Device>();
  readonly dataSources = new Map<string, DataSource>();
  readonly projects = new Map<string, Project>();
  readonly deviceAssignments = new Map<string, DeviceAssignment>();
  readonly dataAssignments = new Map<string, DataAssignment>();
  readonly topics = new Map<string, Topic[]>();
  readonly liveSessions = new Map<string, LiveSession>();
  readonly recordings = new Map<string, Recording>();
  readonly replaySessions = new Map<string, ReplaySession>();
  readonly leases = new Map<string, ControlLease>();
  readonly leaseEpochs = new Map<string, number>();
  readonly receipts = new Map<string, CommandReceipt>();
  readonly workspaceListeners = new Map<string, Set<(event: WorkspaceEvent) => void>>();
  readonly liveListeners = new Map<string, Set<(event: RmsEvent) => void>>();

  nextRevision(): number {
    this.revision += 1;
    return this.revision;
  }
}

export class MockRmsApi implements RmsApi {
  readonly integrations: IntegrationApi;
  readonly projects: ProjectApi;
  readonly live: LiveApi;
  readonly replay: ReplayApi;
  readonly control: ControlApi;

  private readonly store = new MockStore();
  private readonly latencyMs: number;
  private readonly eventIntervalMs: number;
  private readonly eventTimers = new Map<string, ReturnType<typeof globalThis.setInterval>>();

  constructor(options: MockRmsApiOptions = {}) {
    this.latencyMs = options.latencyMs ?? 0;
    this.eventIntervalMs = options.eventIntervalMs ?? 1_200;
    if (options.seed !== false) {
      this.seed();
    }

    this.integrations = {
      listIntegrations: () => this.listRegisteredIntegrations(),
      createIntegration: (input) => this.createIntegration(input),
      listDevices: () => this.listRegisteredDevices(),
      registerDevice: (input) => this.registerDevice(input),
      listDataSources: (deviceId) => this.listRegisteredDataSources(deviceId),
      registerDataSource: (input) => this.registerDataSource(input),
      listTopics: (dataSourceId) => this.listRegisteredTopics(dataSourceId),
    };
    this.projects = {
      listProjects: () => this.listManagedProjects(),
      createProject: (input) => this.createProject(input),
      getWorkspace: (projectId) => this.getWorkspace(projectId),
      assignDevice: (projectId, input) => this.assignDevice(projectId, input),
      assignDataSource: (projectId, input) => this.assignDataSource(projectId, input),
      subscribeWorkspace: (projectId, onEvent) =>
        this.subscribeWorkspace(projectId, onEvent),
    };
    this.live = {
      createSession: (input) => this.createLiveSession(input),
      getSession: (sessionId) => this.getLiveSession(sessionId),
      closeSession: (sessionId) => this.closeLiveSession(sessionId),
      subscribeEvents: (sessionId, onEvent) => this.subscribeLiveEvents(sessionId, onEvent),
    };
    this.replay = {
      listRecordings: (projectId) => this.listRecordings(projectId),
      createSession: (input) => this.createReplaySession(input),
      getSession: (sessionId) => this.getReplaySession(sessionId),
      closeSession: (sessionId) => this.closeReplaySession(sessionId),
    };
    this.control = {
      getLease: (liveSessionId) => this.getSessionLease(liveSessionId),
      requestLease: (liveSessionId, expectedDeviceVersion) =>
        this.requestSessionLease(liveSessionId, expectedDeviceVersion),
      releaseLease: (liveSessionId, leaseId) =>
        this.releaseSessionLease(liveSessionId, leaseId),
      sendCommand: (request) => this.sendSessionCommand(request),
    };
  }

  private delay(multiplier = 1): Promise<void> {
    const duration = this.latencyMs * multiplier;
    return duration > 0
      ? new Promise((resolve) => globalThis.setTimeout(resolve, duration))
      : Promise.resolve();
  }

  private seed(): void {
    const capturedAt = "2026-08-20T09:32:08+09:00";
    const integrations: Integration[] = [
      {
        id: "integration-logistics",
        organizationId: ORGANIZATION_ID,
        name: "물류 ROS 2",
        kind: "ros2",
        status: "connected",
        endpointLabel: "A동 Edge Gateway",
        lastHealthAt: capturedAt,
        createdAt: "2026-08-01T00:00:00+09:00",
        resourceVersion: 1,
      },
      {
        id: "integration-inspection",
        organizationId: ORGANIZATION_ID,
        name: "시설 MAVLink",
        kind: "mavlink",
        status: "connected",
        endpointLabel: "야외 Drone Gateway",
        lastHealthAt: "2026-08-20T09:31:59+09:00",
        createdAt: "2026-08-02T00:00:00+09:00",
        resourceVersion: 1,
      },
    ];
    integrations.forEach((integration) => this.store.integrations.set(integration.id, integration));

    const devices: Device[] = [
      this.fixtureDevice("robot-07", "integration-logistics", "Robot-07", "online", "normal", 78, 142),
      this.fixtureDevice("robot-12", "integration-logistics", "Robot-12", "degraded", "attention", 41, 87),
      this.fixtureDevice("robot-21", "integration-logistics", "Robot-21", "offline", "restricted", 0, 31),
      {
        ...this.fixtureDevice("drone-03", "integration-inspection", "Drone-03", "online", "normal", 86, 55),
        kind: "drone",
        operationMode: "임무 대기",
        taskName: "동측 패널 점검",
      },
      {
        ...this.fixtureDevice("drone-08", "integration-inspection", "Drone-08", "offline", "restricted", 100, 19),
        kind: "drone",
        operationMode: "보관",
        taskName: "할당 없음",
      },
    ];
    devices.forEach((device) => this.store.devices.set(device.id, device));

    const sources: DataSource[] = [
      this.fixtureSource("robot-07-source", "integration-logistics", "robot-07", capturedAt),
      this.fixtureSource("robot-12-source", "integration-logistics", "robot-12", "2026-08-20T09:32:05+09:00"),
      this.fixtureSource("robot-21-source", "integration-logistics", "robot-21", "2026-08-20T08:51:00+09:00"),
      this.fixtureSource("drone-03-source", "integration-inspection", "drone-03", "2026-08-20T09:31:59+09:00", true),
      this.fixtureSource("drone-08-source", "integration-inspection", "drone-08", "2026-08-19T18:10:00+09:00", true),
    ];
    sources.forEach((source) => {
      this.store.dataSources.set(source.id, source);
      this.store.topics.set(source.id, this.fixtureTopics(source));
    });

    const projects: Project[] = [
      this.fixtureProject("project-logistics", "물류 자동화", "A동 물류 로봇 운영", "active"),
      this.fixtureProject("project-inspection", "시설 점검", "야외 설비 드론 점검", "standby"),
    ];
    projects.forEach((project) => this.store.projects.set(project.id, project));

    this.seedAssignments("project-logistics", ["robot-07", "robot-12", "robot-21"]);
    this.seedAssignments("project-inspection", ["drone-03", "drone-08"]);

    [
      this.fixtureRecording(
        "recording-robot-07-incident",
        "project-logistics",
        "robot-07",
        "robot-07-source",
        "08:42 경로 이탈",
        "2026-08-20T08:42:10+09:00",
        "04:18",
      ),
      this.fixtureRecording(
        "recording-robot-07-review",
        "project-logistics",
        "robot-07",
        "robot-07-source",
        "어제 마지막 운행",
        "2026-08-19T17:14:00+09:00",
        "21:06",
      ),
      this.fixtureRecording(
        "recording-robot-21-review",
        "project-logistics",
        "robot-21",
        "robot-21-source",
        "정비 전 운행",
        "2026-08-19T16:20:00+09:00",
        "10:44",
      ),
      this.fixtureRecording(
        "recording-drone-03-review",
        "project-inspection",
        "drone-03",
        "drone-03-source",
        "서측 패널 점검",
        "2026-08-19T14:12:00+09:00",
        "14:32",
      ),
    ].forEach((recording) => this.store.recordings.set(recording.id, recording));
  }

  private fixtureDevice(
    id: string,
    integrationId: string,
    name: string,
    status: Device["status"],
    health: Device["health"],
    batteryPercent: number,
    stateVersion: number,
  ): Device {
    return {
      id,
      organizationId: ORGANIZATION_ID,
      integrationId,
      name,
      kind: "robot",
      status,
      health,
      operationMode: status === "online" ? "자율 운행" : "대기",
      batteryPercent,
      taskName: status === "online" ? "Bay 3 이동" : "충전 위치 이동",
      taskProgress: status === "online" ? 62 : 18,
      lastSeenAt: "2026-08-20T09:32:08+09:00",
      stateVersion,
    };
  }

  private fixtureSource(
    id: string,
    integrationId: string,
    deviceId: string,
    lastDataAt: string,
    drone = false,
  ): DataSource {
    return {
      id,
      integrationId,
      deviceId,
      name: "실시간",
      protocol: drone ? "MAVLink + Rerun" : "ROS 2 + Rerun",
      status: "recording",
      liveUrl: `${SAMPLE_BASE}/arkit_scenes.rrd`,
      topicIds: ["pose", "front-camera", drone ? "altitude" : "velocity", "battery", "planner"],
      mappingVersion: 1,
      lastDataAt,
    };
  }

  private fixtureProject(
    id: string,
    name: string,
    description: string,
    status: Project["status"],
  ): Project {
    return {
      id,
      organizationId: ORGANIZATION_ID,
      name,
      description,
      status,
      deviceCount: 0,
      onlineDeviceCount: 0,
      createdAt: "2026-08-01T00:00:00+09:00",
      resourceVersion: 1,
    };
  }

  private fixtureTopics(source: DataSource): Topic[] {
    const templates: Omit<Topic, "dataSourceId" | "deviceId" | "updatedAt">[] = [
      { id: "pose", path: "/localization/pose", label: "위치와 주변", renderer: "spatial", quality: "fresh", value: "운행 구역" },
      { id: "front-camera", path: "/camera/front/image", label: "전방 카메라", renderer: "camera", quality: "fresh", value: "30 fps" },
      { id: "velocity", path: "/vehicle/velocity", label: "속도", renderer: "timeseries", quality: "fresh", value: "1.2", unit: "m/s", samples: [0.4, 0.8, 1.1, 1.2] },
      { id: "altitude", path: "/flight/altitude", label: "고도", renderer: "timeseries", quality: "fresh", value: "0", unit: "m", samples: [0, 0.1, 0] },
      { id: "battery", path: "/power/battery", label: "배터리", renderer: "state", quality: "fresh", value: "78", unit: "%" },
      { id: "planner", path: "/planning/status", label: "경로 계획", renderer: "log", quality: "fresh", message: "경로를 확인했습니다." },
    ];
    const device = this.store.devices.get(source.deviceId);
    return templates
      .filter((topic) => source.topicIds.includes(topic.id))
      .map((topic) => ({
        ...topic,
        dataSourceId: source.id,
        deviceId: source.deviceId,
        value:
          topic.id === "battery" && device?.batteryPercent != null
            ? String(device.batteryPercent)
            : topic.value,
        quality: device?.status === "degraded" ? "delayed" : topic.quality,
        updatedAt: source.lastDataAt,
      }));
  }

  private seedAssignments(projectId: string, deviceIds: string[]): void {
    deviceIds.forEach((deviceId) => {
      const source = [...this.store.dataSources.values()].find(
        (candidate) => candidate.deviceId === deviceId,
      );
      const deviceAssignment: DeviceAssignment = {
        id: `assignment-${projectId}-${deviceId}`,
        projectId,
        deviceId,
        accessMode: "control",
        validFrom: "2026-08-01T00:00:00+09:00",
        resourceVersion: 1,
      };
      this.store.deviceAssignments.set(deviceAssignment.id, deviceAssignment);
      if (source) {
        const dataAssignment: DataAssignment = {
          id: `assignment-${projectId}-${source.id}`,
          projectId,
          dataSourceId: source.id,
          visibility: "operator",
          validFrom: "2026-08-01T00:00:00+09:00",
          resourceVersion: 1,
        };
        this.store.dataAssignments.set(dataAssignment.id, dataAssignment);
      }
    });
  }

  private fixtureRecording(
    id: string,
    projectId: string,
    deviceId: string,
    dataSourceId: string,
    name: string,
    capturedAt: string,
    durationLabel: string,
  ): Recording {
    const project = this.requireProject(projectId);
    const deviceAssignment = this.requireActiveDeviceAssignment(projectId, deviceId);
    const dataAssignment = this.requireActiveDataAssignment(projectId, dataSourceId);
    const source = this.requireDataSource(dataSourceId);
    return {
      id,
      organizationId: project.organizationId,
      projectId,
      deviceId,
      dataSourceId,
      name,
      status: "ready",
      rrdUrl: `${SAMPLE_BASE}/arkit_scenes.rrd`,
      capturedAt,
      durationLabel,
      topicIds: clone(source.topicIds),
      mappingVersion: source.mappingVersion,
      projectSnapshot: {
        projectId,
        projectName: project.name,
        capturedAt,
        deviceAssignmentId: deviceAssignment.id,
        dataAssignmentId: dataAssignment.id,
      },
      resourceVersion: 1,
    };
  }

  private requireProject(projectId: string): Project {
    const project = this.store.projects.get(projectId);
    if (!project) {
      throw new Error("프로젝트를 찾을 수 없습니다.");
    }
    return project;
  }

  private requireDevice(deviceId: string): Device {
    const device = this.store.devices.get(deviceId);
    if (!device) {
      throw new Error("장비를 찾을 수 없습니다.");
    }
    return device;
  }

  private requireDataSource(dataSourceId: string): DataSource {
    const source = this.store.dataSources.get(dataSourceId);
    if (!source) {
      throw new Error("데이터 연동을 찾을 수 없습니다.");
    }
    return source;
  }

  private requireActiveDeviceAssignment(projectId: string, deviceId: string): DeviceAssignment {
    const assignment = [...this.store.deviceAssignments.values()].find(
      (candidate) =>
        candidate.projectId === projectId &&
        candidate.deviceId === deviceId &&
        candidate.validTo == null,
    );
    if (!assignment) {
      throw new Error("프로젝트에 배정되지 않은 장비입니다.");
    }
    return assignment;
  }

  private requireActiveDataAssignment(projectId: string, dataSourceId: string): DataAssignment {
    const assignment = [...this.store.dataAssignments.values()].find(
      (candidate) =>
        candidate.projectId === projectId &&
        candidate.dataSourceId === dataSourceId &&
        candidate.validTo == null,
    );
    if (!assignment) {
      throw new Error("프로젝트에 배정되지 않은 데이터입니다.");
    }
    return assignment;
  }

  private requireOpenLiveSession(sessionId: string): LiveSession {
    const session = this.store.liveSessions.get(sessionId);
    if (!session || session.status !== "open") {
      throw new Error("활성 Live 세션을 찾을 수 없습니다.");
    }
    return session;
  }

  private emitWorkspace(projectId: string): void {
    const event: WorkspaceEvent = {
      eventId: makeId("workspace-event"),
      type: "project.workspace.changed",
      occurredAt: now(),
      projectId,
      snapshotVersion: this.store.revision,
    };
    this.store.workspaceListeners.get(projectId)?.forEach((listener) => listener(clone(event)));
  }

  private emitLive(sessionId: string, event: RmsEvent): void {
    this.store.liveListeners.get(sessionId)?.forEach((listener) => listener(clone(event)));
  }

  private async listRegisteredIntegrations(): Promise<Integration[]> {
    await this.delay();
    return clone([...this.store.integrations.values()]);
  }

  private async createIntegration(input: CreateIntegrationInput): Promise<Integration> {
    await this.delay();
    const revision = this.store.nextRevision();
    const integration: Integration = {
      id: makeId("integration"),
      ...input,
      status: "testing",
      lastHealthAt: now(),
      createdAt: now(),
      resourceVersion: revision,
    };
    this.store.integrations.set(integration.id, integration);
    return clone(integration);
  }

  private async listRegisteredDevices(): Promise<Device[]> {
    await this.delay();
    return clone([...this.store.devices.values()]);
  }

  private async registerDevice(input: RegisterDeviceInput): Promise<Device> {
    await this.delay();
    if (!this.store.integrations.has(input.integrationId)) {
      throw new Error("먼저 데이터 연동을 등록해야 합니다.");
    }
    const id = input.id ?? makeId("device");
    if (this.store.devices.has(id)) {
      throw new Error("이미 등록된 장비입니다.");
    }
    const device: Device = { ...input, id, stateVersion: 1 };
    this.store.devices.set(device.id, device);
    this.store.nextRevision();
    return clone(device);
  }

  private async listRegisteredDataSources(deviceId?: string): Promise<DataSource[]> {
    await this.delay();
    return clone(
      [...this.store.dataSources.values()].filter(
        (source) => deviceId == null || source.deviceId === deviceId,
      ),
    );
  }

  private async registerDataSource(input: RegisterDataSourceInput): Promise<DataSource> {
    await this.delay();
    const device = this.requireDevice(input.deviceId);
    if (!this.store.integrations.has(input.integrationId)) {
      throw new Error("먼저 데이터 연동을 등록해야 합니다.");
    }
    if (device.integrationId !== input.integrationId) {
      throw new Error("장비와 데이터는 같은 연동을 사용해야 합니다.");
    }
    const id = input.id ?? makeId("data-source");
    if (this.store.dataSources.has(id)) {
      throw new Error("이미 등록된 데이터 연동입니다.");
    }
    const source: DataSource = { ...input, id, mappingVersion: 1 };
    this.store.dataSources.set(source.id, source);
    this.store.topics.set(source.id, this.fixtureTopics(source));
    this.store.nextRevision();
    return clone(source);
  }

  private async listRegisteredTopics(dataSourceId: string): Promise<Topic[]> {
    await this.delay();
    return clone(this.store.topics.get(dataSourceId) ?? []);
  }

  private projectWithCounts(project: Project): Project {
    const assignments = [...this.store.deviceAssignments.values()].filter(
      (assignment) => assignment.projectId === project.id && assignment.validTo == null,
    );
    const devices = assignments
      .map((assignment) => this.store.devices.get(assignment.deviceId))
      .filter((device): device is Device => device != null);
    return {
      ...project,
      deviceCount: devices.length,
      onlineDeviceCount: devices.filter((device) => device.status === "online").length,
    };
  }

  private async listManagedProjects(): Promise<Project[]> {
    await this.delay();
    return clone([...this.store.projects.values()].map((project) => this.projectWithCounts(project)));
  }

  private async createProject(input: CreateProjectInput): Promise<Project> {
    await this.delay();
    const revision = this.store.nextRevision();
    const project: Project = {
      id: makeId("project"),
      ...input,
      deviceCount: 0,
      onlineDeviceCount: 0,
      createdAt: now(),
      resourceVersion: revision,
    };
    this.store.projects.set(project.id, project);
    return clone(project);
  }

  private async assignDevice(
    projectId: string,
    input: AssignDeviceInput,
  ): Promise<DeviceAssignment> {
    await this.delay();
    this.requireProject(projectId);
    this.requireDevice(input.deviceId);
    const existing = [...this.store.deviceAssignments.values()].find(
      (candidate) =>
        candidate.projectId === projectId &&
        candidate.deviceId === input.deviceId &&
        candidate.validTo == null,
    );
    if (existing) {
      return clone(existing);
    }
    if (input.accessMode === "control") {
      const conflict = [...this.store.deviceAssignments.values()].find(
        (candidate) =>
          candidate.projectId !== projectId &&
          candidate.deviceId === input.deviceId &&
          candidate.accessMode === "control" &&
          candidate.validTo == null,
      );
      if (conflict) {
        throw new Error("장비는 동시에 두 프로젝트에서 제어할 수 없습니다.");
      }
    }
    const revision = this.store.nextRevision();
    const assignment: DeviceAssignment = {
      id: makeId("device-assignment"),
      projectId,
      ...input,
      validFrom: now(),
      resourceVersion: revision,
    };
    this.store.deviceAssignments.set(assignment.id, assignment);
    this.emitWorkspace(projectId);
    return clone(assignment);
  }

  private async assignDataSource(
    projectId: string,
    input: AssignDataSourceInput,
  ): Promise<DataAssignment> {
    await this.delay();
    this.requireProject(projectId);
    const source = this.requireDataSource(input.dataSourceId);
    this.requireActiveDeviceAssignment(projectId, source.deviceId);
    const existing = [...this.store.dataAssignments.values()].find(
      (candidate) =>
        candidate.projectId === projectId &&
        candidate.dataSourceId === input.dataSourceId &&
        candidate.validTo == null,
    );
    if (existing) {
      return clone(existing);
    }
    const revision = this.store.nextRevision();
    const assignment: DataAssignment = {
      id: makeId("data-assignment"),
      projectId,
      ...input,
      validFrom: now(),
      resourceVersion: revision,
    };
    this.store.dataAssignments.set(assignment.id, assignment);
    this.emitWorkspace(projectId);
    return clone(assignment);
  }

  private async getWorkspace(projectId: string): Promise<WorkspaceSnapshot> {
    await this.delay();
    const snapshotVersion = this.store.revision;
    const project = this.projectWithCounts(this.requireProject(projectId));
    const deviceAssignments = [...this.store.deviceAssignments.values()].filter(
      (assignment) => assignment.projectId === projectId && assignment.validTo == null,
    );
    const dataAssignments = [...this.store.dataAssignments.values()].filter(
      (assignment) => assignment.projectId === projectId && assignment.validTo == null,
    );
    const deviceIds = new Set(deviceAssignments.map((assignment) => assignment.deviceId));
    const sourceIds = new Set(dataAssignments.map((assignment) => assignment.dataSourceId));
    const devices = [...this.store.devices.values()].filter((device) => deviceIds.has(device.id));
    const dataSources = [...this.store.dataSources.values()].filter((source) =>
      sourceIds.has(source.id),
    );
    const topicsByDataSource = Object.fromEntries(
      dataSources.map((source) => [source.id, clone(this.store.topics.get(source.id) ?? [])]),
    );
    const recordings = [...this.store.recordings.values()].filter(
      (recording) => recording.projectId === projectId,
    );
    return clone({
      snapshotVersion,
      capturedAt: now(),
      project,
      deviceAssignments,
      dataAssignments,
      devices,
      dataSources,
      recordings,
      topicsByDataSource,
    });
  }

  private subscribeWorkspace(
    projectId: string,
    onEvent: (event: WorkspaceEvent) => void,
  ): () => void {
    const listeners = this.store.workspaceListeners.get(projectId) ?? new Set();
    listeners.add(onEvent);
    this.store.workspaceListeners.set(projectId, listeners);
    return () => {
      listeners.delete(onEvent);
      if (listeners.size === 0) {
        this.store.workspaceListeners.delete(projectId);
      }
    };
  }

  private openLiveSession(input: CreateLiveSessionInput): LiveSession {
    this.requireProject(input.projectId);
    this.requireActiveDeviceAssignment(input.projectId, input.deviceId);
    this.requireActiveDataAssignment(input.projectId, input.dataSourceId);
    const source = this.requireDataSource(input.dataSourceId);
    if (source.deviceId !== input.deviceId) {
      throw new Error("장비와 데이터 연동이 일치하지 않습니다.");
    }
    if (source.status === "offline") {
      throw new Error("현재 데이터 연동으로 Live 세션을 열 수 없습니다.");
    }
    const revision = this.store.nextRevision();
    const session: LiveSession = {
      id: makeId("live-session"),
      ...input,
      status: "open",
      playState: "following",
      sourceHealth: source.status === "degraded" ? "delayed" : "fresh",
      streamUrl: source.liveUrl,
      startedAt: now(),
      resourceVersion: revision,
    };
    this.store.liveSessions.set(session.id, session);
    return session;
  }

  private async createLiveSession(input: CreateLiveSessionInput): Promise<LiveSession> {
    await this.delay();
    return clone(this.openLiveSession(input));
  }

  private async getLiveSession(sessionId: string): Promise<LiveSession> {
    await this.delay();
    const session = this.store.liveSessions.get(sessionId);
    if (!session) {
      throw new Error("Live 세션을 찾을 수 없습니다.");
    }
    return clone(session);
  }

  private async closeLiveSession(sessionId: string): Promise<Recording> {
    await this.delay();
    const session = this.requireOpenLiveSession(sessionId);
    const source = this.requireDataSource(session.dataSourceId);
    const project = this.requireProject(session.projectId);
    const deviceAssignment = this.requireActiveDeviceAssignment(session.projectId, session.deviceId);
    const dataAssignment = this.requireActiveDataAssignment(session.projectId, session.dataSourceId);
    const revision = this.store.nextRevision();
    const closedAt = now();
    this.store.liveSessions.set(sessionId, {
      ...session,
      status: "closed",
      closedAt,
      resourceVersion: revision,
    });
    this.store.leases.delete(sessionId);
    const recording: Recording = {
      id: makeId("recording"),
      organizationId: project.organizationId,
      projectId: project.id,
      deviceId: session.deviceId,
      dataSourceId: session.dataSourceId,
      name: `${project.name} 운용 기록`,
      status: "ready",
      rrdUrl: source.liveUrl,
      capturedAt: session.startedAt,
      durationLabel: "방금 종료",
      topicIds: clone(source.topicIds),
      mappingVersion: source.mappingVersion,
      projectSnapshot: {
        projectId: project.id,
        projectName: project.name,
        capturedAt: closedAt,
        deviceAssignmentId: deviceAssignment.id,
        dataAssignmentId: dataAssignment.id,
      },
      resourceVersion: revision,
    };
    this.store.recordings.set(recording.id, recording);
    this.stopLiveEventTimer(sessionId);
    this.emitWorkspace(project.id);
    return clone(recording);
  }

  private subscribeLiveEvents(
    sessionId: string,
    onEvent: (event: RmsEvent) => void,
  ): () => void {
    const session = this.requireOpenLiveSession(sessionId);
    const listeners = this.store.liveListeners.get(sessionId) ?? new Set();
    listeners.add(onEvent);
    this.store.liveListeners.set(sessionId, listeners);
    this.startLiveEventTimer(session);
    return () => {
      listeners.delete(onEvent);
      if (listeners.size === 0) {
        this.store.liveListeners.delete(sessionId);
        this.stopLiveEventTimer(sessionId);
      }
    };
  }

  private startLiveEventTimer(session: LiveSession): void {
    if (this.eventTimers.has(session.id)) {
      return;
    }
    const device = this.requireDevice(session.deviceId);
    const source = this.requireDataSource(session.dataSourceId);
    const topicId = device.kind === "drone" ? "altitude" : "velocity";
    let tick = 0;
    const timer = globalThis.setInterval(() => {
      if (this.store.liveSessions.get(session.id)?.status !== "open") {
        this.stopLiveEventTimer(session.id);
        return;
      }
      tick += 1;
      const base = topicId === "altitude" ? 0 : 1.18;
      const amplitude = topicId === "altitude" ? 0.04 : 0.12;
      const value = base + Math.sin(tick / 2.4) * amplitude;
      const samples = Array.from({ length: 9 }, (_, index) =>
        base + Math.sin((tick - 8 + index) / 2.4) * amplitude,
      );
      this.emitLive(session.id, {
        eventId: makeId("live-event"),
        type: "topic.value.changed",
        occurredAt: now(),
        projectId: session.projectId,
        deviceId: session.deviceId,
        liveSessionId: session.id,
        resourceVersion: device.stateVersion + tick,
        data: {
          id: topicId,
          value: topicId === "altitude" ? value.toFixed(1) : value.toFixed(2),
          samples,
          quality: device.status === "degraded" ? "delayed" : "fresh",
          updatedAt: now(),
        },
      });
      source.lastDataAt = now();
    }, this.eventIntervalMs);
    this.eventTimers.set(session.id, timer);
  }

  private stopLiveEventTimer(sessionId: string): void {
    const timer = this.eventTimers.get(sessionId);
    if (timer != null) {
      globalThis.clearInterval(timer);
      this.eventTimers.delete(sessionId);
    }
  }

  private async listRecordings(projectId: string): Promise<Recording[]> {
    await this.delay();
    this.requireProject(projectId);
    return clone(
      [...this.store.recordings.values()].filter(
        (recording) => recording.projectId === projectId,
      ),
    );
  }

  private async createReplaySession(
    input: CreateReplaySessionInput,
  ): Promise<ReplaySession> {
    await this.delay();
    this.requireProject(input.projectId);
    const recording = this.store.recordings.get(input.recordingId);
    if (!recording || recording.projectId !== input.projectId || recording.status !== "ready") {
      throw new Error("재생 가능한 기록을 찾을 수 없습니다.");
    }
    const revision = this.store.nextRevision();
    const session: ReplaySession = {
      id: makeId("replay-session"),
      ...input,
      deviceId: recording.deviceId,
      status: "open",
      streamUrl: recording.rrdUrl,
      cursorSeconds: 0,
      openedAt: now(),
      resourceVersion: revision,
    };
    this.store.replaySessions.set(session.id, session);
    return clone(session);
  }

  private async getReplaySession(sessionId: string): Promise<ReplaySession> {
    await this.delay();
    const session = this.store.replaySessions.get(sessionId);
    if (!session) {
      throw new Error("Replay 세션을 찾을 수 없습니다.");
    }
    return clone(session);
  }

  private async closeReplaySession(sessionId: string): Promise<void> {
    await this.delay();
    const session = this.store.replaySessions.get(sessionId);
    if (!session) {
      return;
    }
    this.store.replaySessions.set(sessionId, {
      ...session,
      status: "closed",
      closedAt: now(),
      resourceVersion: this.store.nextRevision(),
    });
  }

  private async getSessionLease(liveSessionId: string): Promise<ControlLease | null> {
    await this.delay();
    return clone(this.store.leases.get(liveSessionId) ?? null);
  }

  private async requestSessionLease(
    liveSessionId: string,
    expectedDeviceVersion: number,
  ): Promise<ControlLease> {
    await this.delay(2);
    const session = this.requireOpenLiveSession(liveSessionId);
    if (session.playState !== "following") {
      throw new Error("LIVE Following 상태에서만 제어할 수 있습니다.");
    }
    const assignment = this.requireActiveDeviceAssignment(session.projectId, session.deviceId);
    if (assignment.accessMode !== "control") {
      throw new Error("이 프로젝트는 장비 관찰만 허용합니다.");
    }
    const device = this.requireDevice(session.deviceId);
    if (device.status !== "online") {
      throw new Error("현재 장비의 제어권을 받을 수 없습니다.");
    }
    if (device.health === "critical" || device.health === "restricted") {
      throw new Error("장비 안전 상태를 먼저 확인해야 합니다.");
    }
    if (device.stateVersion !== expectedDeviceVersion) {
      throw new Error("장비 상태가 변경되었습니다. 상태를 다시 확인해 주세요.");
    }
    for (const [sessionId, lease] of this.store.leases) {
      if (Date.parse(lease.expiresAt) <= Date.now()) {
        this.store.leases.delete(sessionId);
      }
    }
    const existing = [...this.store.leases.values()].find(
      (candidate) => candidate.deviceId === session.deviceId,
    );
    if (existing) {
      if (existing.liveSessionId === liveSessionId && existing.holderId === OPERATOR_ID) {
        return clone(existing);
      }
      throw new Error(`${existing.holderName}님이 제어 중입니다.`);
    }
    const epoch = (this.store.leaseEpochs.get(session.deviceId) ?? 0) + 1;
    this.store.leaseEpochs.set(session.deviceId, epoch);
    const lease: ControlLease = {
      id: makeId("lease"),
      liveSessionId,
      deviceId: session.deviceId,
      holderId: OPERATOR_ID,
      holderName: "나",
      expiresAt: new Date(Date.now() + 120_000).toISOString(),
      epoch,
    };
    this.store.leases.set(liveSessionId, lease);
    this.emitLive(liveSessionId, {
      eventId: makeId("lease-event"),
      type: "control.lease.changed",
      occurredAt: now(),
      projectId: session.projectId,
      deviceId: session.deviceId,
      liveSessionId,
      resourceVersion: this.store.nextRevision(),
      data: { lease },
    });
    return clone(lease);
  }

  private async releaseSessionLease(liveSessionId: string, leaseId: string): Promise<void> {
    await this.delay();
    const lease = this.store.leases.get(liveSessionId);
    if (lease?.id !== leaseId) {
      return;
    }
    this.store.leases.delete(liveSessionId);
    const session = this.store.liveSessions.get(liveSessionId);
    if (session) {
      this.emitLive(liveSessionId, {
        eventId: makeId("lease-event"),
        type: "control.lease.changed",
        occurredAt: now(),
        projectId: session.projectId,
        deviceId: session.deviceId,
        liveSessionId,
        resourceVersion: this.store.nextRevision(),
        data: { lease: null },
      });
    }
  }

  private async sendSessionCommand(
    request: LiveControlCommandRequest,
  ): Promise<CommandReceipt> {
    await this.delay(2);
    const receiptKey = `${request.liveSessionId}:${request.idempotencyKey}`;
    const previous = this.store.receipts.get(receiptKey);
    if (previous) {
      return clone(previous);
    }
    const session = this.requireOpenLiveSession(request.liveSessionId);
    if (session.deviceId !== request.deviceId || session.playState !== "following") {
      throw new Error("현재 Live 세션에서는 명령을 실행할 수 없습니다.");
    }
    const device = this.requireDevice(session.deviceId);
    const lease = this.store.leases.get(session.id);
    if (device.status !== "online") {
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
      commandId: makeId("command"),
      liveSessionId: session.id,
      commandType: request.commandType,
      status: "accepted",
      message,
      createdAt: now(),
    };
    this.store.receipts.set(receiptKey, receipt);
    this.emitLive(session.id, {
      eventId: makeId("command-event"),
      type: "command.state.changed",
      occurredAt: now(),
      projectId: session.projectId,
      deviceId: session.deviceId,
      liveSessionId: session.id,
      resourceVersion: this.store.nextRevision(),
      data: receipt,
    });
    return clone(receipt);
  }

}
