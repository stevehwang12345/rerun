import { useCallback, useEffect, useMemo, useRef, useState } from "react";

import type { RmsApi } from "../api";
import type {
  Device,
  Project,
  RecordingImport,
  RecordingImportFormat,
} from "../domain";

export const RECORDING_IMPORT_FAILURE_MESSAGE = "파일을 가져오지 못했습니다";
export const RECORDING_IMPORT_ACCEPT =
  ".rrd,.mcap,.zip,.csv,.mp4,.mov,.webm,application/zip,text/csv,video/mp4,video/quicktime,video/webm";

const FORMAT_LABELS: Record<RecordingImportFormat, string> = {
  rrd: "RRD",
  mcap: "MCAP",
  "ros2-bag-zip": "ROS 2 Bag ZIP",
  csv: "CSV",
  video: "영상",
};

interface FileIdentity {
  name: string;
  type: string;
}

interface ProjectTarget {
  project: Project;
  devices: Device[];
}

type ImportPhase = "idle" | "uploading" | "processing" | "ready" | "failed";

export function detectRecordingImportFormat(
  file: FileIdentity,
): RecordingImportFormat | undefined {
  const lowerName = file.name.trim().toLowerCase();
  const mimeType = file.type.trim().toLowerCase();
  if (lowerName.endsWith(".rrd")) return "rrd";
  if (lowerName.endsWith(".mcap")) return "mcap";
  if (lowerName.endsWith(".zip") || mimeType === "application/zip") {
    return "ros2-bag-zip";
  }
  if (lowerName.endsWith(".csv") || mimeType === "text/csv") return "csv";
  if (
    /\.(mp4|mov|webm)$/.test(lowerName) ||
    mimeType === "video/mp4" ||
    mimeType === "video/quicktime" ||
    mimeType === "video/webm"
  ) {
    return "video";
  }
  return undefined;
}

function isAbortError(cause: unknown): boolean {
  return (
    (cause instanceof DOMException && cause.name === "AbortError") ||
    (typeof cause === "object" && cause != null && "name" in cause && cause.name === "AbortError")
  );
}

function abortableDelay(delayMs: number, signal: AbortSignal): Promise<void> {
  if (signal.aborted) return Promise.reject(new DOMException("Aborted", "AbortError"));
  return new Promise((resolve, reject) => {
    const timer = globalThis.setTimeout(() => {
      signal.removeEventListener("abort", abort);
      resolve();
    }, delayMs);
    const abort = () => {
      globalThis.clearTimeout(timer);
      reject(new DOMException("Aborted", "AbortError"));
    };
    signal.addEventListener("abort", abort, { once: true });
  });
}

export async function waitForRecordingImport(
  api: RmsApi,
  importId: string,
  signal: AbortSignal,
  onUpdate: (recordingImport: RecordingImport) => void,
  pollIntervalMs = 900,
): Promise<RecordingImport> {
  for (;;) {
    await abortableDelay(pollIntervalMs, signal);
    const recordingImport = await api.recordingImports.getImport(importId, { signal });
    onUpdate(recordingImport);
    if (
      recordingImport.status === "ready" ||
      recordingImport.status === "failed" ||
      recordingImport.status === "cancelled"
    ) {
      return recordingImport;
    }
  }
}

function boundedProgress(value: number): number {
  return Number.isFinite(value) ? Math.max(0, Math.min(100, Math.round(value))) : 0;
}

function phaseFor(recordingImport: RecordingImport): ImportPhase {
  if (recordingImport.status === "ready" && recordingImport.recordingId) return "ready";
  if (recordingImport.status === "uploading") return "uploading";
  if (recordingImport.status === "processing") return "processing";
  return "failed";
}

