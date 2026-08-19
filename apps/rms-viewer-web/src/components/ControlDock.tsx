import { useState } from "react";
import type {
  ControlCommandDefinition,
  ControlEligibility,
  ControlLease,
  DataSource,
  Device,
  SessionMode,
} from "../domain";
import { Glyph } from "./Glyph";

interface Props {
  device: Device;
  source: DataSource;
  mode: SessionMode;
  lease: ControlLease | null;
  eligibility: ControlEligibility;
  commands: ControlCommandDefinition[];
  busyAction: string | null;
  onRequestLease(): void;
  onReturnLive(): void;
  onCommand(command: ControlCommandDefinition): void;
}
export function ControlDock({
  device,
  source,
  mode,
  lease,
  eligibility,
  commands,
  busyAction,
  onRequestLease,
  onReturnLive,
  onCommand,
}: Props) {
  const [pending, setPending] = useState<ControlCommandDefinition | null>(null);
  const isReplay = source.kind === "recording" || mode === "replay";

  const trigger = (command: ControlCommandDefinition) => {
    if (command.risk === "emergency" || command.risk === "high") {
      setPending(command);
      return;
    }
    onCommand(command);
  };

  return (
    <>
      <section className="control-dock" aria-label="장비 제어">
        <div className="control-dock__status">
          <span className={`control-indicator ${eligibility.allowed ? "is-ready" : ""}`}>
            <Glyph name="shield" />
          </span>
          <span>
            <strong>{eligibility.allowed ? "제어 준비됨" : "보기 전용"}</strong>
            <small>{eligibility.reason}</small>
          </span>
        </div>

        <div className="control-dock__actions">
          {eligibility.allowed ? (
            commands.map((command) => (
              <button
                className={`command-button command-button--${command.risk}`}
                key={command.type}
                disabled={busyAction != null}
                onClick={() => trigger(command)}
              >
                {busyAction === command.type ? "처리 중" : command.label}
              </button>
            ))
          ) : isReplay ? (
            <button className="button button--primary" onClick={onReturnLive}>
              LIVE로 이동
            </button>
          ) : !lease ? (
            <button className="button button--primary" onClick={onRequestLease} disabled={busyAction != null}>
              제어권 요청
            </button>
          ) : null}
        </div>
      </section>

      {pending && (
        <div className="dialog-backdrop" role="presentation" onMouseDown={() => setPending(null)}>
          <div
            className="confirm-dialog"
            role="alertdialog"
            aria-modal="true"
            aria-labelledby="command-dialog-title"
            onMouseDown={(event) => event.stopPropagation()}
          >
            <span className="confirm-dialog__icon">
              <Glyph name="shield" />
            </span>
            <h2 id="command-dialog-title">
              {device.name} {pending.label}
            </h2>
            <p>{pending.description}</p>
            <div className="confirm-dialog__summary">
              <span>대상</span>
              <strong>{device.name}</strong>
              <span>현재 상태</span>
              <strong>{device.operationMode}</strong>
            </div>
            <div className="confirm-dialog__actions">
              <button className="button button--quiet" onClick={() => setPending(null)}>
                취소
              </button>
              <button
                className="button button--danger"
                onClick={() => {
                  onCommand(pending);
                  setPending(null);
                }}
              >
                {pending.label} 요청
              </button>
            </div>
          </div>
        </div>
      )}
    </>
  );
}
