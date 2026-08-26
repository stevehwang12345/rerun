import { useCallback, useEffect, useMemo, useRef, useState } from "react";

import type { RmsApi } from "../api";
import {
  deviceKindLabel,
  type CandidateVerification,
  type DiscoveryCandidate,
  type DiscoveryCandidateStatus,
  type NetworkDiscoverySnapshot,
  type Project,
} from "../domain";
import { StatusBadge } from "../components/ProductShell";

export const NETWORK_DISCOVERY_FAILURE_MESSAGE = "네트워크 검색을 완료하지 못했습니다";
export const NETWORK_LINK_FAILURE_MESSAGE = "프로젝트에 연결하지 못했습니다";

interface ProjectTarget {
  project: Project;
  workspaceVersion: number;
}

type DiscoveryPhase =
  | "searching"
  | "results"
  | "verifying"
  | "review"
  | "approving"
  | "done"
  | "error";

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

export async function waitForNetworkDiscovery(
  api: RmsApi,
  sessionId: string,
  signal: AbortSignal,
  onUpdate: (snapshot: NetworkDiscoverySnapshot) => void,
  pollIntervalMs = 700,
): Promise<NetworkDiscoverySnapshot> {
  for (;;) {
    const snapshot = await api.discovery.getSnapshot(sessionId, { signal });
    onUpdate(snapshot);
    if (snapshot.session.status !== "searching") return snapshot;
    await abortableDelay(pollIntervalMs, signal);
  }
}

export function discoveryCandidateStatusLabel(status: DiscoveryCandidateStatus): string {
  return {
    found: "발견됨",
    verifying: "확인 중",
    verified: "확인됨",
    needs_attention: "확인 필요",
    unavailable: "사용할 수 없음",
    already_linked: "등록됨",
  }[status];
}

function candidateStatusTone(
  status: DiscoveryCandidateStatus,
): "normal" | "attention" | "restricted" | "neutral" {
  if (status === "verified" || status === "already_linked") return "normal";
  if (status === "unavailable") return "restricted";
  if (status === "needs_attention") return "attention";
  return "neutral";
}

function sourceCategoryLabel(category: CandidateVerification["sources"][number]["category"]): string {
  return {
    camera: "영상",
    spatial: "공간",
    telemetry: "상태",
    state: "상태",
    log: "기록",
  }[category];
}

