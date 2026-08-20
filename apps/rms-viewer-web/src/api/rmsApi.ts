import type {
  CommandReceipt,
  ControlLease,
  DataAssignment,
  DataSource,
  Device,
  DeviceAssignment,
  Integration,
  LiveControlCommandRequest,
  LiveSession,
  Project,
  Recording,
  ReplaySession,
  RmsEvent,
  Topic,
  WorkspaceEvent,
  WorkspaceSnapshot,
} from "../domain";

export type CreateIntegrationInput = Pick<
  Integration,
  "organizationId" | "name" | "kind" | "endpointLabel"
>;
export type RegisterDeviceInput = Omit<Device, "id" | "stateVersion"> & { id?: string };
export type RegisterDataSourceInput = Omit<DataSource, "id" | "mappingVersion"> & {
  id?: string;
};
export type CreateProjectInput = Pick<
  Project,
  "organizationId" | "name" | "description" | "status"
>;
export type AssignDeviceInput = Pick<DeviceAssignment, "deviceId" | "accessMode">;
export type AssignDataSourceInput = Pick<DataAssignment, "dataSourceId" | "visibility">;
export type CreateLiveSessionInput = Pick<
  LiveSession,
  "projectId" | "deviceId" | "dataSourceId" | "openedBy"
>;
export type CreateReplaySessionInput = Pick<
  ReplaySession,
  "projectId" | "recordingId" | "openedBy"
>;

export interface IntegrationApi {
  listIntegrations(): Promise<Integration[]>;
  createIntegration(input: CreateIntegrationInput): Promise<Integration>;
  listDevices(): Promise<Device[]>;
  registerDevice(input: RegisterDeviceInput): Promise<Device>;
  listDataSources(deviceId?: string): Promise<DataSource[]>;
  registerDataSource(input: RegisterDataSourceInput): Promise<DataSource>;
  listTopics(dataSourceId: string): Promise<Topic[]>;
}

export interface ProjectApi {
  listProjects(): Promise<Project[]>;
  createProject(input: CreateProjectInput): Promise<Project>;
  getWorkspace(projectId: string): Promise<WorkspaceSnapshot>;
  assignDevice(projectId: string, input: AssignDeviceInput): Promise<DeviceAssignment>;
  assignDataSource(projectId: string, input: AssignDataSourceInput): Promise<DataAssignment>;
  subscribeWorkspace(
    projectId: string,
    onEvent: (event: WorkspaceEvent) => void,
    onError?: () => void,
  ): () => void;
}

export interface LiveApi {
  createSession(input: CreateLiveSessionInput): Promise<LiveSession>;
  getSession(sessionId: string): Promise<LiveSession>;
  closeSession(sessionId: string): Promise<Recording>;
  subscribeEvents(
    sessionId: string,
    onEvent: (event: RmsEvent) => void,
    onError?: () => void,
  ): () => void;
}

/** Replay intentionally has no lease or command methods. */
export interface ReplayApi {
  listRecordings(projectId: string): Promise<Recording[]>;
  createSession(input: CreateReplaySessionInput): Promise<ReplaySession>;
  getSession(sessionId: string): Promise<ReplaySession>;
  closeSession(sessionId: string): Promise<void>;
}

export interface ControlApi {
  getLease(liveSessionId: string): Promise<ControlLease | null>;
  requestLease(liveSessionId: string, expectedDeviceVersion: number): Promise<ControlLease>;
  releaseLease(liveSessionId: string, leaseId: string): Promise<void>;
  sendCommand(request: LiveControlCommandRequest): Promise<CommandReceipt>;
}

export interface RmsApi {
  readonly integrations: IntegrationApi;
  readonly projects: ProjectApi;
  readonly live: LiveApi;
  readonly replay: ReplayApi;
  readonly control: ControlApi;
}

async function parseJson<T>(response: Response): Promise<T> {
  if (!response.ok) {
    const body = await response.text();
    console.error("RMS API request failed", response.status, body);
    if (response.status === 404) {
      throw new Error("요청한 정보를 찾지 못했습니다");
    }
    if (response.status === 409) {
      throw new Error("현재 상태에서는 요청할 수 없습니다");
    }
    throw new Error("요청을 처리하지 못했습니다");
  }
  return (await response.json()) as T;
}

class HttpClient {
  constructor(private readonly baseUrl: string) {}

