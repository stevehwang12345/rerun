import { useEffect, useRef, useState } from "react";

import type { RmsApi } from "../api";
import { OPERATOR_ID, compactTimestamp } from "../domain";
import type {
  DataSource,
  Device,
  LiveSession,
  Recording,
  ReplaySession,
  RmsEvent,
  Topic,
  WorkspaceSnapshot,
} from "../domain";
import { livePath, projectsPath, replayPath, type RmsRoute } from "../routes";

const RUNTIME_MODULE_URL = "/runtime/rms_product_app.js";
const RUNTIME_WASM_URL = "/runtime/rms_product_app_bg.wasm";

interface RmsTopicContext {
  label: string;
  path: string;
  renderer: string;
  value?: string;
}

interface LiveViewerContext {
  project_id: string;
  project_name: string;
  device_id: string;
  device_name: string;
  device_status: string;
  device_health: string;
  device_state_version: number;
  live_session_id: string;
  data_source_id: string;
  source_name: string;
  source_url: string;
  operator_id: string;
  control_enabled: boolean;
  topics: RmsTopicContext[];
}

interface ReplayViewerContext {
  project_id: string;
  project_name: string;
  device_id: string;
  device_name: string;
  recording_id: string;
  recording_name: string;
  replay_session_id: string;
  source_url: string;
  captured_at_label?: string;
  topics: RmsTopicContext[];
}

type RmsHostEvent =
  | { type: "select_device"; device_id: string }
  | {
      type: "open_replay";
      project_id: string;
      device_id: string;
      live_session_id: string;
    }
  | { type: "open_live"; project_id: string; device_id: string };

type RmsControlEvent =
  | {
      type: "request_control_lease";
      request_id: string;
      device_id: string;
      expected_device_version: number;
    }
  | {
      type: "release_control_lease";
      request_id: string;
      device_id: string;
      lease_id: string;
      lease_epoch: number;
    }
  | {
      type: "send_control_command";
      request_id: string;
      device_id: string;
      command_type: string;
      expected_device_version: number;
      lease_id: string;
      lease_epoch: number;
      session_mode: string;
    };

type RmsProductEvent = RmsHostEvent | RmsControlEvent;

type RmsControlResponse =
  | {
      type: "lease_granted";
      request_id: string;
      device_id: string;
      lease_id: string;
      lease_epoch: number;
      holder_id: string;
      holder_name: string;
      expires_at_ms: number;
    }
  | {
      type: "lease_released";
      request_id: string;
      device_id: string;
      lease_id: string;
      lease_epoch: number;
    }
  | {
      type: "command_updated";
      request_id: string;
      device_id: string;
      message: string;
    }
  | {
      type: "failed";
      request_id: string;
      device_id: string;
      message: string;
    };

interface RmsWebHandle {
  start(canvas: HTMLCanvasElement): Promise<void>;
  set_event_callback(callback?: (event: RmsProductEvent) => void): void;
  apply_live_context(context: LiveViewerContext): void;
  apply_replay_context(context: ReplayViewerContext): void;
  apply_viewer_error(message: string): void;
  apply_control_response(response: RmsControlResponse): void;
  prepare_stop(): boolean;
  stop(): void;
}

interface RmsProductRuntimeModule {
  default(input: { module_or_path: URL }): Promise<unknown>;
  RmsWebHandle: new () => RmsWebHandle;
}

interface ResolvedLive {
  kind: "live";
  workspace: WorkspaceSnapshot;
  device: Device;
  session: LiveSession;
  topics: Topic[];
  sourceName: string;
}

interface ResolvedReplay {
  kind: "replay";
  workspace: WorkspaceSnapshot;
  device: Device;
  session: ReplaySession;
  recording: Recording;
  topics: Topic[];
}

type ResolvedSession = ResolvedLive | ResolvedReplay;

let runtimeModulePromise: Promise<RmsProductRuntimeModule> | undefined;

function loadRuntime(): Promise<RmsProductRuntimeModule> {
  runtimeModulePromise ??= import(
    /* @vite-ignore */ RUNTIME_MODULE_URL
  ).then(async (loadedModule) => {
    const runtime = loadedModule as unknown as RmsProductRuntimeModule;
    await runtime.default({
      module_or_path: new URL(RUNTIME_WASM_URL, window.location.href),
    });
    return runtime;
  });
  return runtimeModulePromise;
}

