import { useCallback, useEffect, useMemo, useState } from "react";

import type { RmsApi } from "../api";
import { StatusBadge, WorkspaceState } from "../components/ProductShell";
import type { DataSource, Device, Project, WorkspaceSnapshot } from "../domain";
import { livePath, projectsPath, replayPath } from "../routes";

const ORGANIZATION_ID = "org-rms";

interface ProjectsWorkspaceProps {
  api: RmsApi;
  projectId?: string;
  onNavigate: (path: string) => void;
}

export function ProjectsWorkspace({ api, projectId, onNavigate }: ProjectsWorkspaceProps) {
  const [projects, setProjects] = useState<Project[]>([]);
  const [workspace, setWorkspace] = useState<WorkspaceSnapshot>();
  const [registryDevices, setRegistryDevices] = useState<Device[]>([]);
  const [registrySources, setRegistrySources] = useState<DataSource[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string>();
  const [creating, setCreating] = useState(false);

  const refresh = useCallback(async () => {
    setLoading(true);
    setError(undefined);
    try {
      const nextProjects = await api.projects.listProjects();
      setProjects(nextProjects);
      if (projectId) {
        const [nextWorkspace, devices, sources] = await Promise.all([
          api.projects.getWorkspace(projectId),
          api.integrations.listDevices(),
          api.integrations.listDataSources(),
        ]);
        setWorkspace(nextWorkspace);
        setRegistryDevices(devices);
        setRegistrySources(sources);
      } else {
        setWorkspace(undefined);
      }
    } catch (cause: unknown) {
      setError(cause instanceof Error ? cause.message : "프로젝트를 불러오지 못했습니다.");
    } finally {
      setLoading(false);
    }
  }, [api, projectId]);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  if (loading) {
    return <WorkspaceState title="프로젝트를 확인하고 있습니다" />;
  }

  if (error) {
    return (
      <WorkspaceState
        title="프로젝트를 불러오지 못했습니다"
        description={error}
        actionLabel="다시 시도"
        onAction={() => void refresh()}
        tone="error"
      />
    );
  }

  if (!projectId) {
    return (
      <section className="workspace" aria-labelledby="projects-title">
        <div className="workspace__heading">
          <div>
            <span className="workspace__eyebrow">RMS Projects</span>
            <h1 id="projects-title">프로젝트</h1>
          </div>
          <button type="button" className="button button--primary" onClick={() => setCreating(true)}>
            프로젝트 생성…
          </button>
        </div>
        {projects.length === 0 ? (
          <WorkspaceState title="사용 가능한 프로젝트가 없습니다" actionLabel="프로젝트 생성" onAction={() => setCreating(true)} />
        ) : (
          <div className="card-grid">
            {projects.map((project) => (
              <button
                type="button"
                className="project-card"
                key={project.id}
                onClick={() => onNavigate(projectsPath(project.id))}
              >
                <div>
                  <StatusBadge label={project.status === "active" ? "운영 중" : project.status === "standby" ? "대기" : "보관"} tone={project.status === "active" ? "normal" : "neutral"} />
                  <h2>{project.name}</h2>
                  <p>{project.description}</p>
                </div>
                <dl>
                  <div><dt>장비</dt><dd>{project.deviceCount}</dd></div>
                  <div><dt>온라인</dt><dd>{project.onlineDeviceCount}</dd></div>
                </dl>
              </button>
            ))}
          </div>
        )}
        {creating && (
          <ProjectEditor
            onCancel={() => setCreating(false)}
            onSubmit={async (name, description) => {
              const project = await api.projects.createProject({
                organizationId: ORGANIZATION_ID,
                name,
                description,
                status: "active",
              });
              setCreating(false);
              onNavigate(projectsPath(project.id));
            }}
          />
        )}
      </section>
    );
  }

  if (!workspace) {
    return <WorkspaceState title="프로젝트를 찾을 수 없습니다" actionLabel="프로젝트 목록" onAction={() => onNavigate(projectsPath())} />;
  }

  return (
    <ProjectDetail
      api={api}
      workspace={workspace}
      registryDevices={registryDevices}
      registrySources={registrySources}
      onNavigate={onNavigate}
      onRefresh={refresh}
    />
  );
}

function ProjectDetail({
  api,
  workspace,
  registryDevices,
  registrySources,
  onNavigate,
  onRefresh,
}: {
  api: RmsApi;
  workspace: WorkspaceSnapshot;
  registryDevices: Device[];
  registrySources: DataSource[];
  onNavigate: (path: string) => void;
  onRefresh: () => Promise<void>;
}) {
  const [deviceId, setDeviceId] = useState("");
  const [sourceId, setSourceId] = useState("");
  const [saving, setSaving] = useState(false);
  const [saveError, setSaveError] = useState<string>();
  const assignedDeviceIds = useMemo(
    () => new Set(workspace.deviceAssignments.map((assignment) => assignment.deviceId)),
    [workspace.deviceAssignments],
  );
  const assignedSourceIds = useMemo(
    () => new Set(workspace.dataAssignments.map((assignment) => assignment.dataSourceId)),
    [workspace.dataAssignments],
  );
  const availableDevices = registryDevices.filter((device) => !assignedDeviceIds.has(device.id));
  const availableSources = registrySources.filter(
    (source) => assignedDeviceIds.has(source.deviceId) && !assignedSourceIds.has(source.id),
  );

  return (
    <section className="workspace" aria-labelledby="project-title">
      <div className="workspace__heading">
        <div>
          <button type="button" className="workspace__back" onClick={() => onNavigate(projectsPath())}>프로젝트</button>
          <h1 id="project-title">{workspace.project.name}</h1>
        </div>
        <StatusBadge label={workspace.project.status === "active" ? "운영 중" : "대기"} tone={workspace.project.status === "active" ? "normal" : "neutral"} />
      </div>

      <div className="summary-strip" aria-label="프로젝트 요약">
        <Summary value={workspace.devices.length} label="장비" />
        <Summary value={workspace.devices.filter((device) => device.status === "online").length} label="온라인" />
        <Summary value={workspace.recordings.filter((recording) => recording.status === "ready").length} label="기록" />
      </div>

      <div className="project-layout">
        <section className="resource-column resource-column--wide">
          <header><h2>장비와 데이터</h2></header>
          <div className="resource-column__list">
            {workspace.devices.map((device) => {
              const sources = workspace.dataSources.filter((source) => source.deviceId === device.id);
              const liveSources = sources.filter(
                (source) =>
                  (source.status === "ready" || source.status === "recording") &&
                  source.liveUrl.trim().length > 0,
              );
              return (
                <div className="device-card" key={device.id}>
                  <div className="device-card__summary">
                    <div>
                      <strong>{device.name}</strong>
                      <span>{device.taskName}</span>
                    </div>
                    <StatusBadge
                      label={device.status === "online" ? "연결됨" : device.status === "degraded" ? "지연됨" : "연결 끊김"}
                      tone={device.status === "online" ? "normal" : device.status === "degraded" ? "attention" : "restricted"}
                    />
                  </div>
                  <div className="device-card__sources">
                    {sources.length === 0 ? <span>연결된 데이터 없음</span> : sources.map((source) => <span key={source.id}>{source.name}</span>)}
                  </div>
                  <div className="device-card__actions">
                    {device.status !== "offline" && liveSources.map((source) => (
                      <button
                        key={source.id}
                        type="button"
                        className="button button--primary"
                        onClick={() => onNavigate(livePath(workspace.project.id, device.id, undefined, source.id))}
                      >
                        {source.name} 열기
                      </button>
                    ))}
                    {workspace.recordings.some((recording) => recording.deviceId === device.id && recording.status === "ready") && (
                      <button type="button" className="button" onClick={() => {
                        const recording = workspace.recordings.find((candidate) => candidate.deviceId === device.id && candidate.status === "ready");
                        if (recording) onNavigate(replayPath(workspace.project.id, recording.id));
                      }}>기록 보기</button>
                    )}
                  </div>
                </div>
              );
            })}
            {workspace.devices.length === 0 && <WorkspaceState title="이 프로젝트에 연결된 장비가 없습니다" />}
          </div>
        </section>

        <aside className="assignment-panel">
          <h2>리소스 추가</h2>
          <label>
            장비
            <select value={deviceId} onChange={(event) => setDeviceId(event.target.value)}>
              <option value="">장비 선택</option>
              {availableDevices.map((device) => <option key={device.id} value={device.id}>{device.name}</option>)}
            </select>
          </label>
          <button
            type="button"
            className="button"
            disabled={!deviceId || saving}
            onClick={() => {
              setSaving(true);
              setSaveError(undefined);
              void api.projects.assignDevice(workspace.project.id, { deviceId, accessMode: "control" })
                .then(() => { setDeviceId(""); return onRefresh(); })
                .catch((cause: unknown) => setSaveError(cause instanceof Error ? cause.message : "추가하지 못했습니다"))
                .finally(() => setSaving(false));
            }}
          >
            장비 추가
          </button>
          <label>
            데이터
            <select value={sourceId} onChange={(event) => setSourceId(event.target.value)}>
              <option value="">데이터 선택</option>
              {availableSources.map((source) => <option key={source.id} value={source.id}>{source.name}</option>)}
            </select>
          </label>
          <button
            type="button"
            className="button"
            disabled={!sourceId || saving}
            onClick={() => {
              setSaving(true);
              setSaveError(undefined);
              void api.projects.assignDataSource(workspace.project.id, { dataSourceId: sourceId, visibility: "operator" })
                .then(() => { setSourceId(""); return onRefresh(); })
                .catch((cause: unknown) => setSaveError(cause instanceof Error ? cause.message : "추가하지 못했습니다"))
                .finally(() => setSaving(false));
            }}
          >
            데이터 추가
          </button>
          {saveError && <span className="form-error" role="alert">{saveError}</span>}
        </aside>
      </div>
    </section>
  );
}

function Summary({ value, label }: { value: number; label: string }) {
  return <div className="summary-strip__item"><strong>{value}</strong><span>{label}</span></div>;
}

function ProjectEditor({
  onCancel,
  onSubmit,
}: {
  onCancel: () => void;
  onSubmit: (name: string, description: string) => Promise<void>;
}) {
  const [name, setName] = useState("");
  const [description, setDescription] = useState("");
  const [saving, setSaving] = useState(false);
  const [submitError, setSubmitError] = useState<string>();
  return (
    <div className="dialog-backdrop" role="presentation">
      <section className="dialog" role="dialog" aria-modal="true" aria-labelledby="project-dialog-title">
        <header><h2 id="project-dialog-title">프로젝트 생성</h2><button type="button" className="icon-button" aria-label="닫기" onClick={onCancel}>×</button></header>
        <form className="form-stack" onSubmit={(event) => { event.preventDefault(); if (!name.trim() || saving) return; setSaving(true); setSubmitError(undefined); void onSubmit(name.trim(), description.trim()).catch((cause: unknown) => setSubmitError(cause instanceof Error ? cause.message : "생성하지 못했습니다")).finally(() => setSaving(false)); }}>
          <label>이름<input value={name} onChange={(event) => setName(event.target.value)} autoFocus /></label>
          <label>설명<input value={description} onChange={(event) => setDescription(event.target.value)} /></label>
          {submitError && <span className="form-error" role="alert">{submitError}</span>}
          <div className="form-actions"><button type="button" className="button" onClick={onCancel}>취소</button><button type="submit" className="button button--primary" disabled={!name.trim() || saving}>{saving ? "생성 중" : "생성"}</button></div>
        </form>
      </section>
    </div>
  );
}
