import { useCallback, useEffect, useMemo, useState } from "react";

import { createRmsApi } from "./api";
import { ProductShell } from "./components/ProductShell";
import { parseRoute, type RmsRoute } from "./routes";
import { IntegrationsWorkspace } from "./workspaces/IntegrationsWorkspace";
import { ProjectsWorkspace } from "./workspaces/ProjectsWorkspace";
import { SessionCatalogWorkspace } from "./workspaces/SessionCatalogWorkspace";
import { ViewerWorkspace } from "./workspaces/ViewerWorkspace";

function currentRoute(): RmsRoute {
  return parseRoute(window.location.pathname, window.location.search);
}

export default function App() {
  const api = useMemo(() => createRmsApi(), []);
  const [route, setRoute] = useState(currentRoute);

  const navigate = useCallback((path: string, replace = false) => {
    const nextUrl = new URL(path, window.location.href);
    if (`${window.location.pathname}${window.location.search}` === `${nextUrl.pathname}${nextUrl.search}`) {
      return;
    }
    if (replace) {
      window.history.replaceState(null, "", path);
    } else {
      window.history.pushState(null, "", path);
    }
    setRoute(parseRoute(nextUrl.pathname, nextUrl.search));
  }, []);

  useEffect(() => {
    const onPopState = () => setRoute(currentRoute());
    window.addEventListener("popstate", onPopState);
    return () => window.removeEventListener("popstate", onPopState);
  }, []);

  let workspace;
  switch (route.kind) {
    case "integrations":
      workspace = (
        <IntegrationsWorkspace api={api} onOpenProjects={() => navigate("/projects")} />
      );
      break;
    case "projects":
      workspace = (
        <ProjectsWorkspace api={api} projectId={route.projectId} onNavigate={navigate} />
      );
      break;
    case "live-index":
      workspace = <SessionCatalogWorkspace api={api} mode="live" onNavigate={navigate} />;
      break;
    case "replay-index":
      workspace = <SessionCatalogWorkspace api={api} mode="replay" onNavigate={navigate} />;
      break;
    case "live":
    case "replay":
      workspace = <ViewerWorkspace api={api} route={route} onNavigate={navigate} />;
      break;
  }

  return (
    <ProductShell route={route} onNavigate={navigate}>
      {workspace}
    </ProductShell>
  );
}