function topicContexts(topics: Topic[]): RmsTopicContext[] {
  return topics.map((topic) => ({
    label: topic.label,
    path: topic.path,
    renderer: topic.renderer,
    value: topic.value ?? topic.message,
  }));
}

function absoluteSourceUrl(sourceUrl: string): string {
  return new URL(sourceUrl, window.location.href).href;
}

function isControlEvent(event: RmsProductEvent): event is RmsControlEvent {
  return (
    event.type === "request_control_lease" ||
    event.type === "release_control_lease" ||
    event.type === "send_control_command"
  );
}

function isAvailableLiveSource(source: WorkspaceSnapshot["dataSources"][number]): boolean {
  return (
    (source.status === "ready" || source.status === "recording") &&
    source.liveUrl.trim().length > 0
  );
}

export function isMatchingLiveSource(
  source: DataSource,
  deviceId: string,
  sessionDataSourceId?: string,
  requestedDataSourceId?: string,
): boolean {
  return (
    source.deviceId === deviceId &&
    isAvailableLiveSource(source) &&
    (sessionDataSourceId == null || source.id === sessionDataSourceId) &&
    (requestedDataSourceId == null || source.id === requestedDataSourceId)
  );
}

export interface ViewerLifecycleGate {
  beginTransition(): boolean;
  failClosed(): boolean;
  canProcessLive(): boolean;
  hasFailed(): boolean;
}

export function createViewerLifecycleGate(live: boolean): ViewerLifecycleGate {
  let transitionPending = false;
  let failedClosed = false;

  return {
    beginTransition() {
      if (transitionPending || failedClosed) return false;
      transitionPending = true;
      return true;
    },
    failClosed() {
      if (failedClosed) return false;
      failedClosed = true;
      return true;
    },
    canProcessLive() {
      return live && !transitionPending && !failedClosed;
    },
    hasFailed() {
      return failedClosed;
    },
  };
}

export function commandIdempotencyKey(runtimeNonce: string, requestId: string): string {
  return `${runtimeNonce}:${requestId}`;
}

export function isLiveControlContextEnabled(
  workspace: WorkspaceSnapshot,
  device: Device,
  session: LiveSession,
): boolean {
  const deviceAssignment = workspace.deviceAssignments.find(
    (assignment) =>
      assignment.deviceId === device.id &&
      assignment.accessMode === "control" &&
      assignment.validTo == null,
  );
  const dataAssignment = workspace.dataAssignments.find(
    (assignment) =>
      assignment.dataSourceId === session.dataSourceId &&
      assignment.visibility === "operator" &&
      assignment.validTo == null,
  );

  return (
    workspace.project.status === "active" &&
    deviceAssignment != null &&
    dataAssignment != null &&
    device.status === "online" &&
    device.health !== "restricted" &&
    device.health !== "critical" &&
    session.status === "open" &&
    session.playState === "following" &&
    session.openedBy === OPERATOR_ID
  );
}

async function resolveSession(api: RmsApi, route: Extract<RmsRoute, { kind: "live" | "replay" }>): Promise<ResolvedSession> {
  const workspace = await api.projects.getWorkspace(route.projectId);

  if (route.kind === "live") {
    const device = workspace.devices.find((candidate) => candidate.id === route.deviceId);
    if (!device) {
      throw new Error("프로젝트에 연결된 장비가 아닙니다");
    }
    let session = route.liveSessionId
      ? await api.live.getSession(route.liveSessionId)
      : undefined;
    if (session?.status === "closed") {
      session = undefined;
    }
    const source = workspace.dataSources.find((candidate) =>
      isMatchingLiveSource(
        candidate,
        device.id,
        session?.dataSourceId,
        route.dataSourceId,
      ),
    );
    if (!source || !isAvailableLiveSource(source)) {
      throw new Error("사용할 수 있는 실시간 데이터가 없습니다");
    }
    session ??= await api.live.createSession({
          projectId: route.projectId,
          deviceId: device.id,
          dataSourceId: source.id,
          openedBy: OPERATOR_ID,
        });
    if (
      session.projectId !== route.projectId ||
      session.deviceId !== device.id ||
      session.dataSourceId !== source.id ||
      session.status !== "open"
    ) {
      throw new Error("실시간 세션 정보가 현재 프로젝트와 일치하지 않습니다");
    }
    return {
      kind: "live",
      workspace,
      device,
      session,
      topics: workspace.topicsByDataSource[source.id] ?? [],
      sourceName: source.name,
    };
  }

  const recording = workspace.recordings.find(
    (candidate) => candidate.id === route.recordingId && candidate.status === "ready",
  );
  if (!recording) {
    throw new Error("재생할 수 있는 기록을 찾지 못했습니다");
  }
  const device =
    workspace.devices.find((candidate) => candidate.id === recording.deviceId) ??
    (await api.integrations.listDevices()).find(
      (candidate) => candidate.id === recording.deviceId,
    );
  if (!device) {
    throw new Error("기록의 장비 정보를 찾지 못했습니다");
  }
  let session = route.replaySessionId
    ? await api.replay.getSession(route.replaySessionId)
    : undefined;
  if (session?.status === "closed") {
    session = undefined;
  }
  session ??= await api.replay.createSession({
        projectId: route.projectId,
        recordingId: recording.id,
        openedBy: OPERATOR_ID,
      });
  if (
    session.projectId !== route.projectId ||
    session.recordingId !== recording.id ||
    session.deviceId !== device.id ||
    session.status !== "open"
  ) {
    throw new Error("Replay 세션 정보가 현재 프로젝트와 일치하지 않습니다");
  }
  return {
    kind: "replay",
    workspace,
    device,
    session,
    recording,
    topics:
      workspace.topicsByDataSource[recording.dataSourceId] ??
      (await api.integrations.listTopics(recording.dataSourceId)),
  };
}

