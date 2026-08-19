import type {
  CommandReceipt,
  ControlCommandRequest,
  ControlLease,
  DataSource,
  Device,
  Project,
  RmsEvent,
  Topic,
} from "../domain";

export interface RmsApi {
  listProjects(): Promise<Project[]>;
  listDevices(projectId: string): Promise<Device[]>;
  listDataSources(projectId: string, deviceId: string): Promise<DataSource[]>;
  listTopics(deviceId: string, dataSourceId: string): Promise<Topic[]>;
  getControlLease(deviceId: string): Promise<ControlLease | null>;
  requestControlLease(deviceId: string, expectedDeviceVersion: number): Promise<ControlLease>;
  releaseControlLease(deviceId: string, leaseId: string): Promise<void>;
  sendControlCommand(request: ControlCommandRequest): Promise<CommandReceipt>;
  subscribeEvents(
    projectId: string,
    deviceId: string,
    dataSourceId: string,
    onEvent: (event: RmsEvent) => void,
    onError?: () => void,
  ): () => void;
}

async function parseJson<T>(response: Response): Promise<T> {
  if (!response.ok) {
    const body = await response.text();
    throw new Error(body || `요청을 처리하지 못했습니다. (${response.status})`);
  }
  return (await response.json()) as T;
}

export class HttpRmsApi implements RmsApi {
  public constructor(private readonly baseUrl: string) {}

  private url(path: string): string {
    return `${this.baseUrl.replace(/\/$/, "")}${path}`;
  }

  async listProjects(): Promise<Project[]> {
    return parseJson(await fetch(this.url("/v1/projects"), { credentials: "include" }));
  }

  async listDevices(projectId: string): Promise<Device[]> {
    return parseJson(
      await fetch(this.url(`/v1/projects/${encodeURIComponent(projectId)}/devices`), {
        credentials: "include",
      }),
    );
  }

  async listDataSources(projectId: string, deviceId: string): Promise<DataSource[]> {
    const query = new URLSearchParams({ device_id: deviceId });
    return parseJson(
      await fetch(
        this.url(`/v1/projects/${encodeURIComponent(projectId)}/data-sources?${query}`),
        { credentials: "include" },
      ),
    );
  }

  async listTopics(deviceId: string, dataSourceId: string): Promise<Topic[]> {
    const query = new URLSearchParams({ data_source_id: dataSourceId });
    return parseJson(
      await fetch(this.url(`/v1/devices/${encodeURIComponent(deviceId)}/topics?${query}`), {
        credentials: "include",
      }),
    );
  }

  async getControlLease(deviceId: string): Promise<ControlLease | null> {
    const response = await fetch(
      this.url(`/v1/devices/${encodeURIComponent(deviceId)}/control-state`),
      { credentials: "include" },
    );
    if (response.status === 404) {
      return null;
    }
    const state = await parseJson<{ lease: ControlLease | null }>(response);
    return state.lease;
  }

  async requestControlLease(
    deviceId: string,
    expectedDeviceVersion: number,
  ): Promise<ControlLease> {
    const requestId = crypto.randomUUID();
    const idempotencyKey = crypto.randomUUID();
    return parseJson(
      await fetch(this.url(`/v1/devices/${encodeURIComponent(deviceId)}/control-leases`), {
        method: "POST",
        credentials: "include",
        headers: {
          "Content-Type": "application/json",
          "Idempotency-Key": idempotencyKey,
          "X-RMS-Request-ID": requestId,
        },
        body: JSON.stringify({ scope: "motion", expectedDeviceVersion }),
      }),
    );
  }

  async releaseControlLease(_deviceId: string, leaseId: string): Promise<void> {
    const requestId = crypto.randomUUID();
    const idempotencyKey = crypto.randomUUID();
    const response = await fetch(
      this.url(`/v1/control-leases/${encodeURIComponent(leaseId)}`),
      {
        method: "DELETE",
        credentials: "include",
        headers: {
          "Idempotency-Key": idempotencyKey,
          "X-RMS-Request-ID": requestId,
        },
      },
    );
    if (!response.ok) {
      throw new Error((await response.text()) || "제어권을 반납하지 못했습니다.");
    }
  }

  async sendControlCommand(request: ControlCommandRequest): Promise<CommandReceipt> {
    const requestId = crypto.randomUUID();
    return parseJson(
      await fetch(this.url(`/v1/devices/${encodeURIComponent(request.deviceId)}/commands`), {
        method: "POST",
        credentials: "include",
        headers: {
          "Content-Type": "application/json",
          "Idempotency-Key": request.idempotencyKey,
          "X-RMS-Request-ID": requestId,
        },
        body: JSON.stringify(request),
      }),
    );
  }

  subscribeEvents(
    projectId: string,
    deviceId: string,
    dataSourceId: string,
    onEvent: (event: RmsEvent) => void,
    onError?: () => void,
  ): () => void {
    const query = new URLSearchParams({
      project_id: projectId,
      device_id: deviceId,
      data_source_id: dataSourceId,
    });
    const source = new EventSource(this.url(`/v1/events?${query}`), {
      withCredentials: true,
    });
    const consume = (message: MessageEvent<string>) => {
      try {
        onEvent(JSON.parse(message.data) as RmsEvent);
      } catch {
        onError?.();
      }
    };
    source.onmessage = consume;
    const eventTypes: RmsEvent["type"][] = [
      "topic.value.changed",
      "device.state.changed",
      "control.lease.changed",
      "command.state.changed",
    ];
    eventTypes.forEach((eventType) => source.addEventListener(eventType, consume as EventListener));
    source.onerror = () => onError?.();
    return () => source.close();
  }
}
