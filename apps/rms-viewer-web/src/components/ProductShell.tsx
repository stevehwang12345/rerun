import type { ReactNode } from "react";

import {
  integrationsPath,
  liveIndexPath,
  projectsPath,
  replayIndexPath,
  type RmsRoute,
  routeSection,
} from "../routes";

interface ProductShellProps {
  route: RmsRoute;
  onNavigate: (path: string) => void;
  children: ReactNode;
}

const navigation = [
  { section: "integrations", label: "연동", path: integrationsPath() },
  { section: "projects", label: "프로젝트", path: projectsPath() },
  { section: "live", label: "실시간", path: liveIndexPath() },
  { section: "replay", label: "기록", path: replayIndexPath() },
] as const;

export function ProductShell({ route, onNavigate, children }: ProductShellProps) {
  const activeSection = routeSection(route);

  return (
    <div className="product-shell">
      <header className="product-shell__header">
        <button
          type="button"
          className="product-shell__brand"
          onClick={() => onNavigate(projectsPath())}
          aria-label="RMS 프로젝트로 이동"
        >
          RMS
        </button>
        <nav className="product-shell__nav" aria-label="주 메뉴">
          {navigation.map((item) => (
            <a
              key={item.section}
              href={item.path}
              className={activeSection === item.section ? "is-active" : undefined}
              aria-current={activeSection === item.section ? "page" : undefined}
              onClick={(event) => {
                event.preventDefault();
                onNavigate(item.path);
              }}
            >
              {item.label}
            </a>
          ))}
        </nav>
        <span className="product-shell__operator">운영자</span>
      </header>
      <div className="product-shell__body">{children}</div>
    </div>
  );
}

interface WorkspaceStateProps {
  title: string;
  description?: string;
  actionLabel?: string;
  onAction?: () => void;
  tone?: "default" | "error";
}

export function WorkspaceState({
  title,
  description,
  actionLabel,
  onAction,
  tone = "default",
}: WorkspaceStateProps) {
  return (
    <div className={`workspace-state workspace-state--${tone}`} role={tone === "error" ? "alert" : "status"}>
      <strong>{title}</strong>
      {description && <span>{description}</span>}
      {actionLabel && onAction && (
        <button type="button" className="button button--primary" onClick={onAction}>
          {actionLabel}
        </button>
      )}
    </div>
  );
}

interface StatusBadgeProps {
  label: string;
  tone?: "normal" | "attention" | "restricted" | "neutral";
}

export function StatusBadge({ label, tone = "neutral" }: StatusBadgeProps) {
  return <span className={`status-badge status-badge--${tone}`}>{label}</span>;
}
