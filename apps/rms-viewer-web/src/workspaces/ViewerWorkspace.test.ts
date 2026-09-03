import { afterEach, describe, expect, it, vi } from "vitest";

import type { RmsApi } from "../api";
import type {
  DataSource,
  Device,
  LiveSession,
  Recording,
  ReplaySession,
  Topic,
  WorkspaceSnapshot,
} from "../domain";
import { replayPath } from "../routes";
import {
  VIEWER_FAILURE_MESSAGE,
  commandIdempotencyKey,
  createViewerLifecycleGate,
  isLiveControlContextEnabled,
  isMatchingLiveSource,
  replaceWithCanonicalSessionRoute,
  replayPlaybackContext,
  replayTopics,
  resolveSession,
} from "./ViewerWorkspace";

afterEach(() => {
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

const source: DataSource = {
  id: "source-front",
  integrationId: "integration-1",
  deviceId: "robot-07",
  name: "Front",
  protocol: "rerun",
  status: "recording",
  liveUrl: "/rerun/live/source-front",
  topicIds: ["pose"],
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

const topic: Topic = {
  id: "pose",
  dataSourceId: source.id,
  deviceId: device.id,
  path: "/localization/pose",
  label: "Pose",
  renderer: "spatial",
  quality: "fresh",
  value: "정상",
  updatedAt: "2026-08-20T00:00:00Z",
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
  topicsByRecording: {},
  topicsByDataSource: { [source.id]: [topic] },
} satisfies WorkspaceSnapshot;

const recording: Recording = {
  id: "recording-1",
  organizationId: "org-rms",
  projectId: liveSession.projectId,
  deviceId: device.id,
  dataSourceId: source.id,
  name: "Incident",
  status: "ready",
  rrdUrl: "/rerun/recordings/recording-1",
  capturedAt: "2026-08-20T00:00:00Z",
  durationLabel: "00:50",
  timelines: [
    {
      name: "tick",
      kind: "sequence",
      start: "0",
      end: "100",
      durationSeconds: 50,
      fps: 2,
    },
  ],
  defaultTimeline: "tick",
  durationSeconds: 50,
  rrdVersion: "0.36.1",
  footerVerified: true,
  contentSha256: "a".repeat(64),
  topicIds: source.topicIds,
  mappingVersion: 1,
  projectSnapshot: {
    projectId: liveSession.projectId,
    projectName: "Project",
    capturedAt: "2026-08-20T00:00:00Z",
    deviceAssignmentId: "device-assignment-1",
    dataAssignmentId: "data-assignment-1",
  },
  resourceVersion: 7,
};

const replaySession: ReplaySession = {
  id: "replay-1",
  projectId: liveSession.projectId,
  recordingId: recording.id,
  deviceId: device.id,
  openedBy: "operator-01",
  status: "open",
  streamUrl: recording.rrdUrl,
  cursorSeconds: 0,
  initialTimeline: "tick",
  initialCursor: { kind: "sequence", value: "20" },
  initialPlayState: "playing",
  initialSpeed: 2,
  initialLoop: {
    mode: "selection",
    start: { kind: "sequence", value: "10" },
    end: { kind: "sequence", value: "40" },
  },
  openedAt: "2026-08-20T00:00:00Z",
  resourceVersion: 7,
};

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
    expect(isLiveControlContextEnabled(workspace, device, liveSession, [topic])).toBe(true);
    expect(
      isLiveControlContextEnabled(
        { ...workspace, deviceAssignments: [] },
        device,
        liveSession,
        [topic],
      ),
    ).toBe(false);
    expect(
      isLiveControlContextEnabled(
        workspace,
        { ...device, health: "critical" },
        liveSession,
        [topic],
      ),
    ).toBe(false);
    expect(
      isLiveControlContextEnabled(
        workspace,
        device,
        { ...liveSession, openedBy: "operator-02" },
        [topic],
      ),
    ).toBe(false);
  });

  it("fails closed when the source or a required Topic is not fresh", () => {
    expect(
      isLiveControlContextEnabled(
        { ...workspace, dataSources: [{ ...source, status: "degraded" }] },
        device,
        liveSession,
        [topic],
      ),
    ).toBe(false);
    expect(
      isLiveControlContextEnabled(
        { ...workspace, dataSources: [{ ...source, status: "ready" }] },
        device,
        liveSession,
        [topic],
      ),
    ).toBe(false);
    expect(
      isLiveControlContextEnabled(
        workspace,
        device,
        { ...liveSession, sourceHealth: "delayed" },
        [topic],
      ),
    ).toBe(false);
    expect(
      isLiveControlContextEnabled(
        workspace,
        device,
        liveSession,
        [{ ...topic, quality: "unavailable" }],
      ),
    ).toBe(false);
    expect(isLiveControlContextEnabled(workspace, device, liveSession, [])).toBe(false);
    expect(
      isLiveControlContextEnabled(
        { ...workspace, dataSources: [] },
        device,
        liveSession,
        [topic],
      ),
    ).toBe(false);
  });
});

