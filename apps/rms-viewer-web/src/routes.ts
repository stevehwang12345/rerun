export type RmsRoute =
  | { kind: "integrations" }
  | { kind: "projects"; projectId?: string }
  | { kind: "live-index" }
  | {
      kind: "live";
      projectId: string;
      deviceId: string;
      liveSessionId?: string;
      dataSourceId?: string;
    }
  | { kind: "replay-index" }
  | {
      kind: "replay";
      projectId: string;
      recordingId: string;
      replaySessionId?: string;
    };

function segment(value: string): string {
  return encodeURIComponent(value);
}

function decode(value: string | undefined): string | undefined {
  if (!value) {
    return undefined;
  }

  try {
    return decodeURIComponent(value);
  } catch {
    return undefined;
  }
}

export function integrationsPath(): string {
  return "/integrations";
}

export function projectsPath(projectId?: string): string {
  return projectId ? `/projects/${segment(projectId)}` : "/projects";
}

export function liveIndexPath(): string {
  return "/live";
}

export function replayIndexPath(): string {
  return "/replay";
}

export function livePath(
  projectId: string,
  deviceId: string,
  liveSessionId?: string,
  dataSourceId?: string,
): string {
  const base = `/projects/${segment(projectId)}/live/${segment(deviceId)}`;
  const path = liveSessionId ? `${base}/${segment(liveSessionId)}` : base;
  return dataSourceId ? `${path}?${new URLSearchParams({ source: dataSourceId })}` : path;
}

export function replayPath(
  projectId: string,
  recordingId: string,
  replaySessionId?: string,
): string {
  const base = `/projects/${segment(projectId)}/replay/${segment(recordingId)}`;
  return replaySessionId ? `${base}/${segment(replaySessionId)}` : base;
}

export function parseRoute(pathname: string, search = ""): RmsRoute {
  const parts = pathname.split("/").filter(Boolean);
  if (parts.length === 1 && parts[0] === "integrations") {
    return { kind: "integrations" };
  }

  if (parts.length === 1 && parts[0] === "live") {
    return { kind: "live-index" };
  }

  if (parts.length === 1 && parts[0] === "replay") {
    return { kind: "replay-index" };
  }

  if (parts[0] !== "projects") {
    return { kind: "projects" };
  }

  const projectId = decode(parts[1]);
  if (!projectId) {
    return { kind: "projects" };
  }

  if (parts.length === 2) {
    return { kind: "projects", projectId };
  }

  if (parts[2] === "live") {
    const deviceId = decode(parts[3]);
    if (deviceId) {
      return {
        kind: "live",
        projectId,
        deviceId,
        liveSessionId: decode(parts[4]),
        dataSourceId: new URLSearchParams(search).get("source") ?? undefined,
      };
    }
  }

  if (parts[2] === "replay") {
    const recordingId = decode(parts[3]);
    if (recordingId) {
      return {
        kind: "replay",
        projectId,
        recordingId,
        replaySessionId: decode(parts[4]),
      };
    }
  }

  return { kind: "projects", projectId };
}

export function routeSection(route: RmsRoute): "integrations" | "projects" | "live" | "replay" {
  if (route.kind === "live-index") {
    return "live";
  }
  if (route.kind === "replay-index") {
    return "replay";
  }
  return route.kind;
}
