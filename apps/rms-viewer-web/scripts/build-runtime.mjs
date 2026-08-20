import { spawnSync } from "node:child_process";
import { mkdirSync, rmSync } from "node:fs";
import { fileURLToPath } from "node:url";

const arguments_ = new Set(process.argv.slice(2));
const debug = arguments_.has("--debug");
const release = arguments_.has("--release");

if (debug && release) {
  console.error("Choose either --debug or --release.");
  process.exitCode = 2;
} else {
  const repositoryRoot = fileURLToPath(new URL("../../../", import.meta.url));
  const runtimeDirectory = new URL("../public/runtime/", import.meta.url);
  mkdirSync(runtimeDirectory, { recursive: true });
  for (const artifact of [
    "rms_product_app.js",
    "rms_product_app.d.ts",
    "rms_product_app_bg.js",
    "rms_product_app_bg.wasm",
    "rms_product_app_bg.wasm.d.ts",
  ]) {
    rmSync(new URL(artifact, runtimeDirectory), { force: true });
  }
  const cargo = process.platform === "win32" ? "cargo.exe" : "cargo";
  const profile = debug ? "--debug" : "--release";
  const command = [
    "run",
    "-p",
    "re_dev_tools",
    "--",
    "build-web-viewer",
    "--package",
    "rms_product_app",
    "--out-name",
    "rms_product_app",
    "--target",
    "web",
    "--no-default-features",
    "--features",
    "",
    "-o",
    "apps/rms-viewer-web/public/runtime",
    profile,
  ];

  const result = spawnSync(cargo, command, {
    cwd: repositoryRoot,
    stdio: "inherit",
    shell: false,
  });

  if (result.error) {
    console.error("RMS runtime build failed.");
    process.exitCode = 1;
  } else {
    process.exitCode = result.status ?? 1;
  }
}