export function RecordingImportEditor({
  api,
  onCancel,
  onOpenReplay,
}: {
  api: RmsApi;
  onCancel: () => void;
  onOpenReplay: (projectId: string, recordingId: string) => void;
}) {
  const [targets, setTargets] = useState<ProjectTarget[]>([]);
  const [targetsLoading, setTargetsLoading] = useState(true);
  const [targetsError, setTargetsError] = useState(false);
  const [projectId, setProjectId] = useState("");
  const [deviceId, setDeviceId] = useState("");
  const [file, setFile] = useState<File>();
  const [format, setFormat] = useState<RecordingImportFormat>();
  const [phase, setPhase] = useState<ImportPhase>("idle");
  const [progress, setProgress] = useState(0);
  const [message, setMessage] = useState<string>();
  const [recordingImport, setRecordingImport] = useState<RecordingImport>();
  const inputRef = useRef<HTMLInputElement>(null);
  const controllerRef = useRef<AbortController | undefined>(undefined);
  const importIdRef = useRef<string | undefined>(undefined);
  const terminalRef = useRef(false);
  const targetLoadVersionRef = useRef(0);

  const loadTargets = useCallback(() => {
    const loadVersion = ++targetLoadVersionRef.current;
    setTargetsLoading(true);
    setTargetsError(false);
    void api.projects
      .listProjects()
      .then(async (projects) => {
        const availableProjects = projects.filter((project) => project.status !== "archived");
        const workspaces = await Promise.all(
          availableProjects.map((project) => api.projects.getWorkspace(project.id)),
        );
        if (targetLoadVersionRef.current !== loadVersion) return;
        const nextTargets = workspaces
          .map((workspace) => ({ project: workspace.project, devices: workspace.devices }))
          .filter((target) => target.devices.length > 0);
        setTargets(nextTargets);
        setProjectId((current) =>
          nextTargets.some((target) => target.project.id === current)
            ? current
            : (nextTargets[0]?.project.id ?? ""),
        );
      })
      .catch((cause: unknown) => {
        if (targetLoadVersionRef.current !== loadVersion) return;
        console.error("Failed to load recording import targets", cause);
        setTargetsError(true);
      })
      .finally(() => {
        if (targetLoadVersionRef.current === loadVersion) setTargetsLoading(false);
      });
  }, [api]);

  useEffect(() => {
    loadTargets();
    return () => {
      targetLoadVersionRef.current += 1;
    };
  }, [loadTargets]);

  const selectedTarget = useMemo(
    () => targets.find((target) => target.project.id === projectId),
    [projectId, targets],
  );

  useEffect(() => {
    const devices = selectedTarget?.devices ?? [];
    setDeviceId((current) =>
      devices.some((device) => device.id === current) ? current : (devices[0]?.id ?? ""),
    );
  }, [selectedTarget]);

  useEffect(
    () => () => {
      controllerRef.current?.abort();
      const importId = importIdRef.current;
      if (importId && !terminalRef.current) {
        void api.recordingImports.cancelImport(importId).catch((cause: unknown) => {
          console.error("Failed to cancel recording import during cleanup", cause);
        });
      }
    },
    [api],
  );

  const selectFile = (nextFile?: File) => {
    if (!nextFile) return;
    const detectedFormat = detectRecordingImportFormat(nextFile);
    setRecordingImport(undefined);
    setProgress(0);
    if (!detectedFormat || nextFile.size <= 0) {
      setFile(undefined);
      setFormat(undefined);
      setPhase("failed");
      setMessage("지원하는 파일을 선택해 주세요");
      return;
    }
    setFile(nextFile);
    setFormat(detectedFormat);
    setPhase("idle");
    setMessage(undefined);
  };

  const startImport = async () => {
    if (!file || !format || !projectId || !deviceId || phase === "uploading" || phase === "processing") {
      return;
    }
    const controller = new AbortController();
    controllerRef.current?.abort();
    controllerRef.current = controller;
    importIdRef.current = undefined;
    terminalRef.current = false;
    setPhase("uploading");
    setProgress(0);
    setMessage(undefined);
    setRecordingImport(undefined);

    try {
      const created = await api.recordingImports.createImport(
        { projectId, deviceId, file, format },
        {
          signal: controller.signal,
          onProgress: (nextProgress) => {
            if (!controller.signal.aborted) setProgress(boundedProgress(nextProgress));
          },
        },
      );
      importIdRef.current = created.id;
      if (controller.signal.aborted) {
        void api.recordingImports.cancelImport(created.id).catch(() => undefined);
        return;
      }
      setRecordingImport(created);
      setProgress(boundedProgress(created.progressPercent));
      setPhase(phaseFor(created));
      if (created.status === "ready" || created.status === "failed" || created.status === "cancelled") {
        terminalRef.current = true;
        if (phaseFor(created) === "failed") setMessage(RECORDING_IMPORT_FAILURE_MESSAGE);
        return;
      }

      const completed = await waitForRecordingImport(
        api,
        created.id,
        controller.signal,
        (nextImport) => {
          setRecordingImport(nextImport);
          setProgress(boundedProgress(nextImport.progressPercent));
          setPhase(phaseFor(nextImport));
        },
      );
      terminalRef.current = true;
      if (phaseFor(completed) === "failed") setMessage(RECORDING_IMPORT_FAILURE_MESSAGE);
    } catch (cause: unknown) {
      if (isAbortError(cause)) return;
      console.error("Recording import failed", cause);
      setPhase("failed");
      setMessage(RECORDING_IMPORT_FAILURE_MESSAGE);
      const importId = importIdRef.current;
      if (importId) {
        void api.recordingImports.cancelImport(importId).catch(() => undefined);
      }
    }
  };

  if (targetsLoading) {
    return <div className="import-placeholder" role="status">대상을 확인하고 있습니다</div>;
  }

  if (targetsError) {
    return (
      <div className="import-placeholder" role="alert">
        <span>프로젝트를 불러오지 못했습니다</span>
        <button type="button" className="button" onClick={loadTargets}>다시 시도</button>
      </div>
    );
  }

  if (targets.length === 0) {
    return (
      <div className="import-placeholder" role="status">
        프로젝트에 장비를 먼저 연결해 주세요
      </div>
    );
  }

  const busy = phase === "uploading" || phase === "processing";
  const ready = phase === "ready" && recordingImport?.recordingId != null;
  const fileLabel = file && format ? `${file.name} · ${FORMAT_LABELS[format]}` : "파일 선택";

  return (
    <form
      className="form-stack import-form"
      onSubmit={(event) => {
        event.preventDefault();
        void startImport();
      }}
    >
      <div className="import-targets">
        <label>
          프로젝트
          <select
            value={projectId}
            onChange={(event) => setProjectId(event.target.value)}
            disabled={busy || ready}
            autoFocus
          >
            {targets.map((target) => (
              <option key={target.project.id} value={target.project.id}>{target.project.name}</option>
            ))}
          </select>
        </label>
        <label>
          장비
          <select
            value={deviceId}
            onChange={(event) => setDeviceId(event.target.value)}
            disabled={busy || ready}
          >
            {(selectedTarget?.devices ?? []).map((device) => (
              <option key={device.id} value={device.id}>{device.name}</option>
            ))}
          </select>
        </label>
      </div>

      <input
        ref={inputRef}
        hidden
        type="file"
        accept={RECORDING_IMPORT_ACCEPT}
        onChange={(event) => selectFile(event.target.files?.[0])}
      />
      <button
        type="button"
        className="import-dropzone"
        disabled={busy || ready}
        onClick={() => inputRef.current?.click()}
        onDragOver={(event) => event.preventDefault()}
        onDrop={(event) => {
          event.preventDefault();
          if (!busy && !ready) selectFile(event.dataTransfer.files[0]);
        }}
        aria-describedby="recording-import-formats"
      >
        <strong>{fileLabel}</strong>
        <span id="recording-import-formats">RRD, MCAP, ROS 2 Bag ZIP, CSV, 영상</span>
      </button>

      {(busy || ready) && (
        <div className="import-progress" aria-live="polite">
          <div>
            <span>{phase === "uploading" ? "업로드 중" : phase === "processing" ? "처리 중" : "준비됨"}</span>
            <strong>{boundedProgress(progress)}%</strong>
          </div>
          <progress max={100} value={boundedProgress(progress)} aria-label="가져오기 진행률" />
        </div>
      )}

      {message && <span className="form-error" role="alert">{message}</span>}

      <div className="form-actions">
        <button type="button" className="button" onClick={onCancel}>
          {busy ? "취소" : "닫기"}
        </button>
        {ready ? (
          <button
            type="button"
            className="button button--primary"
            onClick={() => onOpenReplay(projectId, recordingImport.recordingId!)}
          >
            Replay 열기
          </button>
        ) : (
          <button
            type="submit"
            className="button button--primary"
            disabled={!file || !format || !projectId || !deviceId || busy}
          >
            {busy ? "처리 중" : phase === "failed" ? "다시 가져오기" : "가져오기"}
          </button>
        )}
      </div>
    </form>
  );
}
