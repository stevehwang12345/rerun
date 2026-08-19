import { useMemo } from "react";
import type { Device, Topic } from "../domain";
import { Glyph, type GlyphName } from "./Glyph";

export type ViewerPreset = "operations" | "camera" | "diagnostics";

interface Props {
  device: Device;
  topics: Topic[];
  preset: ViewerPreset;
  onPresetChange(preset: ViewerPreset): void;
}
const presetTopics: Record<ViewerPreset, string[]> = {
  operations: ["front-camera", "velocity", "altitude", "battery", "planner"],
  camera: ["front-camera", "pose"],
  diagnostics: ["pose", "front-camera", "velocity", "altitude", "battery", "planner"],
};

export function TopicPanel({ device, topics, preset, onPresetChange }: Props) {
  const visibleTopics = useMemo(
    () =>
      presetTopics[preset]
        .map((topicId) => topics.find((topic) => topic.id === topicId))
        .filter((topic): topic is Topic => Boolean(topic)),
    [preset, topics],
  );

  return (
    <aside className="topic-panel" aria-label="맞춤형 Topic Viewer">
      <div className="preset-switcher" role="group" aria-label="Viewer Preset">
        <button data-active={preset === "operations"} onClick={() => onPresetChange("operations")}>
          운영
        </button>
        <button data-active={preset === "camera"} onClick={() => onPresetChange("camera")}>
          카메라
        </button>
        <button data-active={preset === "diagnostics"} onClick={() => onPresetChange("diagnostics")}>
          진단
        </button>
      </div>

      <section className="task-card">
        <header>
          <span>현재 작업</span>
          <strong>{device.taskProgress}%</strong>
        </header>
        <h2>{device.taskName}</h2>
        <div className="task-card__bar">
          <i style={{ width: `${device.taskProgress}%` }} />
        </div>
        <p>{device.taskProgress > 0 ? "경로를 따라 이동 중입니다." : "작업 시작을 기다리고 있습니다."}</p>
      </section>

      <div className="topic-panel__heading">
        <span>표시 중인 정보</span>
        <span>{visibleTopics.length}</span>
      </div>

      <div className="topic-cards">
        {visibleTopics.map((topic) => (
          <TopicCard key={topic.id} topic={topic} showPath={preset === "diagnostics"} />
        ))}
      </div>
    </aside>
  );
}

function TopicCard({ topic, showPath }: { topic: Topic; showPath: boolean }) {
  const icon: Record<Topic["renderer"], GlyphName> = {
    spatial: "cube",
    camera: "camera",
    timeseries: "chart",
    state: "state",
    log: "state",
  };

  if (topic.renderer === "camera") {
    return (
      <article className="topic-card topic-card--camera">
        <div className="camera-preview" aria-label="전방 카메라 미리보기">
          <span className="camera-preview__horizon" />
          <span className="camera-preview__route" />
          <span className="camera-preview__reticle" />
          <span className="camera-preview__live">LIVE</span>
        </div>
        <TopicHeader topic={topic} icon={icon[topic.renderer]} showPath={showPath} />
      </article>
    );
  }

  if (topic.renderer === "timeseries") {
    return (
      <article className="topic-card topic-card--metric">
        <TopicHeader topic={topic} icon={icon[topic.renderer]} showPath={showPath} />
        <div className="metric-row">
          <strong>
            {topic.value}
            <small>{topic.unit}</small>
          </strong>
          <Sparkline samples={topic.samples ?? []} />
        </div>
      </article>
    );
  }

  return (
    <article className={`topic-card topic-card--${topic.renderer}`}>
      <TopicHeader topic={topic} icon={icon[topic.renderer]} showPath={showPath} />
      {topic.message ? <p>{topic.message}</p> : <strong className="topic-value">{topic.value}</strong>}
    </article>
  );
}

function TopicHeader({
  topic,
  icon,
  showPath,
}: {
  topic: Topic;
  icon: GlyphName;
  showPath: boolean;
}) {
  return (
    <header className="topic-card__header">
      <span className="topic-card__icon">
        <Glyph name={icon} />
      </span>
      <span>
        <strong>{topic.label}</strong>
        {showPath && <small>{topic.path}</small>}
      </span>
      <i className={`quality-dot quality-dot--${topic.quality}`} title={qualityText(topic.quality)} />
    </header>
  );
}

function Sparkline({ samples }: { samples: number[] }) {
  if (samples.length < 2) return null;
  const min = Math.min(...samples);
  const max = Math.max(...samples);
  const range = max - min || 1;
  const points = samples
    .map((sample, index) => {
      const x = (index / (samples.length - 1)) * 100;
      const y = 28 - ((sample - min) / range) * 22;
      return `${x},${y}`;
    })
    .join(" ");

  return (
    <svg className="sparkline" viewBox="0 0 100 32" preserveAspectRatio="none" aria-hidden="true">
      <polyline points={points} />
    </svg>
  );
}

function qualityText(quality: Topic["quality"]): string {
  return {
    fresh: "정상",
    delayed: "데이터 지연",
    unavailable: "사용할 수 없음",
  }[quality];
}