function applyResolvedSession(
  handle: RmsWebHandle,
  resolved: ResolvedSession,
  liveControlRequested = true,
): void {
  if (resolved.kind === "live") {
    handle.apply_live_context({
      project_id: resolved.workspace.project.id,
      project_name: resolved.workspace.project.name,
      device_id: resolved.device.id,
      device_name: resolved.device.name,
      device_status: resolved.device.status,
      device_health: resolved.device.health,
      device_state_version: resolved.device.stateVersion,
      live_session_id: resolved.session.id,
      data_source_id: resolved.session.dataSourceId,
      source_name: resolved.sourceName,
      source_url: absoluteSourceUrl(resolved.session.streamUrl),
      operator_id: OPERATOR_ID,
      control_enabled:
        liveControlRequested &&
        isLiveControlContextEnabled(resolved.workspace, resolved.device, resolved.session),
      topics: topicContexts(resolved.topics),
    });
    return;
  }

  handle.apply_replay_context({
    project_id: resolved.workspace.project.id,
    project_name: resolved.workspace.project.name,
    device_id: resolved.device.id,
    device_name: resolved.device.name,
    recording_id: resolved.recording.id,
    recording_name: resolved.recording.name,
    replay_session_id: resolved.session.id,
    source_url: absoluteSourceUrl(resolved.session.streamUrl),
    captured_at_label: compactTimestamp(resolved.recording.capturedAt),
    topics: topicContexts(resolved.topics),
  });
}

async function resolveControlEvent(
  api: RmsApi,
  session: ResolvedLive,
  event: RmsControlEvent,
  runtimeNonce: string,
): Promise<RmsControlResponse> {
  if (event.device_id !== session.device.id || session.session.status !== "open") {
    return {
      type: "failed",
      request_id: event.request_id,
      device_id: event.device_id,
      message: "현재 실시간 세션에서 처리할 수 없는 요청입니다",
    };
  }

  try {
    if (event.type === "request_control_lease") {
      const lease = await api.control.requestLease(
        session.session.id,
        event.expected_device_version,
      );
      const expiresAtMs = Date.parse(lease.expiresAt);
      if (
        lease.liveSessionId !== session.session.id ||
        lease.deviceId !== session.device.id ||
        lease.holderId !== OPERATOR_ID ||
        !Number.isFinite(expiresAtMs) ||
        expiresAtMs <= Date.now()
      ) {
        await api.control.releaseLease(session.session.id, lease.id).catch(() => undefined);
        throw new Error("유효한 제어권을 확인하지 못했습니다");
      }
      return {
        type: "lease_granted",
        request_id: event.request_id,
        device_id: lease.deviceId,
        lease_id: lease.id,
        lease_epoch: lease.epoch,
        holder_id: lease.holderId,
        holder_name: lease.holderName,
        expires_at_ms: expiresAtMs,
      };
    }

    if (event.type === "release_control_lease") {
      await api.control.releaseLease(session.session.id, event.lease_id);
      return {
        type: "lease_released",
        request_id: event.request_id,
        device_id: event.device_id,
        lease_id: event.lease_id,
        lease_epoch: event.lease_epoch,
      };
    }

    const issuedAt = new Date();
    const receipt = await api.control.sendCommand({
      liveSessionId: session.session.id,
      deviceId: session.device.id,
      commandType: event.command_type,
      expectedDeviceVersion: event.expected_device_version,
      idempotencyKey: commandIdempotencyKey(runtimeNonce, event.request_id),
      leaseId: event.lease_id,
      leaseEpoch: event.lease_epoch,
      sessionMode: "live",
      issuedAt: issuedAt.toISOString(),
      expiresAt: new Date(issuedAt.getTime() + 5_000).toISOString(),
    });
    return {
      type: "command_updated",
      request_id: event.request_id,
      device_id: session.device.id,
      message: receipt.message,
    };
  } catch (cause: unknown) {
    return {
      type: "failed",
      request_id: event.request_id,
      device_id: event.device_id,
      message: cause instanceof Error ? cause.message : "요청을 처리하지 못했습니다",
    };
  }
}

