import type { DataSource, Device, Project } from "../domain";
import { healthLabel, statusLabel } from "../domain";
import { Glyph } from "./Glyph";

interface Props {
  projects: Project[];
  project: Project | null;
  devices: Device[];
  device: Device | null;
  dataSources: DataSource[];
  dataSource: DataSource | null;
  showMock: boolean;
  onProjectChange(projectId: string): void;
  onDeviceChange(deviceId: string): void;
  onDataSourceChange(dataSourceId: string): void;
}

export function ContextSidebar({
  projects,
  project,
  devices,
  device,
  dataSources,
  dataSource,
  showMock,
  onProjectChange,
  onDeviceChange,
  onDataSourceChange,
}: Props) {
  return (
    <aside className="context-sidebar" aria-label="프로젝트와 장비">
      <div className="brand">
        <span className="brand__mark">R</span>
        <span>
          <strong>RMS</strong>
          <small>Operations</small>
        </span>
      </div>

      <label className="project-picker">
        <span>프로젝트</span>
        <select
          value={project?.id ?? ""}
          onChange={(event) => onProjectChange(event.target.value)}
          aria-label="프로젝트 선택"
        >
          {projects.map((item) => (
            <option key={item.id} value={item.id}>
              {item.name}
            </option>
          ))}
        </select>
        <small>
          {project ? `${project.onlineDeviceCount}/${project.deviceCount}대 연결` : "불러오는 중"}
        </small>
      </label>

      <section className="sidebar-section sidebar-section--devices">
        <header>
          <span>디바이스</span>
          <span>{devices.length}</span>
        </header>
        <div className="sidebar-list">
          {devices.map((item) => (
            <button
              className="device-row"
              data-active={item.id === device?.id}
              key={item.id}
              onClick={() => onDeviceChange(item.id)}
            >
              <span className={`status-dot status-dot--${item.status}`} />
              <span className="device-row__copy">
                <strong>{item.name}</strong>
                <small>{item.taskName}</small>
              </span>
              <span className="sr-only">
                {statusLabel(item.status)}, {healthLabel(item.health)}
              </span>
              <Glyph name="chevron" />
            </button>
          ))}
        </div>
      </section>

      <section className="sidebar-section sidebar-section--data">
        <header>
          <span>데이터</span>
          <Glyph name="database" />
        </header>
        <div className="sidebar-list">
          {dataSources.map((item) => (
            <button
              className="source-row"
              data-active={item.id === dataSource?.id}
              key={item.id}
              onClick={() => onDataSourceChange(item.id)}
            >
              <Glyph name={item.kind === "live" ? "live" : "history"} />
              <span>
                <strong>{item.name}</strong>
                <small>
                  {item.kind === "live" ? "지금" : `${shortDate(item.capturedAt)} · ${item.durationLabel}`}
                </small>
              </span>
              {item.kind === "live" && <i>LIVE</i>}
            </button>
          ))}
        </div>
      </section>

      <div className="sidebar-footer">
        <span className="avatar">HK</span>
        <span>
          <strong>운영자</strong>
          <small>Operator</small>
        </span>
        {showMock && <span className="mock-chip">MOCK</span>}
      </div>
    </aside>
  );
}

function shortDate(value: string): string {
  const date = new Date(value);
  return `${date.getMonth() + 1}/${date.getDate()} ${String(date.getHours()).padStart(2, "0")}:${String(date.getMinutes()).padStart(2, "0")}`;
}
