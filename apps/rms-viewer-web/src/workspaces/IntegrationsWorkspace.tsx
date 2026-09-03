import { useCallback, useEffect, useState } from "react";
import type { ReactNode } from "react";

import type { RmsApi } from "../api";
import type {
  DataSource,
  Device,
  DeviceKind,
  Integration,
  IntegrationKind,
} from "../domain";
import { deviceKindLabel } from "../domain";
import { StatusBadge, WorkspaceState } from "../components/ProductShell";
import { NetworkDiscoveryEditor } from "./NetworkDiscoveryEditor";
import { RecordingImportEditor } from "./RecordingImportEditor";

const ORGANIZATION_ID = "org-rms";
interface IntegrationsWorkspaceProps {
  api: RmsApi;
  onOpenProjects: () => void;
  onOpenReplay: (projectId: string, recordingId: string) => void;
}

type Editor = "network" | "integration" | "device" | "source" | "import" | undefined;

export function integrationKindLabel(kind: IntegrationKind): string {
  return {
    ros2: "로봇 데이터",
    mcap: "파일 데이터",
    rtsp: "영상",
    mavlink: "비행 데이터",
    autoware: "차량 데이터",
    rerun: "Rerun 데이터",
    rms_edge: "현장 Edge Agent",
  }[kind];
}

export function dataSourceKindLabel(source: DataSource, device?: Device): string {
  if (source.protocol.trim().toLowerCase() === "file") return "파일 데이터";
  if (device?.kind === "robot") return "로봇 데이터";
  if (device?.kind === "drone") return "비행 데이터";
  if (device?.kind === "vehicle") return "차량 데이터";
  if (device?.kind === "camera") return "영상";
  if (source.protocol.toLowerCase().includes("rerun")) return "Rerun 데이터";
  return "데이터 연결";
}

