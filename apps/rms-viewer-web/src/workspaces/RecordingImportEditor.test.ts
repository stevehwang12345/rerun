import { describe, expect, it, vi } from "vitest";

import { MockRmsApi } from "../api";
import {
  detectRecordingImportFormat,
  waitForRecordingImport,
} from "./RecordingImportEditor";

describe("Recording import file detection", () => {
  it.each([
    ["scene.RRD", "", "rrd"],
    ["drive.mcap", "application/octet-stream", "mcap"],
    ["rosbag.zip", "application/zip", "ros2-bag-zip"],
    ["telemetry.csv", "text/csv", "csv"],
    ["camera.mp4", "video/mp4", "video"],
    ["camera.MOV", "video/quicktime", "video"],
    ["camera.webm", "video/webm", "video"],
  ] as const)("detects %s", (name, type, expected) => {
    expect(detectRecordingImportFormat({ name, type })).toBe(expected);
  });

  it("rejects a file outside the supported import formats", () => {
    expect(detectRecordingImportFormat({ name: "archive.tar.gz", type: "" })).toBeUndefined();
    expect(
      detectRecordingImportFormat({ name: "camera.avi", type: "video/x-msvideo" }),
    ).toBeUndefined();
  });
});

describe("Recording import lifecycle", () => {
  it("uploads, processes, and produces a Replay-ready Recording", async () => {
    const api = new MockRmsApi();
    const progress: number[] = [];
    const created = await api.recordingImports.createImport(
      {
        projectId: "project-logistics",
        deviceId: "robot-07",
        file: new File(["mcap"], "drive.mcap", { type: "application/octet-stream" }),
        format: "mcap",
      },
      { onProgress: (value) => progress.push(value) },
    );

    expect(created).toMatchObject({
      projectId: "project-logistics",
      deviceId: "robot-07",
      format: "mcap",
      status: "processing",
    });
    expect(created.dataSourceId).toBe("import-source-project-logistics-robot-07");
    expect(progress).toEqual([0, 100]);

    const onUpdate = vi.fn();
    const completed = await waitForRecordingImport(
      api,
      created.id,
      new AbortController().signal,
      onUpdate,
      0,
    );

    expect(completed).toMatchObject({
      status: "ready",
      progressPercent: 100,
      recordingId: expect.any(String),
    });
    expect(onUpdate).toHaveBeenCalledWith(completed);
    const workspace = await api.projects.getWorkspace("project-logistics");
    expect(workspace.recordings.some((recording) => recording.id === completed.recordingId)).toBe(
      true,
    );
  });

  it("stops polling immediately when its owner aborts", async () => {
    const api = new MockRmsApi();
    const controller = new AbortController();
    controller.abort();

    await expect(
      waitForRecordingImport(api, "unused", controller.signal, () => undefined, 0),
    ).rejects.toMatchObject({ name: "AbortError" });
  });
});
