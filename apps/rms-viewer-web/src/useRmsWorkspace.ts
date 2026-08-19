import { useCallback, useEffect, useMemo, useState } from "react";
import type { RmsApi } from "./api";
import {
  OPERATOR_ID,
  deriveControlEligibility,
  sourceMode,
  type CommandReceipt,
  type ControlCommandDefinition,
  type ControlLease,
  type DataSource,
  type Device,
  type Project,
  type SessionMode,
  type Topic,
} from "./domain";

export const CONTROL_COMMANDS: ControlCommandDefinition[] = [
  {
    type: "pause_mission",
    label: "일시정지",
    risk: "medium",
    description: "현재 Mission을 안전한 위치에서 멈춥니다.",
  },
  {
    type: "resume_mission",
    label: "계속 진행",
    risk: "medium",
    description: "일시정지한 Mission을 계속 진행합니다.",
  },
  {
    type: "safe_stop",
    label: "안전 정지",
    risk: "emergency",
    description: "장비가 현재 위치에서 안전 정지를 수행합니다.",
  },
];

export function useRmsWorkspace(api: RmsApi) {
  const [projects, setProjects] = useState<Project[]>([]);
  const [devices, setDevices] = useState<Device[]>([]);
  const [dataSources, setDataSources] = useState<DataSource[]>([]);
  const [topics, setTopics] = useState<Topic[]>([]);
  const [projectId, setProjectId] = useState("");
  const [deviceId, setDeviceId] = useState("");
  const [dataSourceId, setDataSourceId] = useState("");
  const [lease, setLease] = useState<ControlLease | null>(null);
  const [viewerReady, setViewerReady] = useState(false);
  const [sessionMode, setSessionMode] = useState<SessionMode>("live");
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState("");
  const [notice, setNotice] = useState<CommandReceipt | null>(null);
  const [busyAction, setBusyAction] = useState<string | null>(null);
  const [eligibilityEpoch, setEligibilityEpoch] = useState(0);

  const project = projects.find((item) => item.id === projectId) ?? null;
  const device = devices.find((item) => item.id === deviceId) ?? null;
  const dataSource =
    dataSources.find((item) => item.id === dataSourceId) ?? dataSources[0] ?? null;

  useEffect(() => {
    let active = true;
    setLoading(true);
    api
      .listProjects()
      .then((items) => {
        if (!active) return;
        setProjects(items);
        setProjectId((current) => current || items[0]?.id || "");
      })
      .catch((reason: unknown) => setError(messageOf(reason)))
      .finally(() => active && setLoading(false));
    return () => {
      active = false;
    };
  }, [api]);

  useEffect(() => {
    if (!projectId) return;
    let active = true;
    setLoading(true);
    setDevices([]);
    setDataSources([]);
    setTopics([]);
    api
      .listDevices(projectId)
      .then((items) => {
        if (!active) return;
        setDevices(items);
        setDeviceId(items[0]?.id || "");
      })
      .catch((reason: unknown) => setError(messageOf(reason)))
      .finally(() => active && setLoading(false));
    return () => {
      active = false;
    };
  }, [api, projectId]);

  useEffect(() => {
    if (!projectId || !deviceId) return;
    let active = true;
    setLoading(true);
    setViewerReady(false);
    setTopics([]);
    Promise.all([
      api.listDataSources(projectId, deviceId),
      api.getControlLease(deviceId),
    ])
      .then(([items, currentLease]) => {
        if (!active) return;
        setDataSources(items);
        const preferred = items.find((source) => source.kind === "live") ?? items[0];
        setDataSourceId(preferred?.id || "");
        setSessionMode(preferred ? sourceMode(preferred) : "replay");
        setLease(currentLease);
      })
      .catch((reason: unknown) => setError(messageOf(reason)))
      .finally(() => active && setLoading(false));
    return () => {
      active = false;
    };
  }, [api, deviceId, projectId]);

  useEffect(() => {
    if (!deviceId || !dataSourceId) return;
    let active = true;
    setViewerReady(false);
    api
      .listTopics(deviceId, dataSourceId)
      .then((items) => active && setTopics(items))
      .catch((reason: unknown) => setError(messageOf(reason)));
    return () => {
      active = false;
    };
  }, [api, dataSourceId, deviceId]);

  useEffect(() => {
    if (!projectId || !deviceId || !dataSourceId) return;
    return api.subscribeEvents(projectId, deviceId, dataSourceId, (event) => {
      if (event.type === "topic.value.changed") {
        setTopics((current) =>
          current.map((topic) =>
            topic.id === event.data.id ? { ...topic, ...event.data } : topic,
          ),
        );
      } else if (event.type === "device.state.changed") {
        setDevices((current) =>
          current.map((item) =>
            item.id === event.data.id ? { ...item, ...event.data } : item,
          ),
        );
      } else if (event.type === "control.lease.changed") {
        setLease(event.data.lease);
      } else if (event.type === "command.state.changed") {
        setNotice(event.data);
      }
    });
  }, [api, dataSourceId, deviceId, projectId]);

  useEffect(() => {
    if (!lease) return;
    const remainingMs = Date.parse(lease.expiresAt) - Date.now();
    if (remainingMs <= 0) {
      setEligibilityEpoch((current) => current + 1);
      return;
    }
    const timer = globalThis.setTimeout(
      () => setEligibilityEpoch((current) => current + 1),
      remainingMs + 25,
    );
    return () => globalThis.clearTimeout(timer);
  }, [lease]);

  const selectProject = useCallback((nextProjectId: string) => {
    setError("");
    setNotice(null);
    setProjectId(nextProjectId);
  }, []);

  const selectDevice = useCallback((nextDeviceId: string) => {
    setError("");
    setNotice(null);
    setDeviceId(nextDeviceId);
  }, []);

  const selectDataSource = useCallback(
    (nextDataSourceId: string) => {
      const nextSource = dataSources.find((source) => source.id === nextDataSourceId);
      setDataSourceId(nextDataSourceId);
      setSessionMode(nextSource ? sourceMode(nextSource) : "replay");
      setViewerReady(false);
      setNotice(null);
      setError("");
    },
    [dataSources],
  );

  const switchToLive = useCallback(() => {
    const liveSource = dataSources.find((source) => source.kind === "live");
    if (liveSource) {
      selectDataSource(liveSource.id);
    }
  }, [dataSources, selectDataSource]);

  const requestLease = useCallback(async () => {
    if (!device) return;
    setBusyAction("lease");
    setError("");
    try {
      setLease(await api.requestControlLease(device.id, device.stateVersion));
    } catch (reason) {
      setError(messageOf(reason));
    } finally {
      setBusyAction(null);
    }
  }, [api, device]);

  const releaseLease = useCallback(async () => {
    if (!device || !lease) return;
    setBusyAction("lease");
    setError("");
    try {
      await api.releaseControlLease(device.id, lease.id);
      setLease(null);
    } catch (reason) {
      setError(messageOf(reason));
    } finally {
      setBusyAction(null);
    }
  }, [api, device, lease]);

  const controlEligibility = useMemo(() => {
    if (!device || !dataSource) {
      return { allowed: false, reason: "장비를 선택해 주세요." };
    }
    return deriveControlEligibility({
      device,
      source: dataSource,
      mode: sessionMode,
      lease,
      operatorId: OPERATOR_ID,
      viewerReady,
    });
  }, [dataSource, device, eligibilityEpoch, lease, sessionMode, viewerReady]);

  const sendCommand = useCallback(
    async (definition: ControlCommandDefinition) => {
      if (!device || !lease || !controlEligibility.allowed) return;
      setBusyAction(definition.type);
      setError("");
      setNotice(null);
      try {
        const issuedAt = new Date();
        const receipt = await api.sendControlCommand({
          deviceId: device.id,
          commandType: definition.type,
          expectedDeviceVersion: device.stateVersion,
          idempotencyKey: crypto.randomUUID(),
          leaseId: lease.id,
          leaseEpoch: lease.epoch,
          sessionMode,
          issuedAt: issuedAt.toISOString(),
          expiresAt: new Date(issuedAt.getTime() + 3_000).toISOString(),
        });
        setNotice(receipt);
      } catch (reason) {
        setError(messageOf(reason));
      } finally {
        setBusyAction(null);
      }
    },
    [api, controlEligibility.allowed, device, lease, sessionMode],
  );

  return {
    projects,
    devices,
    dataSources,
    topics,
    project,
    device,
    dataSource,
    lease,
    sessionMode,
    viewerReady,
    loading,
    error,
    notice,
    busyAction,
    controlEligibility,
    selectProject,
    selectDevice,
    selectDataSource,
    switchToLive,
    requestLease,
    releaseLease,
    sendCommand,
    setSessionMode,
    setViewerReady,
    clearError: () => setError(""),
    clearNotice: () => setNotice(null),
  };
}

function messageOf(reason: unknown): string {
  return reason instanceof Error ? reason.message : "요청을 처리하지 못했습니다.";
}
