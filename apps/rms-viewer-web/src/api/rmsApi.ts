import type {
  ApproveNetworkCandidateInput,
  CandidateVerification,
  CommandReceipt,
  ControlLease,
  DataAssignment,
  DataSource,
  Device,
  DeviceAssignment,
  Integration,
  LiveControlCommandRequest,
  LiveSession,
  NetworkDiscoverySession,
  NetworkDiscoverySnapshot,
  NetworkLinkReceipt,
  Project,
  Recording,
  RecordingImport,
  RecordingImportFormat,
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
export interface CreateRecordingImportInput {
  projectId: string;
  deviceId: string;
  dataSourceId?: string;
  file: File;
  format: RecordingImportFormat;
  mapping?: Record<string, unknown>;
}

export interface RecordingImportRequestOptions {
  signal?: AbortSignal;
}

export interface RecordingImportUploadOptions extends RecordingImportRequestOptions {
  onProgress?: (progressPercent: number) => void;
}

export interface StartNetworkDiscoveryInput {
  organizationId: string;
}

export interface DiscoveryRequestOptions {
  signal?: AbortSignal;
}

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
  /** Returns `undefined` only when an ephemeral ReplaySession no longer exists. */
  getSession(sessionId: string): Promise<ReplaySession | undefined>;
  closeSession(sessionId: string): Promise<void>;
}

export interface RecordingImportApi {
  listImports(projectId?: string): Promise<RecordingImport[]>;
  createImport(
    input: CreateRecordingImportInput,
    options?: RecordingImportUploadOptions,
  ): Promise<RecordingImport>;
  getImport(
    importId: string,
    options?: RecordingImportRequestOptions,
  ): Promise<RecordingImport>;
  cancelImport(importId: string): Promise<void>;
}

/** Discovery is user initiated and only returns sanitized, short-lived candidates. */
export interface DiscoveryApi {
  start(
    input: StartNetworkDiscoveryInput,
    options?: DiscoveryRequestOptions,
  ): Promise<NetworkDiscoverySession>;
  getSnapshot(
    sessionId: string,
    options?: DiscoveryRequestOptions,
  ): Promise<NetworkDiscoverySnapshot>;
  cancel(sessionId: string): Promise<void>;
  verify(
    sessionId: string,
    candidateId: string,
    options?: DiscoveryRequestOptions,
  ): Promise<CandidateVerification>;
  approve(
    sessionId: string,
    candidateId: string,
    input: ApproveNetworkCandidateInput,
    options?: DiscoveryRequestOptions,
  ): Promise<NetworkLinkReceipt>;
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
  readonly recordingImports: RecordingImportApi;
  readonly discovery: DiscoveryApi;
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

  async optionalJson<T>(path: string, init?: RequestInit): Promise<T | undefined> {
    const response = await fetch(this.url(path), {
      credentials: "include",
      ...init,
    });
    if (response.status === 404) return undefined;
    return parseJson(response);
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

  getSession(sessionId: string): Promise<ReplaySession | undefined> {
    return this.client.optionalJson(`/v1/replay-sessions/${encodeURIComponent(sessionId)}`);
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
      console.error("Failed to close Replay session", response.status, await response.text());
      throw new Error("Replay 세션을 종료하지 못했습니다");
    }
  }
}

function uploadAbortError(): DOMException {
  return new DOMException("Recording import upload aborted", "AbortError");
}

class HttpRecordingImportApi implements RecordingImportApi {
  constructor(private readonly client: HttpClient) {}

  listImports(projectId?: string): Promise<RecordingImport[]> {
    const query = projectId ? `?${new URLSearchParams({ projectId })}` : "";
    return this.client.json(`/v1/recording-imports${query}`);
  }

