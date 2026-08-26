// @vitest-environment jsdom

import "@testing-library/jest-dom/vitest";
import { cleanup, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

import { MockRmsApi } from "../api";
import { IntegrationsWorkspace } from "./IntegrationsWorkspace";
import {
  NETWORK_DISCOVERY_FAILURE_MESSAGE,
  NetworkDiscoveryEditor,
} from "./NetworkDiscoveryEditor";

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
});

describe("NetworkDiscoveryEditor user approval flow", () => {
  it("does not search on workspace load and never verifies or links without explicit clicks", async () => {
    const user = userEvent.setup();
    const api = new MockRmsApi({ latencyMs: 20 });
    const start = vi.spyOn(api.discovery, "start");
    const verify = vi.spyOn(api.discovery, "verify");
    const approve = vi.spyOn(api.discovery, "approve");
    const createLiveSession = vi.spyOn(api.live, "createSession");
    const requestLease = vi.spyOn(api.control, "requestLease");

    render(
      <IntegrationsWorkspace
        api={api}
        onOpenProjects={vi.fn()}
        onOpenReplay={vi.fn()}
      />,
    );

    await screen.findByRole("heading", { name: "데이터와 장비 연동" });
    await screen.findByText("Robot-07");
    expect(document.body.textContent).toContain("로봇 데이터");
    expect(document.body.textContent).toContain("비행 데이터");
    expect(document.body.textContent).not.toContain("A동 Edge Gateway");
    expect(document.body.textContent).not.toContain("야외 Drone Gateway");
    expect(document.body.textContent).not.toContain("ROS 2 + Rerun");
    expect(document.body.textContent).not.toContain("MAVLink + Rerun");
    expect(document.body.textContent).not.toContain("mapping v");
    expect(start).not.toHaveBeenCalled();
    expect(verify).not.toHaveBeenCalled();
    expect(approve).not.toHaveBeenCalled();

    await user.click(screen.getByRole("button", { name: "네트워크에서 찾기…" }));
    expect(screen.getByRole("status")).toHaveTextContent("주변 장비를 찾는 중");
    const robotChoice = await screen.findByRole("radio", { name: /Robot-24/ });
    const dialog = screen.getByRole("dialog", { name: "네트워크에서 찾기" });
    expect(start).toHaveBeenCalledTimes(1);
    expect(robotChoice).not.toBeChecked();
    expect(screen.getByRole("button", { name: "연결 확인" })).toBeDisabled();
    expect(verify).not.toHaveBeenCalled();
    expect(approve).not.toHaveBeenCalled();
    expect(dialog.textContent).not.toMatch(
      /(?:ROS\s?2|RTSP|MAVLink|mapping|\b\d{1,3}(?:\.\d{1,3}){3}\b|:\d{2,5})/i,
    );

    await user.click(robotChoice);
    expect(screen.getByRole("button", { name: "연결 확인" })).toBeEnabled();
    expect(verify).not.toHaveBeenCalled();
    await user.click(screen.getByRole("button", { name: "연결 확인" }));

    const projectSelect = await screen.findByRole("combobox", { name: "프로젝트" });
    const sourceChoice = screen.getByRole("checkbox", { name: /위치와 주변/ });
    const linkButton = screen.getByRole("button", { name: "프로젝트에 연결" });
    expect(verify).toHaveBeenCalledTimes(1);
    expect(projectSelect).toHaveValue("");
    expect(sourceChoice).not.toBeChecked();
    expect(linkButton).toBeDisabled();
    expect(approve).not.toHaveBeenCalled();

    await user.selectOptions(projectSelect, "project-logistics");
    await user.click(sourceChoice);
    expect(linkButton).toBeEnabled();
    expect(approve).not.toHaveBeenCalled();
    await user.click(linkButton);

    await screen.findByText("프로젝트에 연결했습니다");
    expect(approve).toHaveBeenCalledTimes(1);
    expect(approve.mock.calls[0]?.[2]).toMatchObject({
      projectId: "project-logistics",
      accessMode: "observe",
      visibility: "operator",
      selectedSourceIds: [expect.any(String)],
    });
    expect(createLiveSession).not.toHaveBeenCalled();
    expect(requestLease).not.toHaveBeenCalled();

    await user.click(within(dialog).getAllByRole("button", { name: "닫기" })[1]!);
    const deviceRow = (await screen.findByText("Robot-24")).closest(".resource-row");
    expect(deviceRow).not.toBeNull();
    expect(deviceRow?.textContent).toContain("로봇");
    expect(deviceRow?.textContent).toContain("확인 필요");
    const sourceRow = screen.getByText("위치와 주변").closest(".resource-row");
    expect(sourceRow?.textContent).toContain("연결 확인 중");
    expect(sourceRow?.textContent).toContain("확인 필요");
  });

  it("cancels the temporary session when the dialog closes", async () => {
    const user = userEvent.setup();
    const api = new MockRmsApi();
    const cancel = vi.spyOn(api.discovery, "cancel");

    render(
      <IntegrationsWorkspace
        api={api}
        onOpenProjects={vi.fn()}
        onOpenReplay={vi.fn()}
      />,
    );
    await screen.findByText("Robot-07");
    await user.click(screen.getByRole("button", { name: "네트워크에서 찾기…" }));
    await screen.findByRole("radio", { name: /Robot-24/ });
    await user.click(screen.getByRole("button", { name: "닫기" }));

    await waitFor(() => expect(cancel).toHaveBeenCalledTimes(1));
  });

  it("keeps raw discovery failures out of the visible and accessible UI", async () => {
    const api = new MockRmsApi();
    vi.spyOn(console, "error").mockImplementation(() => undefined);
    vi.spyOn(api.discovery, "start").mockRejectedValue(
      new Error("connection refused at 10.0.0.7:8554 over rtsp"),
    );

    render(
      <NetworkDiscoveryEditor
        api={api}
        organizationId="organization-rms"
        onCancel={vi.fn()}
        onLinked={vi.fn()}
        onOpenProjects={vi.fn()}
      />,
    );

    expect(await screen.findByRole("alert")).toHaveTextContent(
      NETWORK_DISCOVERY_FAILURE_MESSAGE,
    );
    expect(document.body.textContent).not.toContain("10.0.0.7");
    expect(document.body.textContent).not.toContain("8554");
    expect(document.body.textContent?.toLowerCase()).not.toContain("rtsp");
  });
});