export function IntegrationsWorkspace({
  api,
  onOpenProjects,
  onOpenReplay,
}: IntegrationsWorkspaceProps) {
  const [integrations, setIntegrations] = useState<Integration[]>([]);
  const [devices, setDevices] = useState<Device[]>([]);
  const [dataSources, setDataSources] = useState<DataSource[]>([]);
  const [editor, setEditor] = useState<Editor>();
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string>();

  const refresh = useCallback(async () => {
    setLoading(true);
    setError(undefined);
    try {
      const [nextIntegrations, nextDevices, nextSources] = await Promise.all([
        api.integrations.listIntegrations(),
        api.integrations.listDevices(),
        api.integrations.listDataSources(),
      ]);
      setIntegrations(nextIntegrations);
      setDevices(nextDevices);
      setDataSources(nextSources);
    } catch (cause: unknown) {
      console.error("Failed to load integrations workspace", cause);
      setError("연동 정보를 불러오지 못했습니다");
    } finally {
      setLoading(false);
    }
  }, [api]);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  const connectedCount = integrations.filter((item) => item.status === "connected").length;
  const onlineCount = devices.filter((device) => device.status === "online").length;

  return (
    <section className="workspace" aria-labelledby="integrations-title">
      <div className="workspace__heading">
        <div>
          <span className="workspace__eyebrow">RMS Connect</span>
          <h1 id="integrations-title">데이터와 장비 연동</h1>
        </div>
        <div className="workspace__actions">
          <button type="button" className="button" onClick={() => setEditor("network")}>
            네트워크에서 찾기…
          </button>
          <button type="button" className="button" onClick={() => setEditor("import")}>
            파일 가져오기…
          </button>
          <button type="button" className="button" onClick={() => setEditor("integration")}>
            새 연동…
          </button>
          <button type="button" className="button button--primary" onClick={onOpenProjects}>
            프로젝트로 이동
          </button>
        </div>
      </div>

      <div className="summary-strip" aria-label="연동 요약">
        <Summary value={connectedCount} label="연결됨" />
        <Summary value={onlineCount} label="온라인 장비" />
        <Summary value={dataSources.length} label="데이터 소스" />
      </div>

      {loading && <WorkspaceState title="연동 상태를 확인하고 있습니다" />}
      {error && (
        <WorkspaceState
          title="연동 정보를 불러오지 못했습니다"
          description={error}
          actionLabel="다시 시도"
          onAction={() => void refresh()}
          tone="error"
        />
      )}

      {!loading && !error && (
        <div className="workspace-grid workspace-grid--three">
          <ResourceColumn
            title="데이터 연결"
            actionLabel="연동 추가…"
            onAction={() => setEditor("integration")}
          >
            {integrations.map((integration) => (
              <ResourceRow
                key={integration.id}
                title={integration.name}
                subtitle={integrationKindLabel(integration.kind)}
                badge={integration.status === "connected" ? "연결됨" : "확인 필요"}
                tone={integration.status === "connected" ? "normal" : "attention"}
              />
            ))}
          </ResourceColumn>

          <ResourceColumn
            title="장비"
            actionLabel="장비 등록…"
            onAction={() => setEditor("device")}
          >
            {devices.map((device) => {
              const needsConfirmation = device.health === "unknown";
              return (
                <ResourceRow
                  key={device.id}
                  title={device.name}
                  subtitle={deviceKindLabel(device.kind)}
                  badge={needsConfirmation ? "확인 필요" : device.status === "online" ? "연결됨" : device.status === "degraded" ? "지연됨" : "연결 끊김"}
                  tone={needsConfirmation ? "attention" : device.status === "online" ? "normal" : device.status === "degraded" ? "attention" : "restricted"}
                />
              );
            })}
          </ResourceColumn>

          <ResourceColumn
            title="데이터"
            actionLabel="데이터 등록…"
            onAction={() => setEditor("source")}
          >
            {dataSources.map((source) => (
              <ResourceRow
                key={source.id}
                title={source.name}
                subtitle={
                  source.status === "pending"
                    ? "연결 확인 중"
                    : dataSourceKindLabel(
                        source,
                        devices.find((device) => device.id === source.deviceId),
                      )
                }
                badge={source.status === "ready" || source.status === "recording" ? "준비됨" : "확인 필요"}
                tone={source.status === "ready" || source.status === "recording" ? "normal" : "attention"}
              />
            ))}
          </ResourceColumn>
        </div>
      )}

      {editor === "integration" && (
        <IntegrationEditor
          onCancel={() => setEditor(undefined)}
          onSubmit={async (name, kind, endpointLabel) => {
            await api.integrations.createIntegration({
              organizationId: ORGANIZATION_ID,
              name,
              kind,
              endpointLabel,
            });
            setEditor(undefined);
            await refresh();
          }}
        />
      )}
      {editor === "network" && (
        <DialogFrame title="네트워크에서 찾기" onCancel={() => setEditor(undefined)}>
          <NetworkDiscoveryEditor
            api={api}
            organizationId={
              integrations[0]?.organizationId ?? devices[0]?.organizationId ?? ORGANIZATION_ID
            }
            onCancel={() => setEditor(undefined)}
            onLinked={refresh}
            onOpenProjects={() => {
              setEditor(undefined);
              onOpenProjects();
            }}
          />
        </DialogFrame>
      )}
      {editor === "device" && (
        <DeviceEditor
          integrations={integrations}
          onCancel={() => setEditor(undefined)}
          onSubmit={async (integrationId, name, kind) => {
            await api.integrations.registerDevice({
              organizationId: ORGANIZATION_ID,
              integrationId,
              name,
              kind,
              status: "online",
              health: "normal",
              operationMode: "대기",
              batteryPercent: 100,
              taskName: "할당 없음",
              taskProgress: 0,
              lastSeenAt: new Date().toISOString(),
            });
            setEditor(undefined);
            await refresh();
          }}
        />
      )}
      {editor === "source" && (
        <SourceEditor
          devices={devices}
          onCancel={() => setEditor(undefined)}
          onSubmit={async (deviceId, name, protocol, liveUrl) => {
            const device = devices.find((candidate) => candidate.id === deviceId);
            if (!device) {
              return;
            }
            await api.integrations.registerDataSource({
              integrationId: device.integrationId,
              deviceId,
              name,
              protocol,
              status: "ready",
              liveUrl,
              topicIds: [
                "pose",
                "front-camera",
                device.kind === "drone" ? "altitude" : "velocity",
                "battery",
                "planner",
              ],
              lastDataAt: new Date().toISOString(),
            });
            setEditor(undefined);
            await refresh();
          }}
        />
      )}
      {editor === "import" && (
        <DialogFrame title="파일 가져오기" onCancel={() => setEditor(undefined)}>
          <RecordingImportEditor
            api={api}
            onCancel={() => setEditor(undefined)}
            onOpenReplay={onOpenReplay}
          />
        </DialogFrame>
      )}
    </section>
  );
}

