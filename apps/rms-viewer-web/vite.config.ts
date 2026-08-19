import react from "@vitejs/plugin-react";
import { defineConfig, loadEnv } from "vite";

export default defineConfig(({ mode }) => {
  const env = loadEnv(mode, process.cwd(), "");
  const backendTarget = env.RMS_BACKEND_TARGET;

  return {
    plugins: [react()],
    build: {
      target: "esnext",
    },
    optimizeDeps: {
      exclude: ["@rerun-io/web-viewer"],
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
