import { describe, expect, it, vi } from "vitest";
import type { LiveControlCommandRequest } from "../domain";
import { MockRmsApi } from "./mockRmsApi";

async function provisionWorkspace(accessMode: "control" | "observe" = "control") {
  const api = new MockRmsApi({ seed: false });
  const integration = await api.integrations.createIntegration({
    organizationId: "organization-test",
    name: "Test ROS 2",
    kind: "ros2",
    endpointLabel: "Test Edge",
  });
  const device = await api.integrations.registerDevice({
    id: "robot-test",
    organizationId: "organization-test",
    integrationId: integration.id,
    name: "Robot Test",
    kind: "robot",
    status: "online",
    health: "normal",
    operationMode: "대기",
    batteryPercent: 90,
    taskName: "할당 없음",
    taskProgress: 0,
    lastSeenAt: new Date().toISOString(),
  });
  const source = await api.integrations.registerDataSource({
    id: "source-test",
    integrationId: integration.id,
    deviceId: device.id,
    name: "Robot Telemetry",
    protocol: "ROS 2",
    status: "recording",
    liveUrl: "https://example.test/live.rrd",
    topicIds: ["pose", "velocity", "battery"],
    lastDataAt: new Date().toISOString(),
  });
  const project = await api.projects.createProject({
    organizationId: "organization-test",
    name: "Test Project",
    description: "Service boundary test",
    status: "active",
  });
  await api.projects.assignDevice(project.id, { deviceId: device.id, accessMode });
  await api.projects.assignDataSource(project.id, {
    dataSourceId: source.id,
    visibility: "operator",
  });
  return { api, integration, device, source, project };
}

