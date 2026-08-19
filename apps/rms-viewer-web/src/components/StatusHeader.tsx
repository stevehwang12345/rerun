import type { ControlEligibility, ControlLease, DataSource, Device, SessionMode } from "../domain";
import { healthLabel } from "../domain";
import { Glyph } from "./Glyph";

interface Props {
  device: Device;
  source: DataSource;
  mode: SessionMode;
  lease: ControlLease | null;
  eligibility: ControlEligibility;
  busy: boolean;
  onRequestLease(): void;
  onReleaseLease(): void;
  onReturnLive(): void;
}
export function StatusHeader({
  device,
  source,
  mode,
  lease,
  eligibility,
  busy,
  onRequestLease,
  onReleaseLease,
  onReturnLive,
}: Props) {
  const isReplay = source.kind === "recording" || mode === "replay";

  return (
    <header className="status-header">
      <div className="status-header__identity">
        <span className="device-kind">
          <Glyph name="robot" />
        </span>
        <span>
          <strong>{device.name}</strong>
          <small>{device.operationMode}</small>
        </span>
      </div>

      <div className="status-header__signals">
        <span className={`mode-badge mode-badge--${isReplay ? "replay" : mode}`}>
          <i />
          {isReplay ? "REPLAY" : mode === "paused" ? "일시정지" : "LIVE"}
        </span>
        <span className={`health-badge health-badge--${device.health}`}>
          {healthLabel(device.health)}
        </span>
        {device.batteryPercent != null && (
          <span className="battery-label">배터리 {device.batteryPercent}%</span>
        )}
      </div>

      <div className="status-header__task">
        <small>현재 작업</small>
        <strong>{device.taskName}</strong>
        <span className="task-progress" aria-label={`작업 진행 ${device.taskProgress}%`}>
          <i style={{ width: `${device.taskProgress}%` }} />
        </span>
      </div>

      <div className="status-header__control">
        {isReplay ? (
          <button className="button button--primary" onClick={onReturnLive}>
            LIVE로 이동
          </button>
        ) : lease ? (
          <button className="lease-button" onClick={onReleaseLease} disabled={busy}>
            <Glyph name="shield" />
            <span>
              <strong>제어권: 나</strong>
              <small>{eligibility.allowed ? "제어 가능" : eligibility.reason}</small>
            </span>
            <span className="lease-button__release">반납</span>
          </button>
        ) : (
          <button className="button button--primary" onClick={onRequestLease} disabled={busy}>
            <Glyph name="shield" />
            {busy ? "요청 중" : "제어권 요청"}
          </button>
        )}
      </div>
    </header>
  );
}