describe("Viewer stale Replay session recovery", () => {
  it("creates exactly one replacement and canonicalizes the stale route", async () => {
    const replayWorkspace: WorkspaceSnapshot = {
      ...workspace,
      recordings: [recording],
      topicsByRecording: { [recording.id]: [topic] },
    };
    const getSession = vi.fn(async () => undefined);
    const replacement = { ...replaySession, id: "replay-replacement" };
    const createSession = vi.fn(async () => replacement);
    const api = {
      projects: { getWorkspace: vi.fn(async () => replayWorkspace) },
      integrations: { listDevices: vi.fn(async () => [device]) },
      replay: { getSession, createSession },
    } as unknown as RmsApi;
    const route = {
      kind: "replay",
      projectId: recording.projectId,
      recordingId: recording.id,
      replaySessionId: "replay-stale",
    } as const;

    const resolved = await resolveSession(api, route);

    expect(getSession).toHaveBeenCalledTimes(1);
    expect(getSession).toHaveBeenCalledWith("replay-stale");
    expect(createSession).toHaveBeenCalledTimes(1);
    expect(createSession).toHaveBeenCalledWith({
      projectId: recording.projectId,
      recordingId: recording.id,
      openedBy: "operator-01",
    });
    expect(resolved.kind).toBe("replay");
    expect(resolved.session.id).toBe(replacement.id);

    const stalePath = replayPath(recording.projectId, recording.id, "replay-stale");
    const canonicalPath = replayPath(recording.projectId, recording.id, replacement.id);
    const replaceState = vi.fn();
    vi.stubGlobal("window", {
      location: { pathname: stalePath, search: "" },
      history: { replaceState },
    });

    expect(replaceWithCanonicalSessionRoute(route, resolved)).toBe(canonicalPath);
    expect(replaceState).toHaveBeenCalledTimes(1);
    expect(replaceState).toHaveBeenCalledWith(null, "", canonicalPath);
  });

  it("propagates non-404 lookup failures without creating a replacement", async () => {
    const replayWorkspace: WorkspaceSnapshot = {
      ...workspace,
      recordings: [recording],
      topicsByRecording: { [recording.id]: [topic] },
    };
    const getSession = vi.fn(async () => {
      throw new Error("service unavailable");
    });
    const createSession = vi.fn(async () => replaySession);
    const api = {
      projects: { getWorkspace: vi.fn(async () => replayWorkspace) },
      integrations: { listDevices: vi.fn(async () => [device]) },
      replay: { getSession, createSession },
    } as unknown as RmsApi;

    await expect(
      resolveSession(api, {
        kind: "replay",
        projectId: recording.projectId,
        recordingId: recording.id,
        replaySessionId: "replay-stale",
      }),
    ).rejects.toThrow("service unavailable");
    expect(createSession).not.toHaveBeenCalled();
  });
});