  createImport(
    input: CreateRecordingImportInput,
    options: RecordingImportUploadOptions = {},
  ): Promise<RecordingImport> {
    return new Promise((resolve, reject) => {
      const xhr = new XMLHttpRequest();
      const form = new FormData();
      // The import service validates ownership and reserves capacity before it accepts file bytes.
      // Keep metadata ahead of the streaming file part so invalid targets fail without disk I/O.
      form.append("projectId", input.projectId);
      form.append("deviceId", input.deviceId);
      if (input.dataSourceId) form.append("dataSourceId", input.dataSourceId);
      form.append("format", input.format);
      if (input.mapping) form.append("mapping", JSON.stringify(input.mapping));
      form.append("file", input.file, input.file.name);

      let settled = false;
      const finish = (callback: () => void) => {
        if (settled) return;
        settled = true;
        options.signal?.removeEventListener("abort", onAbort);
        callback();
      };
      const onAbort = () => {
        xhr.abort();
        finish(() => reject(uploadAbortError()));
      };

      xhr.open("POST", this.client.url("/v1/recording-imports"));
      xhr.withCredentials = true;
      xhr.setRequestHeader("Idempotency-Key", crypto.randomUUID());
      xhr.setRequestHeader("X-RMS-Request-ID", crypto.randomUUID());
      xhr.upload.onprogress = (event) => {
        if (!event.lengthComputable || event.total <= 0) return;
        options.onProgress?.(
          Math.max(0, Math.min(100, Math.round((event.loaded / event.total) * 100))),
        );
      };
      xhr.onload = () => {
        if (xhr.status < 200 || xhr.status >= 300) {
          console.error("Recording import upload failed", xhr.status, xhr.responseText);
          finish(() => reject(new Error("파일을 가져오지 못했습니다")));
          return;
        }
        try {
          const recordingImport = JSON.parse(xhr.responseText) as RecordingImport;
          finish(() => resolve(recordingImport));
        } catch (cause: unknown) {
          console.error("Invalid recording import response", cause);
          finish(() => reject(new Error("파일을 가져오지 못했습니다")));
        }
      };
      xhr.onerror = () => {
        console.error("Recording import upload network failure");
        finish(() => reject(new Error("파일을 가져오지 못했습니다")));
      };
      xhr.onabort = () => finish(() => reject(uploadAbortError()));

      if (options.signal?.aborted) {
        onAbort();
        return;
      }
      options.signal?.addEventListener("abort", onAbort, { once: true });
      options.onProgress?.(0);
      xhr.send(form);
    });
  }

  getImport(
    importId: string,
    options: RecordingImportRequestOptions = {},
  ): Promise<RecordingImport> {
    return this.client.json(`/v1/recording-imports/${encodeURIComponent(importId)}`, {
      signal: options.signal,
    });
  }

  async cancelImport(importId: string): Promise<void> {
    const response = await fetch(
      this.client.url(`/v1/recording-imports/${encodeURIComponent(importId)}`),
      {
        method: "DELETE",
        credentials: "include",
        headers: mutationHeaders(),
      },
    );
    if (!response.ok) {
      console.error("Failed to cancel recording import", response.status, await response.text());
      throw new Error("가져오기를 취소하지 못했습니다");
    }
  }
}

class HttpDiscoveryApi implements DiscoveryApi {
  constructor(private readonly client: HttpClient) {}

  start(
    input: StartNetworkDiscoveryInput,
    options: DiscoveryRequestOptions = {},
  ): Promise<NetworkDiscoverySession> {
    return this.client.json("/v1/network-discovery-sessions", {
      method: "POST",
      headers: mutationHeaders(),
      body: JSON.stringify(input),
      signal: options.signal,
    });
  }

  getSnapshot(
    sessionId: string,
    options: DiscoveryRequestOptions = {},
  ): Promise<NetworkDiscoverySnapshot> {
    return this.client.json(
      `/v1/network-discovery-sessions/${encodeURIComponent(sessionId)}`,
      { signal: options.signal },
    );
  }

  async cancel(sessionId: string): Promise<void> {
    const response = await fetch(
      this.client.url(`/v1/network-discovery-sessions/${encodeURIComponent(sessionId)}`),
      {
        method: "DELETE",
        credentials: "include",
        headers: mutationHeaders(),
      },
    );
    if (!response.ok) {
      console.error("Failed to cancel network discovery", response.status, await response.text());
      throw new Error("네트워크 검색을 취소하지 못했습니다");
    }
  }

  verify(
    sessionId: string,
    candidateId: string,
    options: DiscoveryRequestOptions = {},
  ): Promise<CandidateVerification> {
    return this.client.json(
      `/v1/network-discovery-sessions/${encodeURIComponent(sessionId)}/candidates/${encodeURIComponent(candidateId)}/verification`,
      {
        method: "POST",
        headers: mutationHeaders(),
        signal: options.signal,
      },
    );
  }

  approve(
    sessionId: string,
    candidateId: string,
    input: ApproveNetworkCandidateInput,
    options: DiscoveryRequestOptions = {},
  ): Promise<NetworkLinkReceipt> {
    return this.client.json(
      `/v1/network-discovery-sessions/${encodeURIComponent(sessionId)}/candidates/${encodeURIComponent(candidateId)}/approval`,
      {
        method: "POST",
        headers: mutationHeaders(),
        body: JSON.stringify(input),
        signal: options.signal,
      },
    );
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
      console.error("Failed to release control lease", response.status, await response.text());
      throw new Error("제어권을 반납하지 못했습니다");
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
  readonly recordingImports: RecordingImportApi;
  readonly discovery: DiscoveryApi;
  readonly control: ControlApi;

  constructor(baseUrl: string) {
    const client = new HttpClient(baseUrl);
    this.integrations = new HttpIntegrationApi(client);
    this.projects = new HttpProjectApi(client);
    this.live = new HttpLiveApi(client);
    this.replay = new HttpReplayApi(client);
    this.recordingImports = new HttpRecordingImportApi(client);
    this.discovery = new HttpDiscoveryApi(client);
    this.control = new HttpControlApi(client);
  }
}
