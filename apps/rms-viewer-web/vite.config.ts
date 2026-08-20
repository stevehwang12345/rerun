import { createReadStream, existsSync } from "node:fs";
import { basename, extname, resolve } from "node:path";
import react from "@vitejs/plugin-react";
import { defineConfig, loadEnv, type Plugin } from "vite";

const runtimeDirectory = resolve(process.cwd(), "public/runtime");
const runtimeContentTypes: Record<string, string> = {
  ".js": "text/javascript; charset=utf-8",
  ".wasm": "application/wasm",
  ".ts": "text/plain; charset=utf-8",
};

function rmsRuntimeAssets(): Plugin {
  return {
    name: "rms-runtime-assets",
    configureServer(server) {
      server.middlewares.use((request, response, next) => {
        const requestUrl = request.url?.split("?", 1)[0];
        if (!requestUrl?.startsWith("/runtime/")) {
          next();
          return;
        }

        const fileName = decodeURIComponent(requestUrl.slice("/runtime/".length));
        if (!fileName || basename(fileName) !== fileName) {
          response.statusCode = 400;
          response.end();
          return;
        }

        const filePath = resolve(runtimeDirectory, fileName);
        if (!existsSync(filePath)) {
          next();
          return;
        }

        response.setHeader(
          "Content-Type",
          runtimeContentTypes[extname(fileName)] ?? "application/octet-stream",
        );
        response.setHeader("Cache-Control", "no-store");
        createReadStream(filePath).pipe(response);
      });
    },
  };
}

export default defineConfig(({ mode }) => {
  const env = loadEnv(mode, process.cwd(), "");
  const backendTarget = env.RMS_BACKEND_TARGET;

  return {
    plugins: [rmsRuntimeAssets(), react()],
    build: {
      target: "esnext",
    },
    server: {
      host: "127.0.0.1",
      port: 4173,
      strictPort: true,
      proxy: backendTarget
        ? {
            "/api": {
              target: backendTarget,
              changeOrigin: true,
            },
            "/rerun": {
              target: backendTarget,
              changeOrigin: true,
              ws: true,
            },
          }
        : undefined,
    },
    test: {
      environment: "node",
      setupFiles: ["./src/test/setup.ts"],
      css: true,
    },
  };
});
