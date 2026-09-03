import { afterEach, describe, expect, it, vi } from "vitest";
import type {
  CandidateVerification,
  CommandReceipt,
  ControlLease,
  LiveSession,
  NetworkDiscoverySession,
  NetworkLinkReceipt,
  ReplaySession,
  WorkspaceSnapshot,
} from "../domain";
import { HttpRmsApi } from "./rmsApi";

afterEach(() => {
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
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
    const session: ReplaySession = {
      id: "replay-1",
      projectId: "project-1",
      recordingId: "recording-1",
      deviceId: "robot-07",
      openedBy: "operator-01",
      status: "open",
      streamUrl: "/rerun/recordings/recording-1",
      cursorSeconds: 0,
      initialTimeline: "tick",
      initialCursor: { kind: "sequence", value: "0" },
      initialPlayState: "paused",
      initialSpeed: 1,
      initialLoop: { mode: "off" },
      openedAt: "2026-08-20T00:00:00Z",
      resourceVersion: 17,
    };
    const fetchMock = vi.fn(async () => jsonResponse(session, 201));
    vi.stubGlobal("fetch", fetchMock);

    const api = new HttpRmsApi("/api");
    const created = await api.replay.createSession({
      projectId: "project-1",
      recordingId: "recording-1",
      openedBy: "operator-01",
    });

    const [url] = fetchMock.mock.calls[0] as unknown as [string, RequestInit];
    expect(url).toBe("/api/v1/replay-sessions");
    expect(created).toMatchObject({
      initialTimeline: "tick",
      initialCursor: { kind: "sequence", value: "0" },
      initialPlayState: "paused",
      initialSpeed: 1,
      initialLoop: { mode: "off" },
    });
    expect("sendCommand" in api.replay).toBe(false);
    expect("requestLease" in api.replay).toBe(false);
  });

  it("treats only a missing ephemeral Replay session as absent", async () => {
    const fetchMock = vi.fn(async () => jsonResponse({ error: "missing" }, 404));
    vi.stubGlobal("fetch", fetchMock);

    const api = new HttpRmsApi("/api");
    await expect(api.replay.getSession("stale/replay")).resolves.toBeUndefined();

    const [url] = fetchMock.mock.calls[0] as unknown as [string];
    expect(url).toBe("/api/v1/replay-sessions/stale%2Freplay");
  });

  it("does not hide non-404 Replay session lookup failures", async () => {
    vi.spyOn(console, "error").mockImplementation(() => undefined);
    vi.stubGlobal("fetch", vi.fn(async () => jsonResponse({ error: "unavailable" }, 503)));

    const api = new HttpRmsApi("/api");
    await expect(api.replay.getSession("replay-1")).rejects.toThrow(
      "요청을 처리하지 못했습니다",
    );
  });

  it("starts discovery only through the explicit network discovery boundary", async () => {
    const session: NetworkDiscoverySession = {
      id: "discovery 1",
      status: "searching",
      candidateCount: 0,
      startedAt: "2026-08-21T00:00:00Z",
      expiresAt: "2026-08-21T00:02:00Z",
      resourceVersion: 1,
    };
    const fetchMock = vi.fn(async () => jsonResponse(session, 202));
    vi.stubGlobal("fetch", fetchMock);

    const api = new HttpRmsApi("/api/");
    await api.discovery.start({ organizationId: "org-rms" });

    const [url, init] = fetchMock.mock.calls[0] as unknown as [string, RequestInit];
    const headers = init.headers as Record<string, string>;
    expect(url).toBe("/api/v1/network-discovery-sessions");
    expect(init.method).toBe("POST");
    expect(JSON.parse(String(init.body))).toEqual({ organizationId: "org-rms" });
    expect(headers["Idempotency-Key"]).toBeTruthy();
    expect(headers["X-RMS-Request-ID"]).toBeTruthy();
  });

  it("verifies and approves only an opaque discovery candidate", async () => {
    const verification: CandidateVerification = {
      verificationToken: "verification-token",
      candidateId: "candidate/1",
      status: "verified",
      suggestedDevice: { name: "Robot-24", kind: "robot" },
      sources: [
        { id: "source-1", label: "위치와 주변", category: "spatial", status: "ready" },
      ],
      expiresAt: "2026-08-21T00:01:00Z",
    };
    const receipt: NetworkLinkReceipt = {
      status: "linked",
      projectId: "project-1",
      integrationId: "integration-1",
      deviceId: "device-1",
      dataSourceIds: ["data-source-1"],
      workspaceVersion: 18,
    };
    const fetchMock = vi
      .fn()
      .mockResolvedValueOnce(jsonResponse(verification))
      .mockResolvedValueOnce(jsonResponse(receipt, 201));
    vi.stubGlobal("fetch", fetchMock);

    const api = new HttpRmsApi("/api");
    await api.discovery.verify("session 1", "candidate/1");
    const input = {
      verificationToken: verification.verificationToken,
      projectId: "project-1",
      expectedWorkspaceVersion: 17,
      deviceName: "Robot-24",
      selectedSourceIds: ["source-1"],
      accessMode: "observe" as const,
      visibility: "operator" as const,
    };
    await api.discovery.approve("session 1", "candidate/1", input);

    const [verifyUrl, verifyInit] = fetchMock.mock.calls[0] as unknown as [
      string,
      RequestInit,
    ];
    expect(verifyUrl).toBe(
      "/api/v1/network-discovery-sessions/session%201/candidates/candidate%2F1/verification",
    );
    expect(verifyInit.method).toBe("POST");
    expect(verifyInit.body).toBeUndefined();
    const [approvalUrl, approvalInit] = fetchMock.mock.calls[1] as unknown as [
      string,
      RequestInit,
    ];
    expect(approvalUrl).toBe(
      "/api/v1/network-discovery-sessions/session%201/candidates/candidate%2F1/approval",
    );
    expect(JSON.parse(String(approvalInit.body))).toEqual(input);
  });

  it("cancels an active discovery session without exposing a target address", async () => {
    const fetchMock = vi.fn(async () => new Response(null, { status: 204 }));
    vi.stubGlobal("fetch", fetchMock);

    const api = new HttpRmsApi("/api");
    await api.discovery.cancel("discovery/1");

    const [url, init] = fetchMock.mock.calls[0] as unknown as [string, RequestInit];
    expect(url).toBe("/api/v1/network-discovery-sessions/discovery%2F1");
    expect(init.method).toBe("DELETE");
  });
});

function jsonResponse(value: unknown, status = 200): Response {
  return new Response(JSON.stringify(value), {
    status,
    headers: { "Content-Type": "application/json" },
  });
}
