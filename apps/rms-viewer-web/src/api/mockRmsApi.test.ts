import { describe, expect, it, vi } from "vitest";
import { MockRmsApi } from "./mockRmsApi";

describe("MockRmsApi control path", () => {
  it("uses the same lease and state guards as the HTTP contract", async () => {
    const api = new MockRmsApi();
    const lease = await api.requestControlLease("robot-07", 142);
    const receipt = await api.sendControlCommand({
      deviceId: "robot-07",
      commandType: "pause_mission",
      expectedDeviceVersion: 142,
      idempotencyKey: "test-idempotency-key",
      leaseId: lease.id,
      leaseEpoch: lease.epoch,
      sessionMode: "live",
      issuedAt: new Date().toISOString(),
      expiresAt: new Date(Date.now() + 3_000).toISOString(),
    });

    expect(receipt.status).toBe("accepted");
    expect(receipt.message).toContain("일시정지");
  });

  it("rejects commands from replay", async () => {
    const api = new MockRmsApi();
    const lease = await api.requestControlLease("robot-07", 142);

    await expect(
      api.sendControlCommand({
        deviceId: "robot-07",
        commandType: "pause_mission",
        expectedDeviceVersion: 142,
        idempotencyKey: "replay-command",
        leaseId: lease.id,
        leaseEpoch: lease.epoch,
        sessionMode: "replay",
        issuedAt: new Date().toISOString(),
        expiresAt: new Date(Date.now() + 3_000).toISOString(),
      }),
    ).rejects.toThrow("Replay");
  });

  it("streams topic updates through the same SSE-shaped contract", async () => {
    vi.useFakeTimers();
    try {
      const api = new MockRmsApi();
      const events: string[] = [];
      const unsubscribe = api.subscribeEvents(
        "project-logistics",
        "robot-07",
        "robot-07-live",
        (event) => events.push(event.type),
      );

      await vi.advanceTimersByTimeAsync(1_200);
      unsubscribe();

      expect(events).toEqual(["topic.value.changed"]);
    } finally {
      vi.useRealTimers();
    }
  });

  it("does not mix live topic updates into replay data", async () => {
    vi.useFakeTimers();
    try {
      const api = new MockRmsApi();
      const events: string[] = [];
      const unsubscribe = api.subscribeEvents(
        "project-logistics",
        "robot-07",
        "robot-07-incident",
        (event) => events.push(event.type),
      );

      await vi.advanceTimersByTimeAsync(2_400);
      unsubscribe();

      expect(events).toEqual([]);
    } finally {
      vi.useRealTimers();
    }
  });
});
