import { describe, expect, it } from "vitest";

import {
  integrationsPath,
  liveIndexPath,
  livePath,
  parseRoute,
  projectsPath,
  replayIndexPath,
  replayPath,
} from "./routes";

describe("RMS routes", () => {
  it("builds and parses each workspace route", () => {
    expect(parseRoute(integrationsPath())).toEqual({ kind: "integrations" });
    expect(parseRoute(liveIndexPath())).toEqual({ kind: "live-index" });
    expect(parseRoute(replayIndexPath())).toEqual({ kind: "replay-index" });
    expect(parseRoute(projectsPath("project-logistics"))).toEqual({
      kind: "projects",
      projectId: "project-logistics",
    });
    const liveUrl = new URL(
      livePath("project-logistics", "robot-07", "live-142", "source/front"),
      "https://rms.invalid",
    );
    expect(parseRoute(liveUrl.pathname, liveUrl.search)).toEqual({
      kind: "live",
      projectId: "project-logistics",
      deviceId: "robot-07",
      liveSessionId: "live-142",
      dataSourceId: "source/front",
    });
    expect(
      parseRoute(replayPath("project-logistics", "recording/incident", "replay-7")),
    ).toEqual({
      kind: "replay",
      projectId: "project-logistics",
      recordingId: "recording/incident",
      replaySessionId: "replay-7",
    });
  });

  it("falls back to projects for unknown or incomplete paths", () => {
    expect(parseRoute("/")).toEqual({ kind: "projects" });
    expect(parseRoute("/unknown")).toEqual({ kind: "projects" });
    expect(parseRoute("/projects/project-logistics/live")).toEqual({
      kind: "projects",
      projectId: "project-logistics",
    });
  });
});
