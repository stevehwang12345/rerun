import type { ReactNode, SVGProps } from "react";

export type GlyphName =
  | "grid"
  | "robot"
  | "database"
  | "live"
  | "history"
  | "camera"
  | "chart"
  | "state"
  | "cube"
  | "shield"
  | "pause"
  | "play"
  | "chevron";

export function Glyph({ name, ...props }: { name: GlyphName } & SVGProps<SVGSVGElement>) {
  const paths: Record<GlyphName, ReactNode> = {
    grid: (
      <>
        <rect x="3" y="3" width="7" height="7" rx="1" />
        <rect x="14" y="3" width="7" height="7" rx="1" />
        <rect x="3" y="14" width="7" height="7" rx="1" />
        <rect x="14" y="14" width="7" height="7" rx="1" />
      </>
    ),
    robot: (
      <>
        <rect x="4" y="7" width="16" height="11" rx="3" />
        <path d="M9 7V4h6v3M8 18v2M16 18v2" />
        <circle cx="9" cy="12" r="1" />
        <circle cx="15" cy="12" r="1" />
      </>
    ),
    database: (
      <>
        <ellipse cx="12" cy="5" rx="8" ry="3" />
        <path d="M4 5v6c0 1.7 3.6 3 8 3s8-1.3 8-3V5M4 11v6c0 1.7 3.6 3 8 3s8-1.3 8-3v-6" />
      </>
    ),
    live: (
      <>
        <circle cx="12" cy="12" r="2.5" />
        <path d="M7.8 7.8a6 6 0 0 0 0 8.4M16.2 7.8a6 6 0 0 1 0 8.4M4.6 4.6a10.5 10.5 0 0 0 0 14.8M19.4 4.6a10.5 10.5 0 0 1 0 14.8" />
      </>
    ),
    history: (
      <>
        <path d="M4 8V4m0 0h4M4 4a9 9 0 1 1-1 11" />
        <path d="M12 7v5l3 2" />
      </>
    ),
    camera: (
      <>
        <rect x="3" y="6" width="18" height="13" rx="2" />
        <path d="m8 6 1.5-2h5L16 6" />
        <circle cx="12" cy="12.5" r="3.5" />
      </>
    ),
    chart: <path d="M4 19V5m0 14h16M7 15l3-4 3 2 5-7" />,
    state: (
      <>
        <path d="M5 7h14M5 12h14M5 17h14" />
        <circle cx="8" cy="7" r="1.5" />
        <circle cx="15" cy="12" r="1.5" />
        <circle cx="11" cy="17" r="1.5" />
      </>
    ),
    cube: (
      <>
        <path d="m12 3 8 4.5v9L12 21l-8-4.5v-9z" />
        <path d="m4 7.5 8 4.5 8-4.5M12 12v9" />
      </>
    ),
    shield: <path d="M12 3 5 6v5c0 4.6 2.8 8.1 7 10 4.2-1.9 7-5.4 7-10V6zM9 12l2 2 4-4" />,
    pause: (
      <>
        <path d="M8 6v12M16 6v12" />
      </>
    ),
    play: <path d="m9 6 9 6-9 6z" />,
    chevron: <path d="m9 18 6-6-6-6" />,
  };

  return (
    <svg
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth="1.7"
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden="true"
      {...props}
    >
      {paths[name]}
    </svg>
  );
}