export function NetworkDiscoveryEditor({
  api,
  organizationId,
  onCancel,
  onLinked,
  onOpenProjects,
}: {
  api: RmsApi;
  organizationId: string;
  onCancel: () => void;
  onLinked: () => void | Promise<void>;
  onOpenProjects: () => void;
}) {
  const [phase, setPhase] = useState<DiscoveryPhase>("searching");
  const [snapshot, setSnapshot] = useState<NetworkDiscoverySnapshot>();
  const [selectedCandidateId, setSelectedCandidateId] = useState("");
  const [verification, setVerification] = useState<CandidateVerification>();
  const [projectTargets, setProjectTargets] = useState<ProjectTarget[]>([]);
  const [projectsLoading, setProjectsLoading] = useState(true);
  const [projectsError, setProjectsError] = useState(false);
  const [projectId, setProjectId] = useState("");
  const [deviceName, setDeviceName] = useState("");
  const [selectedSourceIds, setSelectedSourceIds] = useState<Set<string>>(new Set());
  const [message, setMessage] = useState<string>();
  const controllerRef = useRef<AbortController | undefined>(undefined);
  const sessionIdRef = useRef<string | undefined>(undefined);
  const terminalRef = useRef(false);
  const projectLoadVersionRef = useRef(0);

  const loadProjects = useCallback(() => {
    const loadVersion = ++projectLoadVersionRef.current;
    setProjectsLoading(true);
    setProjectsError(false);
    void api.projects
      .listProjects()
      .then(async (projects) => {
        const availableProjects = projects.filter((project) => project.status !== "archived");
        const workspaces = await Promise.all(
          availableProjects.map((project) => api.projects.getWorkspace(project.id)),
        );
        if (projectLoadVersionRef.current !== loadVersion) return;
        setProjectTargets(
          workspaces.map((workspace) => ({
            project: workspace.project,
            workspaceVersion: workspace.snapshotVersion,
          })),
        );
      })
      .catch((cause: unknown) => {
        if (projectLoadVersionRef.current !== loadVersion) return;
        console.error("Failed to load network discovery projects", cause);
        setProjectsError(true);
      })
      .finally(() => {
        if (projectLoadVersionRef.current === loadVersion) setProjectsLoading(false);
      });
  }, [api]);

  const beginDiscovery = useCallback(() => {
    const previousSessionId = sessionIdRef.current;
    controllerRef.current?.abort();
    if (previousSessionId && !terminalRef.current) {
      void api.discovery.cancel(previousSessionId).catch((cause: unknown) => {
        console.error("Failed to cancel previous network discovery", cause);
      });
    }
    const controller = new AbortController();
    controllerRef.current = controller;
    sessionIdRef.current = undefined;
    terminalRef.current = false;
    setPhase("searching");
    setSnapshot(undefined);
    setSelectedCandidateId("");
    setVerification(undefined);
    setProjectId("");
    setDeviceName("");
    setSelectedSourceIds(new Set());
    setMessage(undefined);

    void api.discovery
      .start({ organizationId }, { signal: controller.signal })
      .then(async (session) => {
        sessionIdRef.current = session.id;
        if (controller.signal.aborted) {
          void api.discovery.cancel(session.id).catch(() => undefined);
          return;
        }
        setSnapshot({ session, candidates: [] });
        const completed = await waitForNetworkDiscovery(
          api,
          session.id,
          controller.signal,
          setSnapshot,
        );
        if (completed.session.status === "ready") {
          setPhase("results");
          return;
        }
        if (completed.session.status === "expired") {
          setMessage("검색 시간이 지났습니다");
        } else {
          setMessage(NETWORK_DISCOVERY_FAILURE_MESSAGE);
        }
        setPhase("error");
      })
      .catch((cause: unknown) => {
        if (isAbortError(cause)) return;
        console.error("Network discovery failed", cause);
        setMessage(NETWORK_DISCOVERY_FAILURE_MESSAGE);
        setPhase("error");
      });
  }, [api, organizationId]);

  useEffect(() => {
    loadProjects();
    beginDiscovery();
    return () => {
      projectLoadVersionRef.current += 1;
      controllerRef.current?.abort();
      const sessionId = sessionIdRef.current;
      if (sessionId && !terminalRef.current) {
        void api.discovery.cancel(sessionId).catch((cause: unknown) => {
          console.error("Failed to cancel network discovery during cleanup", cause);
        });
      }
    };
  }, [api, beginDiscovery, loadProjects]);

  const selectedCandidate = useMemo(
    () => snapshot?.candidates.find((candidate) => candidate.id === selectedCandidateId),
    [selectedCandidateId, snapshot?.candidates],
  );
  const selectedProject = useMemo(
    () => projectTargets.find((target) => target.project.id === projectId),
    [projectId, projectTargets],
  );

  const verifyCandidate = async () => {
    const sessionId = sessionIdRef.current;
    if (!sessionId || !selectedCandidate) return;
    setPhase("verifying");
    setMessage(undefined);
    try {
      const nextVerification = await api.discovery.verify(
        sessionId,
        selectedCandidate.id,
        { signal: controllerRef.current?.signal },
      );
      if (nextVerification.status !== "verified") {
        setMessage("이 장비는 지금 연결할 수 없습니다");
        setPhase("results");
        return;
      }
      setVerification(nextVerification);
      setDeviceName(nextVerification.suggestedDevice.name);
      setSelectedSourceIds(new Set());
      setProjectId("");
      setPhase("review");
    } catch (cause: unknown) {
      if (isAbortError(cause)) return;
      console.error("Network candidate verification failed", cause);
      setMessage("장비를 확인하지 못했습니다");
      setPhase("results");
    }
  };

  const approveCandidate = async () => {
    const sessionId = sessionIdRef.current;
    if (
      !sessionId ||
      !selectedCandidate ||
      !verification ||
      !selectedProject ||
      !deviceName.trim() ||
      selectedSourceIds.size === 0
    ) {
      return;
    }
    setPhase("approving");
    setMessage(undefined);
    try {
      await api.discovery.approve(
        sessionId,
        selectedCandidate.id,
        {
          verificationToken: verification.verificationToken,
          projectId: selectedProject.project.id,
          expectedWorkspaceVersion: selectedProject.workspaceVersion,
          deviceName: deviceName.trim(),
          selectedSourceIds: [...selectedSourceIds],
          accessMode: "observe",
          visibility: "operator",
        },
        { signal: controllerRef.current?.signal },
      );
      terminalRef.current = true;
      setPhase("done");
      try {
        await onLinked();
      } catch (cause: unknown) {
        console.error("Failed to refresh linked network resources", cause);
      }
    } catch (cause: unknown) {
      if (isAbortError(cause)) return;
      console.error("Network candidate approval failed", cause);
      setMessage(NETWORK_LINK_FAILURE_MESSAGE);
      setPhase("review");
      loadProjects();
    }
  };

  if (phase === "searching") {
    return <div className="discovery-placeholder" role="status">주변 장비를 찾는 중</div>;
  }

  if (phase === "error") {
    return (
      <div className="discovery-placeholder" role="alert">
        <span>{message ?? NETWORK_DISCOVERY_FAILURE_MESSAGE}</span>
        <button type="button" className="button" onClick={beginDiscovery}>다시 찾기</button>
      </div>
    );
  }

  if (phase === "done") {
    return (
      <div className="discovery-placeholder" role="status">
        <strong>프로젝트에 연결했습니다</strong>
        <div className="form-actions">
          <button type="button" className="button" onClick={onCancel}>닫기</button>
          <button type="button" className="button button--primary" onClick={onOpenProjects}>
            프로젝트 열기
          </button>
        </div>
      </div>
    );
  }

  if (phase === "results" || phase === "verifying") {
    const candidates = snapshot?.candidates ?? [];
    return (
      <div className="form-stack discovery-form">
        <div className="discovery-heading">
          <strong>{candidates.length > 0 ? `${candidates.length}개 찾음` : "찾은 장비가 없습니다"}</strong>
          <button type="button" className="text-button" onClick={beginDiscovery} disabled={phase === "verifying"}>
            다시 찾기
          </button>
        </div>
        <fieldset className="discovery-candidates" disabled={phase === "verifying"}>
          <legend className="visually-hidden">연결할 장비</legend>
          {candidates.map((candidate) => (
            <label className="discovery-candidate" key={candidate.id}>
              <input
                type="radio"
                name="network-candidate"
                value={candidate.id}
                checked={selectedCandidateId === candidate.id}
                onChange={() => {
                  setSelectedCandidateId(candidate.id);
                  setMessage(undefined);
                }}
                disabled={candidate.status === "unavailable" || candidate.status === "already_linked"}
              />
              <span className="discovery-candidate__identity">
                <strong>{candidate.displayName}</strong>
                <span>{deviceKindLabel(candidate.category)}</span>
              </span>
              <StatusBadge
                label={discoveryCandidateStatusLabel(candidate.status)}
                tone={candidateStatusTone(candidate.status)}
              />
            </label>
          ))}
        </fieldset>
        {message && <span className="form-error" role="alert">{message}</span>}
        <div className="form-actions">
          <button type="button" className="button" onClick={onCancel}>취소</button>
          <button
            type="button"
            className="button button--primary"
            disabled={!selectedCandidate || phase === "verifying"}
            onClick={() => void verifyCandidate()}
          >
            {phase === "verifying" ? "확인 중" : "연결 확인"}
          </button>
        </div>
      </div>
    );
  }

  const readySources = verification?.sources.filter((source) => source.status === "ready") ?? [];
  const canApprove =
    phase !== "approving" &&
    !projectsLoading &&
    !projectsError &&
    selectedProject != null &&
    deviceName.trim().length > 0 &&
    selectedSourceIds.size > 0;

  return (
    <form
      className="form-stack discovery-form"
      onSubmit={(event) => {
        event.preventDefault();
        void approveCandidate();
      }}
    >
      <div className="discovery-review__device">
        <strong>{selectedCandidate?.displayName}</strong>
        <StatusBadge label="확인됨" tone="normal" />
      </div>
      <label>
        프로젝트
        <select
          value={projectId}
          onChange={(event) => setProjectId(event.target.value)}
          disabled={projectsLoading || projectsError || phase === "approving"}
        >
          <option value="">프로젝트 선택</option>
          {projectTargets.map((target) => (
            <option key={target.project.id} value={target.project.id}>{target.project.name}</option>
          ))}
        </select>
      </label>
      <label>
        장비명
        <input
          value={deviceName}
          onChange={(event) => setDeviceName(event.target.value)}
          disabled={phase === "approving"}
        />
      </label>
      <fieldset className="discovery-sources" disabled={phase === "approving"}>
        <legend>데이터</legend>
        {readySources.map((source) => (
          <label key={source.id}>
            <input
              type="checkbox"
              checked={selectedSourceIds.has(source.id)}
              onChange={(event) => {
                setSelectedSourceIds((current) => {
                  const next = new Set(current);
                  if (event.target.checked) next.add(source.id);
                  else next.delete(source.id);
                  return next;
                });
              }}
            />
            <span><strong>{source.label}</strong><small>{sourceCategoryLabel(source.category)}</small></span>
          </label>
        ))}
      </fieldset>
      <div className="discovery-access"><span>권한</span><strong>관찰</strong></div>
      {projectsLoading && <span className="form-hint" role="status">프로젝트 확인 중</span>}
      {projectsError && (
        <span className="form-error" role="alert">
          프로젝트를 불러오지 못했습니다
          <button type="button" className="text-button" onClick={loadProjects}>다시 시도</button>
        </span>
      )}
      {!projectsLoading && !projectsError && projectTargets.length === 0 && (
        <span className="form-error" role="alert">연결할 프로젝트가 없습니다</span>
      )}
      {message && <span className="form-error" role="alert">{message}</span>}
      <div className="form-actions">
        <button
          type="button"
          className="button"
          disabled={phase === "approving"}
          onClick={() => {
            setVerification(undefined);
            setSelectedSourceIds(new Set());
            setPhase("results");
            setMessage(undefined);
          }}
        >
          이전
        </button>
        <button type="submit" className="button button--primary" disabled={!canApprove}>
          {phase === "approving" ? "연결 중" : "프로젝트에 연결"}
        </button>
      </div>
    </form>
  );
}
