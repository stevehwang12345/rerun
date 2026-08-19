import { WebViewer, type SelectionChangeItem } from "@rerun-io/web-viewer";
import {
  forwardRef,
  useEffect,
  useImperativeHandle,
  useRef,
  useState,
} from "react";
import type { DataSource, SessionMode } from "../domain";

export interface RmsRerunViewerHandle {
  setPlaying(value: boolean): void;
  returnToLatest(): void;
}

interface Props {
  source: DataSource;
  mode: SessionMode;
  onModeChange(mode: SessionMode): void;
  onReadyChange(ready: boolean): void;
  onSelectionChange(label: string): void;
}

export const RmsRerunViewerAdapter = forwardRef<RmsRerunViewerHandle, Props>(
  function RmsRerunViewerAdapter(
    { source, mode, onModeChange, onReadyChange, onSelectionChange },
    ref,
  ) {
    const hostRef = useRef<HTMLDivElement>(null);
    const viewerRef = useRef<WebViewer | null>(null);
    const recordingIdRef = useRef<string | null>(null);
    const [status, setStatus] = useState<"loading" | "ready" | "error">("loading");

    useImperativeHandle(
      ref,
      () => ({
        setPlaying(value: boolean) {
          const viewer = viewerRef.current;
          const recordingId = recordingIdRef.current;
          if (!viewer || !recordingId) return;
          viewer.set_playing(recordingId, value);
          if (source.kind === "live") {
            onModeChange(value ? "live" : "paused");
          }
        },
        returnToLatest() {
          const viewer = viewerRef.current;
          const recordingId = recordingIdRef.current;
          if (!viewer || !recordingId || source.kind !== "live") return;
          const timeline = viewer.get_active_timeline(recordingId);
          if (timeline) {
            const range = viewer.get_time_range(recordingId, timeline);
            if (range) {
              viewer.set_current_time(recordingId, timeline, range.max);
            }
          }
          viewer.set_playing(recordingId, true);
          onModeChange("live");
        },
      }),
      [onModeChange, source.kind],
    );

    useEffect(() => {
      const host = hostRef.current;
      if (!host) return;

      let disposed = false;
      const viewer = new WebViewer();
      viewerRef.current = viewer;
      recordingIdRef.current = null;
      setStatus("loading");
      onReadyChange(false);

      const unsubscribe = [
        viewer.on("recording_open", (event) => {
          if (disposed) return;
          recordingIdRef.current = event.recording_id;
          setStatus("ready");
          onReadyChange(true);
        }),
        viewer.on("selection_change", (event) => {
          if (disposed) return;
          onSelectionChange(selectionLabel(event.items));
        }),
      ];

      viewer
        .start(source.rrdUrl, host, {
          width: "100%",
          height: "100%",
          hide_welcome_screen: true,
          theme: "dark",
          allow_fullscreen: false,
          enable_history: false,
        })
        .then(() => {
          if (disposed) return;
          viewer.override_panel_state("top", "hidden");
          viewer.override_panel_state("blueprint", "hidden");
          viewer.override_panel_state("selection", "hidden");
          viewer.override_panel_state("time", source.kind === "live" ? "collapsed" : "expanded");
          viewer.toggle_panel_overrides(true);
        })
        .catch(() => {
          if (disposed) return;
          setStatus("error");
          onReadyChange(false);
        });

      return () => {
        disposed = true;
        unsubscribe.forEach((remove) => remove());
        onReadyChange(false);
        try {
          viewer.stop();
        } catch {
          // The Viewer can already be stopped after a startup failure.
        }
        viewerRef.current = null;
      };
    }, [onModeChange, onReadyChange, onSelectionChange, source]);

    return (
      <div className="rerun-stage" aria-label={`${source.name} Rerun Viewer`}>
        <div className="rerun-stage__canvas" ref={hostRef} />
        {status !== "ready" && (
          <div className={`viewer-status viewer-status--${status}`} role="status">
            <span className="viewer-status__pulse" aria-hidden="true" />
            <strong>
              {status === "loading"
                ? "화면을 준비하고 있습니다"
                : "화면을 불러오지 못했습니다"}
            </strong>
            {status === "error" && <span>데이터 연결을 확인해 주세요.</span>}
          </div>
        )}
        <div className="rerun-stage__mode" data-mode={mode}>
          {mode === "live" ? "LIVE" : mode === "paused" ? "일시정지" : "REPLAY"}
        </div>
      </div>
    );
  },
);

function selectionLabel(items: SelectionChangeItem[]): string {
  const item = items[0];
  if (!item) return "";
  if (item.type === "entity") return item.entity_path;
  if (item.type === "view") return item.view_name;
  return item.container_name;
}