  url(path: string): string {
    return `${this.baseUrl.replace(/\/$/, "")}${path}`;
  }

  async json<T>(path: string, init?: RequestInit): Promise<T> {
    return parseJson(
      await fetch(this.url(path), {
        credentials: "include",
        ...init,
      }),
    );
  }

  eventSource(path: string): EventSource {
    return new EventSource(this.url(path), { withCredentials: true });
  }
}

function mutationHeaders(idempotencyKey: string = crypto.randomUUID()): Record<string, string> {
  return {
    "Content-Type": "application/json",
    "Idempotency-Key": idempotencyKey,
    "X-RMS-Request-ID": crypto.randomUUID(),
  };
}

function subscribeJsonEvents<T>(
  source: EventSource,
  eventTypes: string[],
  onEvent: (event: T) => void,
  onError?: () => void,
): () => void {
  const consume = (message: MessageEvent<string>) => {
    try {
      onEvent(JSON.parse(message.data) as T);
    } catch {
      onError?.();
    }
  };
  source.onmessage = consume;
  eventTypes.forEach((eventType) => source.addEventListener(eventType, consume as EventListener));
  source.onerror = () => onError?.();
  return () => source.close();
}

class HttpIntegrationApi implements IntegrationApi {
  constructor(private readonly client: HttpClient) {}

  listIntegrations(): Promise<Integration[]> {
    return this.client.json("/v1/integrations");
  }

  createIntegration(input: CreateIntegrationInput): Promise<Integration> {
    return this.client.json("/v1/integrations", {
      method: "POST",
      headers: mutationHeaders(),
      body: JSON.stringify(input),
    });
  }

  listDevices(): Promise<Device[]> {
    return this.client.json("/v1/devices");
  }

  registerDevice(input: RegisterDeviceInput): Promise<Device> {
    return this.client.json("/v1/devices", {
      method: "POST",
      headers: mutationHeaders(),
      body: JSON.stringify(input),
    });
  }

  listDataSources(deviceId?: string): Promise<DataSource[]> {
    const query = deviceId ? `?${new URLSearchParams({ device_id: deviceId })}` : "";
    return this.client.json(`/v1/data-sources${query}`);
  }

  registerDataSource(input: RegisterDataSourceInput): Promise<DataSource> {
    return this.client.json("/v1/data-sources", {
      method: "POST",
      headers: mutationHeaders(),
      body: JSON.stringify(input),
    });
  }

  listTopics(dataSourceId: string): Promise<Topic[]> {
    return this.client.json(`/v1/data-sources/${encodeURIComponent(dataSourceId)}/topics`);
  }
}

class HttpProjectApi implements ProjectApi {
  constructor(private readonly client: HttpClient) {}

  listProjects(): Promise<Project[]> {
    return this.client.json("/v1/projects");
  }

  createProject(input: CreateProjectInput): Promise<Project> {
    return this.client.json("/v1/projects", {
      method: "POST",
      headers: mutationHeaders(),
      body: JSON.stringify(input),
    });
  }

  getWorkspace(projectId: string): Promise<WorkspaceSnapshot> {
    return this.client.json(`/v1/projects/${encodeURIComponent(projectId)}/workspace`);
  }

  assignDevice(projectId: string, input: AssignDeviceInput): Promise<DeviceAssignment> {
    return this.client.json(
      `/v1/projects/${encodeURIComponent(projectId)}/device-assignments`,
      {
        method: "POST",
        headers: mutationHeaders(),
        body: JSON.stringify(input),
      },
    );
  }

  assignDataSource(projectId: string, input: AssignDataSourceInput): Promise<DataAssignment> {
    return this.client.json(
      `/v1/projects/${encodeURIComponent(projectId)}/data-assignments`,
      {
        method: "POST",
        headers: mutationHeaders(),
        body: JSON.stringify(input),
      },
    );
  }

  subscribeWorkspace(
    projectId: string,
    onEvent: (event: WorkspaceEvent) => void,
    onError?: () => void,
  ): () => void {
    const source = this.client.eventSource(
      `/v1/projects/${encodeURIComponent(projectId)}/events`,
    );
    return subscribeJsonEvents(source, ["project.workspace.changed"], onEvent, onError);
  }
}

class HttpLiveApi implements LiveApi {
  constructor(private readonly client: HttpClient) {}

