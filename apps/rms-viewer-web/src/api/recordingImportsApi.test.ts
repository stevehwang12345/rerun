import { afterEach, describe, expect, it, vi } from "vitest";

import type { RecordingImport } from "../domain";
import { HttpRmsApi } from "./rmsApi";

const importResponse: RecordingImport = {
  id: "import-1",
  projectId: "project-1",
  deviceId: "robot-07",
  dataSourceId: "import-source-1",
  fileName: "drive.mcap",
  format: "mcap",
  status: "processing",
  progressPercent: 0,
  sizeBytes: 4,
  createdAt: "2026-08-20T00:00:00Z",
  updatedAt: "2026-08-20T00:00:00Z",
  resourceVersion: 4,
};

class FakeXmlHttpRequest {
  static instances: FakeXmlHttpRequest[] = [];
  static autoRespond = true;
  static responseStatus = 202;
  static responseText = JSON.stringify(importResponse);

  method = "";
  url = "";
  withCredentials = false;
  status = 0;
  responseText = "";
  body?: Document | XMLHttpRequestBodyInit | null;
  aborted = false;
  readonly headers = new Map<string, string>();
  readonly upload: {
    onprogress: ((event: ProgressEvent) => void) | null;
  } = { onprogress: null };
  onload: (() => void) | null = null;
  onerror: (() => void) | null = null;
  onabort: (() => void) | null = null;

  constructor() {
    FakeXmlHttpRequest.instances.push(this);
  }

  open(method: string, url: string): void {
    this.method = method;
    this.url = url;
  }

  setRequestHeader(name: string, value: string): void {
    this.headers.set(name, value);
  }

  send(body?: Document | XMLHttpRequestBodyInit | null): void {
    this.body = body;
    this.upload.onprogress?.({
      lengthComputable: true,
      loaded: 2,
      total: 4,
    } as ProgressEvent);
    if (!FakeXmlHttpRequest.autoRespond) return;
    queueMicrotask(() => {
      if (this.aborted) return;
      this.status = FakeXmlHttpRequest.responseStatus;
      this.responseText = FakeXmlHttpRequest.responseText;
      this.onload?.();
    });
  }

  abort(): void {
    this.aborted = true;
    this.onabort?.();
  }
}

afterEach(() => {
  FakeXmlHttpRequest.instances = [];
  FakeXmlHttpRequest.autoRespond = true;
  FakeXmlHttpRequest.responseStatus = 202;
  FakeXmlHttpRequest.responseText = JSON.stringify(importResponse);
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

describe("HttpRmsApi Recording Import contract", () => {
  it("uploads multipart camelCase fields and reports XHR progress", async () => {
    vi.stubGlobal("XMLHttpRequest", FakeXmlHttpRequest);
    const progress: number[] = [];
    const api = new HttpRmsApi("/api/");

    const result = await api.recordingImports.createImport(
      {
        projectId: "project-1",
        deviceId: "robot-07",
        file: new File(["mcap"], "drive.mcap", { type: "application/octet-stream" }),
        format: "mcap",
      },
      { onProgress: (value) => progress.push(value) },
    );

    expect(result).toEqual(importResponse);
    const request = FakeXmlHttpRequest.instances[0];
    expect(request.method).toBe("POST");
    expect(request.url).toBe("/api/v1/recording-imports");
    expect(request.withCredentials).toBe(true);
    expect(request.headers.get("Idempotency-Key")).toBeTruthy();
    expect(request.headers.get("X-RMS-Request-ID")).toBeTruthy();
    expect(request.headers.has("Content-Type")).toBe(false);
    const form = request.body as FormData;
    expect(form.get("projectId")).toBe("project-1");
    expect(form.get("deviceId")).toBe("robot-07");
    expect(form.get("format")).toBe("mcap");
    expect((form.get("file") as File).name).toBe("drive.mcap");
    expect(Array.from(form.keys())).toEqual([
      "projectId",
      "deviceId",
      "format",
      "file",
    ]);
    expect(progress).toEqual([0, 50]);
  });

  it("aborts the in-flight XHR when its owner is disposed", async () => {
    FakeXmlHttpRequest.autoRespond = false;
    vi.stubGlobal("XMLHttpRequest", FakeXmlHttpRequest);
    const controller = new AbortController();
    const api = new HttpRmsApi("/api");
    const request = api.recordingImports.createImport(
      {
        projectId: "project-1",
        deviceId: "robot-07",
        file: new File(["rrd"], "scene.rrd"),
        format: "rrd",
      },
      { signal: controller.signal },
    );

    controller.abort();

    await expect(request).rejects.toMatchObject({ name: "AbortError" });
    expect(FakeXmlHttpRequest.instances[0]?.aborted).toBe(true);
  });

  it("does not expose a technical upload response to the caller", async () => {
    FakeXmlHttpRequest.responseStatus = 500;
    FakeXmlHttpRequest.responseText = "database connection refused at 10.0.0.7";
    vi.stubGlobal("XMLHttpRequest", FakeXmlHttpRequest);
    vi.spyOn(console, "error").mockImplementation(() => undefined);
    const api = new HttpRmsApi("/api");

    await expect(
      api.recordingImports.createImport({
        projectId: "project-1",
        deviceId: "robot-07",
        file: new File(["rrd"], "scene.rrd"),
        format: "rrd",
      }),
    ).rejects.toThrow("파일을 가져오지 못했습니다");
  });
});
