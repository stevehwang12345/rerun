import { useCallback, useEffect, useState } from "react";

import type { RmsApi } from "../api";
import { StatusBadge, WorkspaceState } from "../components/ProductShell";
import { compactTimestamp, type WorkspaceSnapshot } from "../domain";
import { livePath, replayPath } from "../routes";

interface SessionCatalogWorkspaceProps {
  api: RmsApi;
  mode: "live" | "replay";
  onNavigate: (path: string) => void;
}

export function SessionCatalogWorkspace({ api, mode, onNavigate }: SessionCatalogWorkspaceProps) {
  const [workspaces, setWorkspaces] = useState<WorkspaceSnapshot[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string>();

  const refresh = useCallback(async () => {
    setLoading(true);
    setError(undefined);
    try {
      const projects = await api.projects.listProjects();
      setWorkspaces(await Promise.all(projects.map((project) => api.projects.getWorkspace(project.id))));
    } catch (cause: unknown) {
      setError(cause instanceof Error ? cause.message : "목록을 불러오지 못했습니다.");
    } finally {
      setLoading(false);
    }
  }, [api]);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  if (loading) {
    return <WorkspaceState title={mode === "live" ? "실시간 장비를 확인하고 있습니다" : "기록을 확인하고 있습니다"} />;
  }
  if (error) {
    return <WorkspaceState title="목록을 불러오지 못했습니다" description={error} actionLabel="다시 시도" onAction={() => void refresh()} tone="error" />;
  }

  const liveChoices = workspaces.flatMap((workspace) => workspace.devices.flatMap((device) =>
    workspace.dataSources
      .filter(
        (source) =>
          source.deviceId === device.id &&
          (source.status === "ready" || source.status === "recording") &&
          source.liveUrl.trim().length > 0 &&
          device.status !== "offline",
      )
      .map((source) => ({ workspace, device, source })),
  ));
  const replayChoices = workspaces.flatMap((workspace) => workspace.recordings
    .filter((recording) => recording.status === "ready")
    .map((recording) => ({ workspace, recording, device: workspace.devices.find((device) => device.id === recording.deviceId) })));

  return (
    <section className="workspace" aria-labelledby="catalog-title">
      <div className="workspace__heading">
        <div>
          <span className="workspace__eyebrow">{mode === "live" ? "RMS Live" : "RMS Replay"}</span>
          <h1 id="catalog-title">{mode === "live" ? "실시간" : "기록"}</h1>
        </div>
      </div>
      {mode === "live" ? (
        liveChoices.length === 0 ? <WorkspaceState title="현재 실시간 연결이 없습니다" /> : (
          <div className="catalog-list">
            {liveChoices.map(({ workspace, device, source }) => (
              <button key={`${workspace.project.id}:${device.id}:${source.id}`} type="button" className="catalog-row" onClick={() => onNavigate(livePath(workspace.project.id, device.id, undefined, source.id))}>
                <div><span>{workspace.project.name}</span><strong>{device.name}</strong><small>{device.taskName}</small></div>
                <div className="catalog-row__meta"><StatusBadge label={device.status === "online" ? "LIVE" : device.status === "degraded" ? "지연" : "연결 끊김"} tone={device.status === "online" ? "normal" : "attention"} /><span>{source.name}</span></div>
              </button>
            ))}
          </div>
        )
      ) : replayChoices.length === 0 ? <WorkspaceState title="저장된 기록이 없습니다" /> : (
        <div className="catalog-list">
          {replayChoices.map(({ workspace, recording, device }) => (
            <button key={recording.id} type="button" className="catalog-row" onClick={() => onNavigate(replayPath(workspace.project.id, recording.id))}>
              <div><span>{workspace.project.name}</span><strong>{recording.name}</strong><small>{device?.name ?? "장비"}</small></div>
              <div className="catalog-row__meta"><StatusBadge label="REPLAY" /><span>{compactTimestamp(recording.capturedAt)}</span><span>{recording.durationLabel}</span></div>
            </button>
          ))}
        </div>
      )}
    </section>
  );
}
