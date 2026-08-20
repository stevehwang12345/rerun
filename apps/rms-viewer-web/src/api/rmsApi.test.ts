import { afterEach, describe, expect, it, vi } from "vitest";
import type {
  CommandReceipt,
  ControlLease,
  LiveSession,
  ReplaySession,
  WorkspaceSnapshot,
} from "../domain";
import { HttpRmsApi } from "./rmsApi";

afterEach(() => {
  vi.unstubAllGlobals();
});

describe("HttpRmsApi service contracts", () => {
  it("loads a project workspace as one versioned snapshot", async () => {
    const snapshot = { snapshotVersion: 17 } as WorkspaceSnapshot;
    const fetchMock = vi.fn(async () => jsonResponse(snapshot));
    vi.stubGlobal("fetch", fetchMock);

    const api = new HttpRmsApi("/api/");
    expect(await api.projects.getWorkspace("project 1")).toEqual(snapshot);
    const [url] = fetchMock.mock.calls[0] as unknown as [string];
    expect(url).toBe("/api/v1/projects/project%201/workspace");
  });

  it("creates Live sessions through the Live boundary", async () => {
    const session = {
      id: "live-1",
      projectId: "project-1",
      deviceId: "robot-07",
      dataSourceId: "source-1",
    } as LiveSession;
    const fetchMock = vi.fn(async () => jsonResponse(session, 201));
    vi.stubGlobal("fetch", fetchMock);

    const api = new HttpRmsApi("/api");
    const input = {
      projectId: "project-1",
      deviceId: "robot-07",
      dataSourceId: "source-1",
      openedBy: "operator-01",
    };
    await api.live.createSession(input);

    const [url, init] = fetchMock.mock.calls[0] as unknown as [string, RequestInit];
    expect(url).toBe("/api/v1/live-sessions");
    expect(init.method).toBe("POST");
    expect(JSON.parse(String(init.body))).toEqual(input);
  });

  it("scopes lease requests to a Live session with state and idempotency guards", async () => {
    const lease: ControlLease = {
      id: "lease-1",
      liveSessionId: "live-1",
      deviceId: "robot-07",
      holderId: "operator-01",
      holderName: "나",
      expiresAt: new Date(Date.now() + 60_000).toISOString(),
      epoch: 4,
    };
    const fetchMock = vi.fn(async () => jsonResponse(lease));
    vi.stubGlobal("fetch", fetchMock);

    const api = new HttpRmsApi("/api/");
    await api.control.requestLease("live-1", 142);

    const [url, init] = fetchMock.mock.calls[0] as unknown as [string, RequestInit];
    const headers = init.headers as Record<string, string>;
    expect(url).toBe("/api/v1/live-sessions/live-1/control-leases");
    expect(init.method).toBe("POST");
    expect(headers["Idempotency-Key"]).toBeTruthy();
    expect(headers["X-RMS-Request-ID"]).toBeTruthy();
    expect(JSON.parse(String(init.body))).toEqual({
      scope: "motion",
      expectedDeviceVersion: 142,
    });
  });

  it("preserves the Live session command safety envelope", async () => {
    const receipt: CommandReceipt = {
      commandId: "cmd-1",
      liveSessionId: "live-1",
      commandType: "safe_stop",
      status: "accepted",
      message: "요청을 받았습니다.",
      createdAt: new Date().toISOString(),
    };
    const fetchMock = vi.fn(async () => jsonResponse(receipt, 202));
    vi.stubGlobal("fetch", fetchMock);

    const api = new HttpRmsApi("/api");
    const request = {
      liveSessionId: "live-1",
      deviceId: "robot-07",
      commandType: "safe_stop",
      expectedDeviceVersion: 142,
      idempotencyKey: "idem-1",
      leaseId: "lease-1",
      leaseEpoch: 4,
      sessionMode: "live" as const,
      issuedAt: new Date().toISOString(),
      expiresAt: new Date(Date.now() + 3_000).toISOString(),
    };
    await api.control.sendCommand(request);

    const [url, init] = fetchMock.mock.calls[0] as unknown as [string, RequestInit];
    const headers = init.headers as Record<string, string>;
    expect(url).toBe("/api/v1/live-sessions/live-1/commands");
    expect(headers["Idempotency-Key"]).toBe(request.idempotencyKey);
    expect(headers["X-RMS-Request-ID"]).toBeTruthy();
    expect(JSON.parse(String(init.body))).toEqual(request);
  });

  it("creates Replay sessions on a separate command-free API", async () => {
    const session = {
      id: "replay-1",
      projectId: "project-1",
      recordingId: "recording-1",
    } as ReplaySession;
    const fetchMock = vi.fn(async () => jsonResponse(session, 201));
    vi.stubGlobal("fetch", fetchMock);

    const api = new HttpRmsApi("/api");
    await api.replay.createSession({
      projectId: "project-1",
      recordingId: "recording-1",
      openedBy: "operator-01",
    });

    const [url] = fetchMock.mock.calls[0] as unknown as [string, RequestInit];
    expect(url).toBe("/api/v1/replay-sessions");
    expect("sendCommand" in api.replay).toBe(false);
    expect("requestLease" in api.replay).toBe(false);
  });
});

function jsonResponse(value: unknown, status = 200): Response {
  return new Response(JSON.stringify(value), {
    status,
    headers: { "Content-Type": "application/json" },
  });
}
