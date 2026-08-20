import { spawn } from "node:child_process";
import { fileURLToPath } from "node:url";

const appDirectory = fileURLToPath(new URL("../", import.meta.url));
const repositoryDirectory = fileURLToPath(new URL("../../../", import.meta.url));
const backendHealthUrl = "http://127.0.0.1:8080/health";
const children = new Set();
let stopping = false;

async function backendIsReady() {
  try {
    const response = await fetch(backendHealthUrl, { signal: AbortSignal.timeout(750) });
    return response.ok;
  } catch {
    return false;
  }
}

function start(command, args, options) {
  const child = spawn(command, args, {
    stdio: "inherit",
    shell: process.platform === "win32",
    ...options,
  });
  children.add(child);
  child.once("exit", (code, signal) => {
    children.delete(child);
    if (!stopping && code !== 0) {
      console.error(`RMS 개발 프로세스가 종료되었습니다 (${signal ?? code})`);
      void stop(code ?? 1);
    }
  });
  return child;
}

async function waitForBackend() {
  for (let attempt = 0; attempt < 120; attempt += 1) {
    if (await backendIsReady()) return;
    await new Promise((resolve) => setTimeout(resolve, 500));
  }
  throw new Error("RMS backend가 준비되지 않았습니다");
}

async function stop(exitCode = 0) {
  if (stopping) return;
  stopping = true;
  for (const child of children) child.kill("SIGTERM");
  setTimeout(() => process.exit(exitCode), 250).unref();
}

process.once("SIGINT", () => void stop());
process.once("SIGTERM", () => void stop());

if (!(await backendIsReady())) {
  const cargoArgs =
    process.platform === "win32"
      ? ["+1.95.0-x86_64-pc-windows-gnu", "run", "-p", "rms_server"]
      : ["run", "-p", "rms_server"];
  start("cargo", cargoArgs, { cwd: repositoryDirectory });
  await waitForBackend();
}

const host = start("npm", ["run", "dev:host"], {
  cwd: appDirectory,
  env: {
    ...process.env,
    RMS_BACKEND_TARGET: "http://127.0.0.1:8080",
    VITE_RMS_API_BASE: "/api",
    VITE_RMS_USE_MOCK: "false",
  },
});

host.once("exit", (code) => void stop(code ?? 0));
