import { useCallback, useMemo, useRef, useState } from "react";
import { createRmsApi, isMockMode } from "./api";
import { ContextSidebar } from "./components/ContextSidebar";
import { ControlDock } from "./components/ControlDock";
import { Glyph } from "./components/Glyph";
import {
  RmsRerunViewerAdapter,
  type RmsRerunViewerHandle,
} from "./components/RmsRerunViewerAdapter";
import { StatusHeader } from "./components/StatusHeader";
import { TopicPanel, type ViewerPreset } from "./components/TopicPanel";
import { CONTROL_COMMANDS, useRmsWorkspace } from "./useRmsWorkspace";

export default function App() {
  const api = useMemo(() => createRmsApi(), []);
  const mockMode = useMemo(() => isMockMode(), []);
  const workspace = useRmsWorkspace(api);
  const viewerRef = useRef<RmsRerunViewerHandle>(null);
  const [preset, setPreset] = useState<ViewerPreset>("operations");
  const [selection, setSelection] = useState("");

  const handleSelection = useCallback((label: string) => setSelection(label), []);
  const handleMode = useCallback(workspace.setSessionMode, [workspace.setSessionMode]);
  const handleReady = useCallback(workspace.setViewerReady, [workspace.setViewerReady]);

  if (!workspace.project || !workspace.device || !workspace.dataSource) {
    return (
      <main className="app-loading">
        <span className="app-loading__mark">R</span>
        <strong>{workspace.error || "운영 화면을 준비하고 있습니다"}</strong>
      </main>
    );
  }

  const { device, dataSource } = workspace;

  return (
    <div className="app-shell">
      <ContextSidebar
        projects={workspace.projects}
        project={workspace.project}
        devices={workspace.devices}
        device={device}
        dataSources={workspace.dataSources}
        dataSource={dataSource}
        showMock={mockMode}
        onProjectChange={workspace.selectProject}
        onDeviceChange={workspace.selectDevice}
        onDataSourceChange={workspace.selectDataSource}
      />

      <main className="operations-shell">
        <StatusHeader
          device={device}
          source={dataSource}
          mode={workspace.sessionMode}
          lease={workspace.lease}
          eligibility={workspace.controlEligibility}
          busy={workspace.busyAction === "lease"}
          onRequestLease={workspace.requestLease}
          onReleaseLease={workspace.releaseLease}
          onReturnLive={workspace.switchToLive}
        />

        <div className="workspace-grid">
          <section className="viewer-workspace" aria-label="통합 Viewer">
            <div className="viewer-toolbar">
              <div className="viewer-toolbar__title">
                <Glyph name="cube" />
                <span>
                  <strong>{presetLabel(preset)}</strong>
                  <small>{dataSource.name}</small>
                </span>
              </div>
              <div className="viewer-toolbar__actions">
                {workspace.sessionMode === "paused" ? (
                  <button onClick={() => viewerRef.current?.returnToLatest()}>
                    <Glyph name="play" />
                    LIVE 복귀
                  </button>
                ) : dataSource.kind === "live" ? (
                  <button onClick={() => viewerRef.current?.setPlaying(false)}>
                    <Glyph name="pause" />
                    화면 일시정지
                  </button>
                ) : null}
              </div>
            </div>

            <div className="viewer-frame">
              <RmsRerunViewerAdapter
                key={dataSource.id}
                ref={viewerRef}
                source={dataSource}
                mode={workspace.sessionMode}
                onModeChange={handleMode}
                onReadyChange={handleReady}
                onSelectionChange={handleSelection}
              />
            </div>

            <div className="viewer-footer">
              <span className={`connection-state connection-state--${device.status}`}>
                <i />
                {device.status === "online" ? "데이터 정상" : "데이터 확인 필요"}
              </span>
              <span className="viewer-footer__selection">
                {selection ? `선택: ${compactSelection(selection)}` : "화면에서 대상을 선택할 수 있습니다."}
              </span>
              <span>Rerun 0.36.1</span>
            </div>
          </section>

          <TopicPanel
            device={device}
            topics={workspace.topics}
            preset={preset}
            onPresetChange={setPreset}
          />
        </div>

        <ControlDock
          device={device}
          source={dataSource}
          mode={workspace.sessionMode}
          lease={workspace.lease}
          eligibility={workspace.controlEligibility}
          commands={CONTROL_COMMANDS}
          busyAction={workspace.busyAction}
          onRequestLease={workspace.requestLease}
          onReturnLive={workspace.switchToLive}
          onCommand={workspace.sendCommand}
        />
      </main>

      {(workspace.error || workspace.notice) && (
        <div className={`toast ${workspace.error ? "toast--error" : "toast--success"}`} role="status">
          <span className="toast__icon">
            <Glyph name={workspace.error ? "state" : "shield"} />
          </span>
          <span>
            <strong>{workspace.error ? "요청을 처리할 수 없습니다" : "요청을 전달했습니다"}</strong>
            <small>{workspace.error || workspace.notice?.message}</small>
          </span>
          <button onClick={workspace.error ? workspace.clearError : workspace.clearNotice} aria-label="알림 닫기">
            ×
          </button>
        </div>
      )}
    </div>
  );
}

function presetLabel(preset: ViewerPreset): string {
  return {
    operations: "운영 Viewer",
    camera: "카메라 Viewer",
    diagnostics: "진단 Viewer",
  }[preset];
}

function compactSelection(selection: string): string {
  const parts = selection.split("/").filter(Boolean);
  return parts.slice(-2).join(" / ") || selection;
}