describe("Viewer Replay playback context", () => {
  it("uses the first Recording's immutable Topics when a shared DataSource has newer Topics", () => {
    const firstTopic = { ...topic, id: "robot-camera", path: "/robot/camera" };
    const latestSourceTopic = {
      ...topic,
      id: "zenodo-lidar",
      path: "/scan_raw",
      renderer: "spatial" as const,
    };
    const firstRecording = {
      ...recording,
      id: "recording-robotis",
      topicIds: [firstTopic.id],
    };
    const secondRecording = {
      ...recording,
      id: "recording-zenodo",
      topicIds: [latestSourceTopic.id],
    };
    const sharedSourceWorkspace: WorkspaceSnapshot = {
      ...workspace,
      recordings: [firstRecording, secondRecording],
      topicsByRecording: {
        [firstRecording.id]: [firstTopic],
        [secondRecording.id]: [latestSourceTopic],
      },
      topicsByDataSource: {
        [source.id]: [latestSourceTopic],
      },
    };

    expect(replayTopics(sharedSourceWorkspace, firstRecording)).toEqual([firstTopic]);
    expect(replayTopics(sharedSourceWorkspace, firstRecording)).not.toEqual(
      sharedSourceWorkspace.topicsByDataSource[source.id],
    );
  });

  it("fails closed instead of falling back when a Recording snapshot is absent", () => {
    expect(() =>
      replayTopics(
        {
          ...workspace,
          recordings: [recording],
          topicsByRecording: {},
          topicsByDataSource: { [source.id]: [topic] },
        },
        recording,
      ),
    ).toThrow(`Recording Topic snapshot mismatch: ${recording.id}`);
  });

  it("fails closed when the Recording snapshot has source or identity drift", () => {
    expect(() =>
      replayTopics(
        {
          ...workspace,
          recordings: [recording],
          topicsByRecording: {
            [recording.id]: [{ ...topic, dataSourceId: "newer-shared-source" }],
          },
        },
        { ...recording, topicIds: [topic.id] },
      ),
    ).toThrow(`Recording Topic snapshot mismatch: ${recording.id}`);
  });

  it("passes the server timeline, cursor, play, speed, and loop policy losslessly", () => {
    expect(replayPlaybackContext(recording, replaySession)).toEqual({
      initial_timeline: "tick",
      initial_cursor: { kind: "sequence", value: "20" },
      initial_play_state: "playing",
      initial_fps: 2,
      initial_speed: 2,
      initial_loop: {
        mode: "selection",
        start: { kind: "sequence", value: "10" },
        end: { kind: "sequence", value: "40" },
      },
    });
  });

  it("uses the Recording start and fail-safe paused, 1x, loop-off defaults", () => {
    expect(replayPlaybackContext(recording, { cursorSeconds: 2 })).toEqual({
      initial_timeline: "tick",
      initial_cursor: {
        kind: "sequence",
        value: "4",
      },
      initial_play_state: "paused",
      initial_fps: 2,
      initial_speed: 1,
      initial_loop: { mode: "off" },
    });
  });

  it("keeps duration nanoseconds as a lossless decimal cursor", () => {
    expect(
      replayPlaybackContext(
        {
          ...recording,
          timelines: [
            {
              name: "video_time",
              kind: "duration",
              start: "0",
              end: "9223372036854775800",
              durationSeconds: 9_223_372_036.854776,
              fps: null,
            },
          ],
          defaultTimeline: "video_time",
        },
        {
          cursorSeconds: 0,
          initialTimeline: "video_time",
          initialCursor: { kind: "duration", value: "9223372036854775799" },
        },
      ),
    ).toEqual({
      initial_timeline: "video_time",
      initial_cursor: { kind: "duration", value: "9223372036854775799" },
      initial_fps: null,
      initial_play_state: "paused",
      initial_speed: 1,
      initial_loop: { mode: "off" },
    });
  });

  it("does not expose a runtime error string as the operator message", () => {
    expect(VIEWER_FAILURE_MESSAGE).toBe("화면을 불러오지 못했습니다");
    expect(VIEWER_FAILURE_MESSAGE).not.toContain("WebAssembly");
  });
});

describe("Viewer command idempotency", () => {
  it("is stable for a duplicate runtime event and unique across runtime instances", () => {
    const first = commandIdempotencyKey("runtime-a", "request-7");

    expect(commandIdempotencyKey("runtime-a", "request-7")).toBe(first);
    expect(commandIdempotencyKey("runtime-b", "request-7")).not.toBe(first);
  });
});
