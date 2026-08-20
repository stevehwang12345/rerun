import { useEffect, useRef, useState } from "react";

import { createRmsApi } from "./api";
import { OPERATOR_ID } from "./domain";
import type { DataSourceKind, SessionMode } from "./domain";

const RUNTIME_MODULE_URL = "/runtime/rms_product_app.js";
const RUNTIME_WASM_URL = "/runtime/rms_product_app_bg.wasm";

interface RmsViewerContext {
  project_name: string;
  device_id: string;
  device_name: string;
  device_status: string;
  device_health: string;
  device_state_version: number;
  source_id: string;
  source_name: string;
  source_kind: DataSourceKind;
  source_url: string;
  operator_id: string;
  topics: Array<{ label: string; path: string; renderer: string; value?: string }>;
}

interface RmsWebHandle {
  start(canvas: HTMLCanvasElement): Promise<void>;
  set_event_callback(callback?: (event: RmsProductEvent) => void): void;
  apply_viewer_context(context: RmsViewerContext): void;
  apply_viewer_error(message: string): void;
  apply_control_response(response: RmsControlResponse): void;
  stop(): void;
}

type RmsWorkspaceEvent =
  | { type: "select_device"; device_id: string }
  | {
      type: "select_source_kind";
      device_id: string;
      source_kind: DataSourceKind;
    };

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
      session_mode: SessionMode;
    };

type RmsProductEvent = RmsWorkspaceEvent | RmsControlEvent;

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

interface RmsProductRuntimeModule {
  default(input: { module_or_path: URL }): Promise<unknown>;
  RmsWebHandle: new () => RmsWebHandle;
}

let runtimeModulePromise: Promise<RmsProductRuntimeModule> | undefined;
const rmsApi = createRmsApi();

function isWorkspaceEvent(event: RmsProductEvent): event is RmsWorkspaceEvent {
  return event.type === "select_device" || event.type === "select_source_kind";
}

async function loadViewerContext(
  requestedDeviceId?: string,
  requestedSourceKind?: DataSourceKind,
): Promise<RmsViewerContext> {
  const projects = await rmsApi.listProjects();
  for (const project of projects) {
    const devices = await rmsApi.listDevices(project.id);
    const device = requestedDeviceId
      ? devices.find((candidate) => candidate.id === requestedDeviceId)
      : devices[0];
    if (!device) {
      continue;
    }

    const sources = await rmsApi.listDataSources(project.id, device.id);
    const source = requestedSourceKind
      ? sources.find((candidate) => candidate.kind === requestedSourceKind)
      : sources.find((candidate) => candidate.kind === "live") ?? sources[0];
    if (!source) {
      throw new Error("선택한 장비의 데이터를 찾을 수 없습니다.");
    }
    const topics = await rmsApi.listTopics(device.id, source.id);
    return {
      project_name: project.name,
      device_id: device.id,
      device_name: device.name,
      device_status: device.status,
      device_health: device.health,
      device_state_version: device.stateVersion,
      source_id: source.id,
      source_name: source.name,
      source_kind: source.kind,
      source_url: source.rrdUrl,
      operator_id: OPERATOR_ID,
      topics: topics.map((topic) => ({
        label: topic.label,
        path: topic.path,
        renderer: topic.renderer,
        value: topic.value ?? topic.message,
      })),
    };
  }
  throw new Error("사용 가능한 프로젝트 또는 장비를 찾을 수 없습니다.");
}