describe("MockRmsApi integrated service flow", () => {
  it("provisions Connect resources and binds them through assignment records", async () => {
    const { api, device, source, project } = await provisionWorkspace();
    const workspace = await api.projects.getWorkspace(project.id);

    expect(device).not.toHaveProperty("projectId");
    expect(source).not.toHaveProperty("projectId");
    expect(workspace.project.deviceCount).toBe(1);
    expect(workspace.devices.map((item) => item.id)).toEqual([device.id]);
    expect(workspace.dataSources.map((item) => item.id)).toEqual([source.id]);
    expect(workspace.deviceAssignments[0]).toMatchObject({
      projectId: project.id,
      deviceId: device.id,
      accessMode: "control",
    });
    expect(workspace.dataAssignments[0]).toMatchObject({
      projectId: project.id,
      dataSourceId: source.id,
    });
    expect(workspace.topicsByDataSource[source.id]).toHaveLength(3);
    expect(
      [...workspace.deviceAssignments, ...workspace.dataAssignments].every(
        (assignment) => assignment.resourceVersion <= workspace.snapshotVersion,
      ),
    ).toBe(true);
  });

  it("emits revisioned workspace events after resource assignments", async () => {
    const api = new MockRmsApi({ seed: false });
    const integration = await api.integrations.createIntegration({
      organizationId: "organization-test",
      name: "Test",
      kind: "rerun",
      endpointLabel: "Local",
    });
    const device = await api.integrations.registerDevice({
      id: "device-events",
      organizationId: "organization-test",
      integrationId: integration.id,
      name: "Event Device",
      kind: "robot",
      status: "online",
      health: "normal",
      operationMode: "대기",
      taskName: "없음",
      taskProgress: 0,
      lastSeenAt: new Date().toISOString(),
    });
    const source = await api.integrations.registerDataSource({
      id: "source-events",
      integrationId: integration.id,
      deviceId: device.id,
      name: "Event Source",
      protocol: "Rerun",
      status: "recording",
      liveUrl: "https://example.test/live.rrd",
      topicIds: [],
      lastDataAt: new Date().toISOString(),
    });
    const project = await api.projects.createProject({
      organizationId: "organization-test",
      name: "Event Project",
      description: "Events",
      status: "active",
    });
    const versions: number[] = [];
    const unsubscribe = api.projects.subscribeWorkspace(project.id, (event) =>
      versions.push(event.snapshotVersion),
    );

    await api.projects.assignDevice(project.id, { deviceId: device.id, accessMode: "control" });
    await api.projects.assignDataSource(project.id, {
      dataSourceId: source.id,
      visibility: "operator",
    });
    unsubscribe();

    expect(versions).toHaveLength(2);
    expect(versions[1]).toBeGreaterThan(versions[0] ?? 0);
    expect((await api.projects.getWorkspace(project.id)).snapshotVersion).toBe(versions[1]);
  });

  it("opens Live, scopes lease and command to that session, then creates Replay", async () => {
    const { api, device, source, project } = await provisionWorkspace();
    const liveSession = await api.live.createSession({
      projectId: project.id,
      deviceId: device.id,
      dataSourceId: source.id,
      openedBy: "operator-01",
    });
    const lease = await api.control.requestLease(liveSession.id, device.stateVersion);
    const command: LiveControlCommandRequest = {
      liveSessionId: liveSession.id,
      deviceId: device.id,
      commandType: "pause_mission",
      expectedDeviceVersion: device.stateVersion,
      idempotencyKey: "integrated-command",
      leaseId: lease.id,
      leaseEpoch: lease.epoch,
      sessionMode: "live",
      issuedAt: new Date().toISOString(),
      expiresAt: new Date(Date.now() + 3_000).toISOString(),
    };
    const receipt = await api.control.sendCommand(command);

    expect(lease.liveSessionId).toBe(liveSession.id);
    expect(receipt.liveSessionId).toBe(liveSession.id);
    expect(receipt.message).toContain("일시정지");

    const recording = await api.live.closeSession(liveSession.id);
    expect(recording.projectSnapshot).toMatchObject({
      projectId: project.id,
      projectName: project.name,
    });
    expect(recording).toMatchObject({
      defaultTimeline: "tick",
      durationSeconds: 50,
      rrdVersion: "0.36.1",
      footerVerified: true,
    });
    expect(recording.contentSha256).toMatch(/^[0-9a-f]{64}$/);
    expect(recording.timelines).toEqual([
      {
        name: "tick",
        kind: "sequence",
        start: "0",
        end: "100",
        durationSeconds: 50,
        fps: 2,
      },
    ]);
    expect(await api.control.getLease(liveSession.id)).toBeNull();

    const replaySession = await api.replay.createSession({
      projectId: project.id,
      recordingId: recording.id,
      openedBy: "operator-01",
    });
    expect(replaySession.streamUrl).toBe(recording.rrdUrl);
    expect(replaySession).toMatchObject({
      cursorSeconds: 0,
      initialTimeline: "tick",
      initialCursor: { kind: "sequence", value: "0" },
      initialPlayState: "paused",
      initialSpeed: 1,
      initialLoop: { mode: "off" },
    });
    expect("sendCommand" in api.replay).toBe(false);
    expect("requestLease" in api.replay).toBe(false);
  });

  it("does not accept a lease from a different Live session", async () => {
    const { api, device, source, project } = await provisionWorkspace();
    const first = await api.live.createSession({
      projectId: project.id,
      deviceId: device.id,
      dataSourceId: source.id,
      openedBy: "operator-01",
    });
    const second = await api.live.createSession({
      projectId: project.id,
      deviceId: device.id,
      dataSourceId: source.id,
      openedBy: "operator-01",
    });
    const lease = await api.control.requestLease(first.id, device.stateVersion);

    await expect(
      api.control.requestLease(second.id, device.stateVersion),
    ).rejects.toThrow("제어 중");

    await expect(
      api.control.sendCommand({
        liveSessionId: second.id,
        deviceId: device.id,
        commandType: "safe_stop",
        expectedDeviceVersion: device.stateVersion,
        idempotencyKey: "cross-session",
        leaseId: lease.id,
        leaseEpoch: lease.epoch,
        sessionMode: "live",
        issuedAt: new Date().toISOString(),
        expiresAt: new Date(Date.now() + 3_000).toISOString(),
      }),
    ).rejects.toThrow("유효한 제어권");
  });

  it("does not issue control leases to observe-only assignments", async () => {
    const { api, device, source, project } = await provisionWorkspace("observe");
    const session = await api.live.createSession({
      projectId: project.id,
      deviceId: device.id,
      dataSourceId: source.id,
      openedBy: "operator-01",
    });

    await expect(api.control.requestLease(session.id, device.stateVersion)).rejects.toThrow(
      "관찰만",
    );
  });

  it("streams topic events only through an open Live session", async () => {
    vi.useFakeTimers();
    try {
      const { api, device, source, project } = await provisionWorkspace();
      const session = await api.live.createSession({
        projectId: project.id,
        deviceId: device.id,
        dataSourceId: source.id,
        openedBy: "operator-01",
      });
      const events: string[] = [];
      const unsubscribe = api.live.subscribeEvents(session.id, (event) =>
        events.push(`${event.liveSessionId}:${event.type}`),
      );

      await vi.advanceTimersByTimeAsync(1_200);
      unsubscribe();

      expect(events).toEqual([`${session.id}:topic.value.changed`]);
    } finally {
      vi.useRealTimers();
    }
  });

  it("prevents two projects from owning control of one device", async () => {
    const { api, device } = await provisionWorkspace();
    const second = await api.projects.createProject({
      organizationId: "organization-test",
      name: "Second Project",
      description: "Conflict",
      status: "active",
    });

    await expect(
      api.projects.assignDevice(second.id, { deviceId: device.id, accessMode: "control" }),
    ).rejects.toThrow("동시에 두 프로젝트");
  });

  it("discovers and verifies without registering anything, then links once as observe", async () => {
    const api = new MockRmsApi();
    const projectId = "project-logistics";
    const beforeIntegrations = await api.integrations.listIntegrations();
    const beforeDevices = await api.integrations.listDevices();
    const beforeSources = await api.integrations.listDataSources();
    const beforeWorkspace = await api.projects.getWorkspace(projectId);

    const session = await api.discovery.start({ organizationId: "organization-rms" });
    expect(session).toMatchObject({ status: "searching", candidateCount: 0 });
    const snapshot = await api.discovery.getSnapshot(session.id);
    expect(snapshot.session.status).toBe("ready");
    expect(snapshot.candidates.length).toBeGreaterThan(0);
    const candidateKeys = new Set(snapshot.candidates.flatMap((candidate) => Object.keys(candidate)));
    expect(candidateKeys).not.toContain("liveUrl");
    expect(candidateKeys).not.toContain("endpoint");
    expect(candidateKeys).not.toContain("protocol");
    expect(candidateKeys).not.toContain("mappingVersion");
    expect(candidateKeys).not.toContain("ip");
    expect(candidateKeys).not.toContain("port");

    expect(await api.integrations.listIntegrations()).toHaveLength(beforeIntegrations.length);
    expect(await api.integrations.listDevices()).toHaveLength(beforeDevices.length);
    expect(await api.integrations.listDataSources()).toHaveLength(beforeSources.length);

    const candidate = snapshot.candidates[0]!;
    const verification = await api.discovery.verify(session.id, candidate.id);
    expect(verification.status).toBe("verified");
    expect(await api.integrations.listDevices()).toHaveLength(beforeDevices.length);

    const input = {
      verificationToken: verification.verificationToken,
      projectId,
      expectedWorkspaceVersion: beforeWorkspace.snapshotVersion,
      deviceName: verification.suggestedDevice.name,
      selectedSourceIds: [verification.sources[0]!.id],
      accessMode: "observe" as const,
      visibility: "operator" as const,
    };
    const receipt = await api.discovery.approve(session.id, candidate.id, input);
    const retried = await api.discovery.approve(session.id, candidate.id, input);

    expect(retried).toEqual(receipt);
    expect(await api.integrations.listIntegrations()).toHaveLength(beforeIntegrations.length + 1);
    const devices = await api.integrations.listDevices();
    const sources = await api.integrations.listDataSources(receipt.deviceId);
    const workspace = await api.projects.getWorkspace(projectId);
    expect(devices.find((device) => device.id === receipt.deviceId)).toMatchObject({
      health: "unknown",
      kind: "robot",
    });
    expect(sources).toHaveLength(1);
    expect(sources[0]).toMatchObject({ status: "pending" });
    expect(
      workspace.deviceAssignments.find(
        (assignment) => assignment.deviceId === receipt.deviceId,
      ),
    ).toMatchObject({ accessMode: "observe" });
    expect(
      workspace.dataAssignments.find(
        (assignment) => assignment.dataSourceId === receipt.dataSourceIds[0],
      ),
    ).toMatchObject({ visibility: "operator" });
    expect(await api.control.getLease("not-a-live-session")).toBeNull();
  });

  it("rejects cancelled, expired, and stale discovery approvals", async () => {
    vi.useFakeTimers();
    vi.setSystemTime(new Date("2026-08-21T00:00:00Z"));
    try {
      const api = new MockRmsApi();
      const cancelled = await api.discovery.start({ organizationId: "organization-rms" });
      const cancelledSnapshot = await api.discovery.getSnapshot(cancelled.id);
      await api.discovery.cancel(cancelled.id);
      await expect(
        api.discovery.verify(cancelled.id, cancelledSnapshot.candidates[0]!.id),
      ).rejects.toThrow();

      const session = await api.discovery.start({ organizationId: "organization-rms" });
      const snapshot = await api.discovery.getSnapshot(session.id);
      const verification = await api.discovery.verify(session.id, snapshot.candidates[0]!.id);
      const workspace = await api.projects.getWorkspace("project-logistics");
      const input = {
        verificationToken: verification.verificationToken,
        projectId: "project-logistics",
        expectedWorkspaceVersion: workspace.snapshotVersion + 1,
        deviceName: "Robot-Stale",
        selectedSourceIds: [verification.sources[0]!.id],
        accessMode: "observe" as const,
        visibility: "operator" as const,
      };
      await expect(
        api.discovery.approve(session.id, snapshot.candidates[0]!.id, input),
      ).rejects.toThrow("변경");

      await vi.advanceTimersByTimeAsync(121_000);
      await expect(
        api.discovery.approve(session.id, snapshot.candidates[0]!.id, {
          ...input,
          expectedWorkspaceVersion: workspace.snapshotVersion,
        }),
      ).rejects.toThrow("지났습니다");
    } finally {
      vi.useRealTimers();
    }
  });
});