export function ViewerWorkspace({
  api,
  route,
  onNavigate,
}: {
  api: RmsApi;
  route: Extract<RmsRoute, { kind: "live" | "replay" }>;
  onNavigate: (path: string, replace?: boolean) => void;
}) {
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const [status, setStatus] = useState<"loading" | "running" | "error">("loading");
  const [message, setMessage] = useState("Viewer를 준비하고 있습니다");

  useEffect(() => {
    const canvas = canvasRef.current;
    if (!canvas) {
      setStatus("error");
      setMessage("Viewer를 시작하지 못했습니다");
      return;
    }

    let disposed = false;
    let handle: RmsWebHandle | undefined;
    let resolved: ResolvedSession | undefined;
    let unsubscribeEvents: (() => void) | undefined;
    let unsubscribeWorkspace: (() => void) | undefined;
    let controlQueue = Promise.resolve();
    let sessionCloseRequested = false;
    let activeLease: { id: string; epoch: number } | undefined;
    let heartbeatTimer: ReturnType<typeof setTimeout> | undefined;
    let shuttingDown = false;
    let stopped = false;
    let runtimeStarted = false;
    const lifecycleGate = createViewerLifecycleGate(route.kind === "live");
    const runtimeNonce = crypto.randomUUID();

    const releaseActiveLease = (live: ResolvedLive | undefined) => {
      const lease = activeLease;
      activeLease = undefined;
      if (live && lease) {
        void api.control.releaseLease(live.session.id, lease.id).catch(() => undefined);
      }
    };

    const prepareStop = () => {
      const currentHandle = handle;
      if (!shuttingDown || stopped || !runtimeStarted || !currentHandle) return;

      if (!currentHandle.prepare_stop()) return;

      releaseActiveLease(resolved?.kind === "live" ? resolved : undefined);
      currentHandle.set_event_callback();
      currentHandle.stop();
      stopped = true;
    };

    const deliverControlResponse = (response: RmsControlResponse): boolean => {
      const currentHandle = handle;
      if (!currentHandle || stopped || (disposed && !shuttingDown)) return false;

      currentHandle.apply_control_response(response);
      prepareStop();
      return true;
    };

    const revokeViewerControl = (live: ResolvedLive | undefined, viewerError?: string) => {
      const currentHandle = handle;
      if (!live || !currentHandle || stopped) {
        releaseActiveLease(live);
        return;
      }

      queueMicrotask(() => {
        if (stopped) return;
        try {
          applyResolvedSession(currentHandle, live, false);
          if (viewerError) currentHandle.apply_viewer_error(viewerError);
        } catch {
          releaseActiveLease(live);
        }
      });
    };

    const fail = (cause: unknown) => {
      if (!lifecycleGate.failClosed()) return;
      const nextMessage = cause instanceof Error ? cause.message : "화면을 불러오지 못했습니다";
      if (heartbeatTimer) clearTimeout(heartbeatTimer);
      heartbeatTimer = undefined;
      unsubscribeEvents?.();
      unsubscribeEvents = undefined;
      unsubscribeWorkspace?.();
      unsubscribeWorkspace = undefined;
      revokeViewerControl(resolved?.kind === "live" ? resolved : undefined, nextMessage);
      if (!disposed) {
        setStatus("error");
        setMessage(nextMessage);
      }
    };

    void Promise.all([loadRuntime(), resolveSession(api, route)])
      .then(async ([runtime, nextResolved]) => {
        if (disposed) {
          if (nextResolved.kind === "live") {
            await api.live.closeSession(nextResolved.session.id).catch(() => undefined);
          } else {
            await api.replay.closeSession(nextResolved.session.id).catch(() => undefined);
          }
          return;
        }
        resolved = nextResolved;
        handle = new runtime.RmsWebHandle();
        handle.set_event_callback((event) => {
          if (isControlEvent(event)) {
            const live = resolved?.kind === "live" ? resolved : undefined;
            const cleanupRelease = live != null && event.type === "release_control_lease";
            const controlAuthorized =
              live != null &&
              lifecycleGate.canProcessLive() &&
              isLiveControlContextEnabled(live.workspace, live.device, live.session);
            if (!live || (!controlAuthorized && !cleanupRelease)) {
              const message =
                live
                  ? "현재 전환 상태에서는 제어할 수 없습니다"
                  : "Replay에서는 제어할 수 없습니다";
              controlQueue = controlQueue
                .catch(() => undefined)
                .then(() => {
                  deliverControlResponse({
                    type: "failed",
                    request_id: event.request_id,
                    device_id: event.device_id,
                    message,
                  });
                });
              return;
            }
            controlQueue = controlQueue.catch(() => undefined).then(async () => {
              if (
                event.type !== "release_control_lease" &&
                (!lifecycleGate.canProcessLive() ||
                  !isLiveControlContextEnabled(live.workspace, live.device, live.session))
              ) {
                deliverControlResponse({
                  type: "failed",
                  request_id: event.request_id,
                  device_id: event.device_id,
                  message: "현재 세션에서는 제어할 수 없습니다",
                });
                return;
              }
              const response = await resolveControlEvent(api, live, event, runtimeNonce);
              if (response.type === "lease_granted") {
                activeLease = { id: response.lease_id, epoch: response.lease_epoch };
              } else if (
                response.type === "lease_released" &&
                activeLease?.id === response.lease_id &&
                activeLease.epoch === response.lease_epoch
              ) {
                activeLease = undefined;
              } else if (response.type === "failed" && lifecycleGate.canProcessLive()) {
                releaseActiveLease(live);
              }

              if (!deliverControlResponse(response) && response.type === "lease_granted") {
                activeLease = undefined;
                await api.control
                  .releaseLease(live.session.id, response.lease_id)
                  .catch(() => undefined);
              }
            });
            return;
          }

          if (event.type === "open_replay" && resolved?.kind === "live") {
            if (!lifecycleGate.beginTransition()) return;
            const live = resolved;
            if (
              event.project_id !== live.workspace.project.id ||
              event.device_id !== live.device.id ||
              event.live_session_id !== live.session.id
            ) {
              fail(new Error("현재 실시간 세션과 일치하지 않습니다"));
              return;
            }
            revokeViewerControl(live);
            sessionCloseRequested = true;
            void api.live.closeSession(live.session.id)
              .then(async (recording) => {
                const replay = await api.replay.createSession({
                  projectId: live.workspace.project.id,
                  recordingId: recording.id,
                  openedBy: OPERATOR_ID,
                });
                if (!disposed) {
                  onNavigate(
                    replayPath(live.workspace.project.id, recording.id, replay.id),
                  );
                }
              })
              .catch(fail);
            return;
          }

          if (event.type === "open_live") {
            if (!lifecycleGate.beginTransition()) return;
            revokeViewerControl(resolved?.kind === "live" ? resolved : undefined);
            onNavigate(livePath(event.project_id, event.device_id));
            return;
          }

          if (event.type === "select_device") {
            if (!lifecycleGate.beginTransition()) return;
            revokeViewerControl(resolved?.kind === "live" ? resolved : undefined);
            onNavigate(livePath(route.projectId, event.device_id));
          }
        });

        await handle.start(canvas);
        runtimeStarted = true;
        if (disposed) {
          prepareStop();
          return;
        }
        applyResolvedSession(handle, nextResolved, lifecycleGate.canProcessLive());

        if (nextResolved.kind === "live") {
          const armHeartbeat = () => {
            if (!lifecycleGate.canProcessLive()) return;
            if (heartbeatTimer) clearTimeout(heartbeatTimer);
            heartbeatTimer = setTimeout(() => {
              if (!disposed) fail(new Error("실시간 연결이 끊겼습니다"));
            }, 10_000);
          };
          armHeartbeat();
          unsubscribeEvents = api.live.subscribeEvents(
            nextResolved.session.id,
            (event: RmsEvent) => {
              if (
                disposed ||
                !lifecycleGate.canProcessLive() ||
                resolved?.kind !== "live"
              ) {
                return;
              }
              if (
                event.liveSessionId !== resolved.session.id ||
                event.projectId !== resolved.workspace.project.id ||
                event.deviceId !== resolved.device.id
              ) {
                fail(new Error("실시간 이벤트의 세션 정보가 일치하지 않습니다"));
                return;
              }
              armHeartbeat();
              if (event.type === "topic.value.changed") {
                resolved = {
                  ...resolved,
                  topics: resolved.topics.map((topic) =>
                    topic.id === event.data.id ? { ...topic, ...event.data } : topic,
                  ),
                };
              } else if (event.type === "device.state.changed") {
                resolved = {
                  ...resolved,
                  device: { ...resolved.device, ...event.data },
                };
              }
              applyResolvedSession(handle!, resolved, lifecycleGate.canProcessLive());
            },
            () => {
              if (!disposed) fail(new Error("실시간 연결이 끊겼습니다"));
            },
          );
          unsubscribeWorkspace = api.projects.subscribeWorkspace(
            nextResolved.workspace.project.id,
            () => {
              void api.projects
                .getWorkspace(nextResolved.workspace.project.id)
                .then((workspace) => {
                  const current = resolved;
                  if (
                    disposed ||
                    lifecycleGate.hasFailed() ||
                    current?.kind !== "live" ||
                    workspace.snapshotVersion < current.workspace.snapshotVersion
                  ) {
                    return;
                  }
                  const device =
                    workspace.devices.find((candidate) => candidate.id === current.device.id) ??
                    current.device;
                  const source = workspace.dataSources.find((candidate) =>
                    isMatchingLiveSource(
                      candidate,
                      device.id,
                      current.session.dataSourceId,
                    ),
                  );
                  resolved = {
                    ...current,
                    workspace,
                    device,
                    topics: source
                      ? workspace.topicsByDataSource[source.id] ?? current.topics
                      : current.topics,
                  };
                  applyResolvedSession(handle!, resolved, lifecycleGate.canProcessLive());
                })
                .catch(fail);
            },
            () => fail(new Error("프로젝트 권한 연결이 끊겼습니다")),
          );
        }

        const canonicalPath =
          nextResolved.kind === "live"
            ? livePath(
                route.projectId,
                nextResolved.device.id,
                nextResolved.session.id,
                nextResolved.session.dataSourceId,
              )
            : replayPath(
                route.projectId,
                nextResolved.recording.id,
                nextResolved.session.id,
              );
        if (`${window.location.pathname}${window.location.search}` !== canonicalPath) {
          window.history.replaceState(null, "", canonicalPath);
        }
        setStatus("running");
      })
      .catch((cause: unknown) => {
        if (shuttingDown && handle && !runtimeStarted && !stopped) {
          handle.set_event_callback();
          handle.stop();
          stopped = true;
        }
        fail(cause);
      });

    return () => {
      disposed = true;
      shuttingDown = true;
      lifecycleGate.failClosed();
      if (heartbeatTimer) clearTimeout(heartbeatTimer);
      unsubscribeEvents?.();
      unsubscribeWorkspace?.();
      if (resolved?.kind === "live" && !sessionCloseRequested) {
        void api.live.closeSession(resolved.session.id).catch(() => undefined);
      } else if (resolved?.kind === "replay") {
        void api.replay.closeSession(resolved.session.id).catch(() => undefined);
      }
      if (handle) {
        prepareStop();
      } else {
        releaseActiveLease(resolved?.kind === "live" ? resolved : undefined);
      }
    };
  }, [api, onNavigate, route]);

  return (
    <section className="viewer-workspace" aria-label={route.kind === "live" ? "실시간 Viewer" : "Replay Viewer"}>
      <canvas
        ref={canvasRef}
        id="rms-product-canvas"
        className="viewer-workspace__canvas"
        aria-label={route.kind === "live" ? "RMS 실시간 Viewer" : "RMS Replay Viewer"}
      />
      {status !== "running" && (
        <div className={`viewer-workspace__status viewer-workspace__status--${status}`} role={status === "error" ? "alert" : "status"}>
          <strong>{status === "error" ? "화면을 열지 못했습니다" : "Viewer 준비 중"}</strong>
          <span>{message}</span>
          {status === "error" && (
            <button type="button" className="button button--primary" onClick={() => onNavigate(projectsPath(route.projectId))}>
              프로젝트로 이동
            </button>
          )}
        </div>
      )}
    </section>
  );
}