function Summary({ value, label }: { value: number; label: string }) {
  return (
    <div className="summary-strip__item">
      <strong>{value}</strong>
      <span>{label}</span>
    </div>
  );
}

function ResourceColumn({
  title,
  actionLabel,
  onAction,
  children,
}: {
  title: string;
  actionLabel: string;
  onAction: () => void;
  children: ReactNode;
}) {
  return (
    <section className="resource-column">
      <header>
        <h2>{title}</h2>
        <button type="button" className="text-button" onClick={onAction}>
          {actionLabel}
        </button>
      </header>
      <div className="resource-column__list">{children}</div>
    </section>
  );
}

function ResourceRow({
  title,
  subtitle,
  badge,
  tone,
}: {
  title: string;
  subtitle: string;
  badge: string;
  tone: "normal" | "attention" | "restricted";
}) {
  return (
    <div className="resource-row">
      <div>
        <strong>{title}</strong>
        <span>{subtitle}</span>
      </div>
      <StatusBadge label={badge} tone={tone} />
    </div>
  );
}

function DialogFrame({
  title,
  children,
  onCancel,
}: {
  title: string;
  children: ReactNode;
  onCancel: () => void;
}) {
  return (
    <div className="dialog-backdrop" role="presentation">
      <section className="dialog" role="dialog" aria-modal="true" aria-labelledby="dialog-title">
        <header>
          <h2 id="dialog-title">{title}</h2>
          <button type="button" className="icon-button" aria-label="닫기" onClick={onCancel}>
            ×
          </button>
        </header>
        {children}
      </section>
    </div>
  );
}

function mutationMessage(cause: unknown): string {
  console.error("Failed to update an integration resource", cause);
  return "저장하지 못했습니다";
}

function IntegrationEditor({
  onCancel,
  onSubmit,
}: {
  onCancel: () => void;
  onSubmit: (name: string, kind: IntegrationKind, endpointLabel: string) => Promise<void>;
}) {
  const [name, setName] = useState("");
  const [kind, setKind] = useState<IntegrationKind>("ros2");
  const [endpoint, setEndpoint] = useState("");
  const [saving, setSaving] = useState(false);
  const [submitError, setSubmitError] = useState<string>();
  const valid = name.trim() && endpoint.trim();

  return (
    <DialogFrame title="새 데이터 연동" onCancel={onCancel}>
      <form
        className="form-stack"
        onSubmit={(event) => {
          event.preventDefault();
          if (!valid || saving) return;
          setSaving(true);
          setSubmitError(undefined);
          void onSubmit(name.trim(), kind, endpoint.trim())
            .catch((cause: unknown) => setSubmitError(mutationMessage(cause)))
            .finally(() => setSaving(false));
        }}
      >
        <label>이름<input value={name} onChange={(event) => setName(event.target.value)} autoFocus /></label>
        <label>종류<select value={kind} onChange={(event) => setKind(event.target.value as IntegrationKind)}>{(["ros2", "mcap", "rtsp", "mavlink", "autoware", "rerun"] as IntegrationKind[]).map((value) => <option key={value}>{value}</option>)}</select></label>
        <label>연결 위치<input value={endpoint} onChange={(event) => setEndpoint(event.target.value)} placeholder="현장 Gateway 또는 파일" /></label>
        {submitError && <span className="form-error" role="alert">{submitError}</span>}
        <FormActions onCancel={onCancel} saving={saving} valid={Boolean(valid)} />
      </form>
    </DialogFrame>
  );
}

