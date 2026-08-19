import { afterEach, describe, expect, it, vi } from "vitest";
import type { CommandReceipt, ControlLease } from "../domain";
import { HttpRmsApi } from "./rmsApi";

afterEach(() => {
  vi.unstubAllGlobals();
});

describe("HttpRmsApi backend contract", () => {
  it("sends control lease requests with state and idempotency guards", async () => {
    const lease: ControlLease = {
      id: "lease-1",
      deviceId: "robot-07",
      holderId: "operator-01",
      holderName: "나",
      expiresAt: new Date(Date.now() + 60_000).toISOString(),
      epoch: 4,
    };
    const fetchMock = vi.fn(async () => jsonResponse(lease));
    vi.stubGlobal("fetch", fetchMock);

    const api = new HttpRmsApi("/api/");
    await api.requestControlLease("robot-07", 142);

    const [url, init] = fetchMock.mock.calls[0] as unknown as [string, RequestInit];
    const headers = init.headers as Record<string, string>;
    expect(url).toBe("/api/v1/devices/robot-07/control-leases");
    expect(init.method).toBe("POST");
    expect(headers["Idempotency-Key"]).toBeTruthy();
    expect(headers["X-RMS-Request-ID"]).toBeTruthy();
    expect(JSON.parse(String(init.body))).toEqual({
      scope: "motion",
      expectedDeviceVersion: 142,
    });
  });

  it("preserves the command safety envelope and request headers", async () => {
    const receipt: CommandReceipt = {
      commandId: "cmd-1",
      commandType: "safe_stop",
      status: "accepted",
      message: "요청을 받았습니다.",
      createdAt: new Date().toISOString(),
    };
    const fetchMock = vi.fn(async () => jsonResponse(receipt, 202));
    vi.stubGlobal("fetch", fetchMock);

    const api = new HttpRmsApi("/api");
    const request = {
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
    await api.sendControlCommand(request);

    const [url, init] = fetchMock.mock.calls[0] as unknown as [string, RequestInit];
    const headers = init.headers as Record<string, string>;
    expect(url).toBe("/api/v1/devices/robot-07/commands");
    expect(headers["Idempotency-Key"]).toBe(request.idempotencyKey);
    expect(headers["X-RMS-Request-ID"]).toBeTruthy();
    expect(JSON.parse(String(init.body))).toEqual(request);
  });
});

function jsonResponse(value: unknown, status = 200): Response {
  return new Response(JSON.stringify(value), {
    status,
    headers: { "Content-Type": "application/json" },
  });
}
