import { describe, expect, it } from "vitest";
import {
  OPERATOR_ID,
  deriveControlEligibility,
  type ControlEligibilityInput,
  type ControlLease,
  type Device,
  type ViewerDataSource,
} from "./domain";

const device: Device = {
  id: "robot-07",
  organizationId: "organization-1",
  integrationId: "integration-1",
  name: "Robot-07",
  kind: "robot",
  status: "online",
  health: "normal",
  operationMode: "자율 운행",
  batteryPercent: 80,
  taskName: "이동",
  taskProgress: 30,
  lastSeenAt: "2026-08-20T00:00:00Z",
  stateVersion: 3,
};

const source: ViewerDataSource = {
  id: "live",
  deviceId: device.id,
  name: "실시간",
  kind: "live",
  status: "recording",
  rrdUrl: "https://example.test/live.rrd",
  capturedAt: "2026-08-20T00:00:00Z",
  topicIds: [],
};

const lease: ControlLease = {
  id: "lease-1",
  liveSessionId: "live-session-1",
  deviceId: device.id,
  holderId: OPERATOR_ID,
  holderName: "나",
  expiresAt: new Date(Date.now() + 120_000).toISOString(),
  epoch: 1,
};

function input(overrides: Partial<ControlEligibilityInput> = {}): ControlEligibilityInput {
  return {
    device,
    source,
    mode: "live",
    lease,
    operatorId: OPERATOR_ID,
    viewerReady: true,
    ...overrides,
  };
}

describe("RMS domain boundaries", () => {
  it("keeps integration resources independent from project ownership", () => {
    expect(device).not.toHaveProperty("projectId");
  });
});

describe("deriveControlEligibility", () => {
  it("allows control only for the current live lease holder", () => {
    expect(deriveControlEligibility(input())).toEqual({
      allowed: true,
      reason: "제어 가능",
    });
  });

  it("fails closed in replay", () => {
    expect(
      deriveControlEligibility(
        input({ source: { kind: "recording" }, mode: "replay" }),
      ),
    ).toEqual({
      allowed: false,
      reason: "Replay에서는 제어할 수 없습니다.",
    });
  });

  it("fails closed while the live viewer is paused", () => {
    expect(deriveControlEligibility(input({ mode: "paused" })).allowed).toBe(false);
  });

  it("requires a lease and a ready viewer", () => {
    expect(deriveControlEligibility(input({ lease: null })).reason).toBe("제어권이 필요합니다.");
    expect(deriveControlEligibility(input({ viewerReady: false })).allowed).toBe(false);
  });

  it("rejects an expired lease", () => {
    expect(
      deriveControlEligibility(
        input({ lease: { ...lease, expiresAt: new Date(Date.now() - 1_000).toISOString() } }),
      ).reason,
    ).toBe("제어권이 만료되었습니다.");
  });
});