  createSession(input: CreateLiveSessionInput): Promise<LiveSession> {
    return this.client.json("/v1/live-sessions", {
      method: "POST",
      headers: mutationHeaders(),
      body: JSON.stringify(input),
    });
  }

  getSession(sessionId: string): Promise<LiveSession> {
    return this.client.json(`/v1/live-sessions/${encodeURIComponent(sessionId)}`);
  }

  closeSession(sessionId: string): Promise<Recording> {
    return this.client.json(`/v1/live-sessions/${encodeURIComponent(sessionId)}`, {
      method: "DELETE",
      headers: mutationHeaders(),
    });
  }

  subscribeEvents(
    sessionId: string,
    onEvent: (event: RmsEvent) => void,
    onError?: () => void,
  ): () => void {
    const source = this.client.eventSource(
      `/v1/live-sessions/${encodeURIComponent(sessionId)}/events`,
    );
    return subscribeJsonEvents(
      source,
      [
        "topic.value.changed",
        "device.state.changed",
        "control.lease.changed",
        "command.state.changed",
      ],
      onEvent,
      onError,
    );
  }
}

class HttpReplayApi implements ReplayApi {
  constructor(private readonly client: HttpClient) {}

  listRecordings(projectId: string): Promise<Recording[]> {
    return this.client.json(`/v1/projects/${encodeURIComponent(projectId)}/recordings`);
  }

  createSession(input: CreateReplaySessionInput): Promise<ReplaySession> {
    return this.client.json("/v1/replay-sessions", {
      method: "POST",
      headers: mutationHeaders(),
      body: JSON.stringify(input),
    });
  }

  getSession(sessionId: string): Promise<ReplaySession> {
    return this.client.json(`/v1/replay-sessions/${encodeURIComponent(sessionId)}`);
  }

  async closeSession(sessionId: string): Promise<void> {
    const response = await fetch(
      this.client.url(`/v1/replay-sessions/${encodeURIComponent(sessionId)}`),
      {
        method: "DELETE",
        credentials: "include",
        headers: mutationHeaders(),
      },
    );
    if (!response.ok) {
      throw new Error((await response.text()) || "Replay 세션을 종료하지 못했습니다.");
    }
  }
}

class HttpControlApi implements ControlApi {
  constructor(private readonly client: HttpClient) {}

  async getLease(liveSessionId: string): Promise<ControlLease | null> {
    const response = await fetch(
      this.client.url(`/v1/live-sessions/${encodeURIComponent(liveSessionId)}/control-state`),
      { credentials: "include" },
    );
    if (response.status === 404) {
      return null;
    }
    return (await parseJson<{ lease: ControlLease | null }>(response)).lease;
  }

  requestLease(liveSessionId: string, expectedDeviceVersion: number): Promise<ControlLease> {
    return this.client.json(
      `/v1/live-sessions/${encodeURIComponent(liveSessionId)}/control-leases`,
      {
        method: "POST",
        headers: mutationHeaders(),
        body: JSON.stringify({ scope: "motion", expectedDeviceVersion }),
      },
    );
  }

  async releaseLease(liveSessionId: string, leaseId: string): Promise<void> {
    const response = await fetch(
      this.client.url(
        `/v1/live-sessions/${encodeURIComponent(liveSessionId)}/control-leases/${encodeURIComponent(leaseId)}`,
      ),
      {
        method: "DELETE",
        credentials: "include",
        headers: mutationHeaders(),
      },
    );
    if (!response.ok) {
      throw new Error((await response.text()) || "제어권을 반납하지 못했습니다.");
    }
  }

  sendCommand(request: LiveControlCommandRequest): Promise<CommandReceipt> {
    return this.client.json(
      `/v1/live-sessions/${encodeURIComponent(request.liveSessionId)}/commands`,
      {
        method: "POST",
        headers: mutationHeaders(request.idempotencyKey),
        body: JSON.stringify(request),
      },
    );
  }
}

export class HttpRmsApi implements RmsApi {
  readonly integrations: IntegrationApi;
  readonly projects: ProjectApi;
  readonly live: LiveApi;
  readonly replay: ReplayApi;
  readonly control: ControlApi;

  constructor(baseUrl: string) {
    const client = new HttpClient(baseUrl);
    this.integrations = new HttpIntegrationApi(client);
    this.projects = new HttpProjectApi(client);
    this.live = new HttpLiveApi(client);
    this.replay = new HttpReplayApi(client);
    this.control = new HttpControlApi(client);
  }
}