async function resolveControlEvent(event: RmsControlEvent): Promise<RmsControlResponse> {
  try {
    switch (event.type) {
      case "request_control_lease": {
        const lease = await rmsApi.requestControlLease(
          event.device_id,
          event.expected_device_version,
        );
        if (lease.deviceId !== event.device_id || lease.holderId !== OPERATOR_ID) {
          await rmsApi.releaseControlLease(lease.deviceId, lease.id);
          throw new Error("현재 사용자에게 발급된 제어권이 아닙니다.");
        }
        const expiresAtMs = Date.parse(lease.expiresAt);
        if (!Number.isFinite(expiresAtMs) || expiresAtMs <= Date.now()) {
          await rmsApi.releaseControlLease(lease.deviceId, lease.id);
          throw new Error("만료된 제어권을 받았습니다.");
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
      case "release_control_lease":
        await rmsApi.releaseControlLease(event.device_id, event.lease_id);
        return {
          type: "lease_released",
          request_id: event.request_id,
          device_id: event.device_id,
          lease_id: event.lease_id,
          lease_epoch: event.lease_epoch,
        };
      case "send_control_command": {
        const issuedAt = new Date();
        const receipt = await rmsApi.sendControlCommand({
          deviceId: event.device_id,
          commandType: event.command_type,
          expectedDeviceVersion: event.expected_device_version,
          idempotencyKey: crypto.randomUUID(),
          leaseId: event.lease_id,
          leaseEpoch: event.lease_epoch,
          sessionMode: event.session_mode,
          issuedAt: issuedAt.toISOString(),
          expiresAt: new Date(issuedAt.getTime() + 5_000).toISOString(),
        });
        return {
          type: "command_updated",
          request_id: event.request_id,
          device_id: event.device_id,
          message: receipt.message,
        };
      }
    }
  } catch (error: unknown) {
    return {
      type: "failed",
      request_id: event.request_id,
      device_id: event.device_id,
      message: error instanceof Error ? error.message : "요청을 처리하지 못했습니다.",
    };
  }
}

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

export default function App() {
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const [status, setStatus] = useState<"loading" | "running" | "error">("loading");

  useEffect(() => {
    const canvas = canvasRef.current;
    if (!canvas) {
      setStatus("error");
      return;
    }

    let disposed = false;
    let handle: RmsWebHandle | undefined;
    let workspaceGeneration = 0;
    let controlQueue = Promise.resolve();

    void loadRuntime()
      .then(async (runtime) => {
        if (disposed) {
          return;
        }

        handle = new runtime.RmsWebHandle();
        handle.set_event_callback((event) => {
          if (isWorkspaceEvent(event)) {
            const generation = ++workspaceGeneration;
            void loadViewerContext(
              event.device_id,
              event.type === "select_source_kind" ? event.source_kind : undefined,
            )
              .then((context) => {
                if (!disposed && generation === workspaceGeneration) {
                  handle?.apply_viewer_context(context);
                }
              })
              .catch((error: unknown) => {
                if (!disposed && generation === workspaceGeneration) {
                  handle?.apply_viewer_error(
                    error instanceof Error ? error.message : "데이터를 열지 못했습니다.",
                  );
                }
              });
            return;
          }

          controlQueue = controlQueue.then(async () => {
            const response = await resolveControlEvent(event);
            if (!disposed) {
              handle?.apply_control_response(response);
            }
          });
        });
        await handle.start(canvas);
        if (disposed) {
          handle.stop();
          handle = undefined;
          return;
        }

        const context = await loadViewerContext();
        if (disposed) {
          handle.stop();
          handle = undefined;
          return;
        }
        handle.apply_viewer_context(context);
        setStatus("running");
      })
      .catch((error: unknown) => {
        console.error("Failed to start the RMS product runtime.", error);
        handle?.stop();
        handle = undefined;
        if (!disposed) {
          setStatus("error");
        }
      });

    return () => {
      disposed = true;
      handle?.set_event_callback();
      handle?.stop();
      handle = undefined;
    };
  }, []);

  return (
    <main className="runtime-host">
      <canvas
        ref={canvasRef}
        id="rms-product-canvas"
        className="runtime-host__canvas"
        aria-label="RMS 통합 Viewer"
      />
      {status !== "running" && (
        <p className="runtime-host__status" role={status === "error" ? "alert" : "status"}>
          {status === "error"
            ? "RMS Viewer를 시작할 수 없습니다."
            : "RMS Viewer를 시작하는 중입니다."}
        </p>
      )}
    </main>
  );
}
