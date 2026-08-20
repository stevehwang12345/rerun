import { describe, expect, it } from "vitest";

import type {
  DataSource,
  Device,
  LiveSession,
  WorkspaceSnapshot,
} from "../domain";
import {
  commandIdempotencyKey,
  createViewerLifecycleGate,
  isLiveControlContextEnabled,
  isMatchingLiveSource,
} from "./ViewerWorkspace";

const source: DataSource = {
  id: "source-front",
  integrationId: "integration-1",
  deviceId: "robot-07",
  name: "Front",
  protocol: "rerun",
  status: "recording",
  liveUrl: "/rerun/live/source-front",
  topicIds: [],
  mappingVersion: 1,
  lastDataAt: "2026-08-20T00:00:00Z",
};

const device: Device = {
  id: "robot-07",
  organizationId: "org-rms",
  integrationId: "integration-1",
  name: "Robot-07",
  kind: "robot",
  status: "online",
  health: "normal",
  operationMode: "운영",
  taskName: "이동",
  taskProgress: 10,
  lastSeenAt: "2026-08-20T00:00:00Z",
  stateVersion: 7,
};

const liveSession: LiveSession = {
  id: "live-1",
  projectId: "project-1",
  deviceId: device.id,
  dataSourceId: source.id,
  openedBy: "operator-01",
  status: "open",
  playState: "following",
  sourceHealth: "fresh",
  streamUrl: source.liveUrl,
  startedAt: "2026-08-20T00:00:00Z",
  resourceVersion: 7,
};

const workspace = {
  snapshotVersion: 7,
  capturedAt: "2026-08-20T00:00:00Z",
  project: {
    id: liveSession.projectId,
    organizationId: "org-rms",
    name: "Project",
    description: "",
    status: "active",
    deviceCount: 1,
    onlineDeviceCount: 1,
    createdAt: "2026-08-20T00:00:00Z",
    resourceVersion: 7,
  },
  deviceAssignments: [
    {
      id: "device-assignment-1",
      projectId: liveSession.projectId,
      deviceId: device.id,
      accessMode: "control",
      validFrom: "2026-08-20T00:00:00Z",
      resourceVersion: 7,
    },
  ],
  dataAssignments: [
    {
      id: "data-assignment-1",
      projectId: liveSession.projectId,
      dataSourceId: source.id,
      visibility: "operator",
      validFrom: "2026-08-20T00:00:00Z",
      resourceVersion: 7,
    },
  ],
  devices: [device],
  dataSources: [source],
  recordings: [],
  topicsByDataSource: { [source.id]: [] },
} satisfies WorkspaceSnapshot;

describe("Viewer live safety gate", () => {
  it("rejects control and live updates as soon as a transition begins", () => {
    const gate = createViewerLifecycleGate(true);

    expect(gate.canProcessLive()).toBe(true);
    expect(gate.beginTransition()).toBe(true);
    expect(gate.canProcessLive()).toBe(false);
    expect(gate.beginTransition()).toBe(false);
  });

  it("latches an SSE failure and never re-enables live processing", () => {
    const gate = createViewerLifecycleGate(true);

    expect(gate.failClosed()).toBe(true);
    expect(gate.hasFailed()).toBe(true);
    expect(gate.canProcessLive()).toBe(false);
    expect(gate.failClosed()).toBe(false);
    expect(gate.canProcessLive()).toBe(false);
  });

  it("never enables control for a Replay runtime", () => {
    expect(createViewerLifecycleGate(false).canProcessLive()).toBe(false);
  });
});

describe("Viewer session identity", () => {
  it("requires the route, Live session, Device, and DataSource to agree", () => {
    expect(isMatchingLiveSource(source, "robot-07", source.id, source.id)).toBe(true);
    expect(isMatchingLiveSource(source, "robot-08", source.id, source.id)).toBe(false);
    expect(isMatchingLiveSource(source, "robot-07", "source-rear", source.id)).toBe(false);
    expect(isMatchingLiveSource(source, "robot-07", source.id, "source-rear")).toBe(false);
  });

  it("removes control capability when assignment or device safety changes", () => {
    expect(isLiveControlContextEnabled(workspace, device, liveSession)).toBe(true);
    expect(
      isLiveControlContextEnabled(
        { ...workspace, deviceAssignments: [] },
        device,
        liveSession,
      ),
    ).toBe(false);
    expect(
      isLiveControlContextEnabled(
        workspace,
        { ...device, health: "critical" },
        liveSession,
      ),
    ).toBe(false);
    expect(
      isLiveControlContextEnabled(
        workspace,
        device,
        { ...liveSession, openedBy: "operator-02" },
      ),
    ).toBe(false);
  });
});

describe("Viewer command idempotency", () => {
  it("is stable for a duplicate runtime event and unique across runtime instances", () => {
    const first = commandIdempotencyKey("runtime-a", "request-7");

    expect(commandIdempotencyKey("runtime-a", "request-7")).toBe(first);
    expect(commandIdempotencyKey("runtime-b", "request-7")).not.toBe(first);
  });
});
