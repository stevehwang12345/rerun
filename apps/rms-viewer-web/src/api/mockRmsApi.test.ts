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
    expect(await api.control.getLease(liveSession.id)).toBeNull();

    const replaySession = await api.replay.createSession({
      projectId: project.id,
      recordingId: recording.id,
      openedBy: "operator-01",
    });
    expect(replaySession.streamUrl).toBe(recording.rrdUrl);
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
});