function DeviceEditor({
  integrations,
  onCancel,
  onSubmit,
}: {
  integrations: Integration[];
  onCancel: () => void;
  onSubmit: (integrationId: string, name: string, kind: DeviceKind) => Promise<void>;
}) {
  const [integrationId, setIntegrationId] = useState(integrations[0]?.id ?? "");
  const [name, setName] = useState("");
  const [kind, setKind] = useState<DeviceKind>("robot");
  const [saving, setSaving] = useState(false);
  const [submitError, setSubmitError] = useState<string>();
  const valid = integrationId && name.trim();
  return (
    <DialogFrame title="장비 등록" onCancel={onCancel}>
      <form className="form-stack" onSubmit={(event) => { event.preventDefault(); if (!valid || saving) return; setSaving(true); setSubmitError(undefined); void onSubmit(integrationId, name.trim(), kind).catch((cause: unknown) => setSubmitError(mutationMessage(cause))).finally(() => setSaving(false)); }}>
        <label>연동<select value={integrationId} onChange={(event) => setIntegrationId(event.target.value)}>{integrations.map((item) => <option key={item.id} value={item.id}>{item.name}</option>)}</select></label>
        <label>장비명<input value={name} onChange={(event) => setName(event.target.value)} autoFocus /></label>
        <label>종류<select value={kind} onChange={(event) => setKind(event.target.value as DeviceKind)}><option value="robot">로봇</option><option value="drone">드론</option><option value="vehicle">차량</option><option value="camera">카메라</option><option value="gateway">게이트웨이</option></select></label>
        {submitError && <span className="form-error" role="alert">{submitError}</span>}
        <FormActions onCancel={onCancel} saving={saving} valid={Boolean(valid)} />
      </form>
    </DialogFrame>
  );
}

function SourceEditor({
  devices,
  onCancel,
  onSubmit,
}: {
  devices: Device[];
  onCancel: () => void;
  onSubmit: (deviceId: string, name: string, protocol: string, liveUrl: string) => Promise<void>;
}) {
  const [deviceId, setDeviceId] = useState(devices[0]?.id ?? "");
  const [name, setName] = useState("");
  const [protocol, setProtocol] = useState("rerun");
  const [liveUrl, setLiveUrl] = useState("");
  const [saving, setSaving] = useState(false);
  const [submitError, setSubmitError] = useState<string>();
  const valid = deviceId && name.trim() && liveUrl.trim();
  return (
    <DialogFrame title="데이터 등록" onCancel={onCancel}>
      <form className="form-stack" onSubmit={(event) => { event.preventDefault(); if (!valid || saving) return; setSaving(true); setSubmitError(undefined); void onSubmit(deviceId, name.trim(), protocol, liveUrl.trim()).catch((cause: unknown) => setSubmitError(mutationMessage(cause))).finally(() => setSaving(false)); }}>
        <label>장비<select value={deviceId} onChange={(event) => setDeviceId(event.target.value)}>{devices.map((device) => <option key={device.id} value={device.id}>{device.name}</option>)}</select></label>
        <label>데이터명<input value={name} onChange={(event) => setName(event.target.value)} autoFocus /></label>
        <label>프로토콜<select value={protocol} onChange={(event) => setProtocol(event.target.value)}><option value="rerun">Rerun</option><option value="ros2">ROS 2</option><option value="mcap">MCAP</option><option value="rtsp">RTSP</option><option value="mavlink">MAVLink</option><option value="autoware">Autoware</option></select></label>
        <label>데이터 주소<input value={liveUrl} onChange={(event) => setLiveUrl(event.target.value)} placeholder="RRD 또는 Redap 주소" /></label>
        {submitError && <span className="form-error" role="alert">{submitError}</span>}
        <FormActions onCancel={onCancel} saving={saving} valid={Boolean(valid)} />
      </form>
    </DialogFrame>
  );
}

function FormActions({ onCancel, saving, valid }: { onCancel: () => void; saving: boolean; valid: boolean }) {
  return (
    <div className="form-actions">
      <button type="button" className="button" onClick={onCancel}>취소</button>
      <button type="submit" className="button button--primary" disabled={!valid || saving}>{saving ? "저장 중" : "저장"}</button>
    </div>
  );
}
