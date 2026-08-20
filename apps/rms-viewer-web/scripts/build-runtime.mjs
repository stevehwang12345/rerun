import { spawnSync } from "node:child_process";
import { existsSync, mkdirSync, rmSync } from "node:fs";
import { join } from "node:path";
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
  const buildEnvironment = { ...process.env };
  if (process.platform === "win32") {
    const toolchains = spawnSync("rustup.exe", ["toolchain", "list"], {
      encoding: "utf8",
      shell: false,
    });
    const gnuToolchain = "1.95.0-x86_64-pc-windows-gnu";
    if (toolchains.stdout?.includes(gnuToolchain)) {
      buildEnvironment.RUSTUP_TOOLCHAIN = gnuToolchain;
    }

    const llvmDirectory = join(
      process.env.USERPROFILE ?? "",
      "scoop",
      "apps",
      "llvm",
      "current",
      "bin",
    );
    const clang = join(llvmDirectory, "clang.exe");
    const llvmAr = join(llvmDirectory, "llvm-ar.exe");
    if (existsSync(clang) && existsSync(llvmAr)) {
      buildEnvironment.CC_wasm32_unknown_unknown = clang;
      buildEnvironment.AR_wasm32_unknown_unknown = llvmAr;
      const systemPath = process.env.PATH ?? process.env.Path ?? "";
      delete buildEnvironment.Path;
      buildEnvironment.PATH = `${llvmDirectory};${systemPath}`;
    }
  }
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
    env: buildEnvironment,
  });

  if (result.error) {
    console.error("RMS runtime build failed.", result.error.message);
    process.exitCode = 1;
  } else {
    process.exitCode = result.status ?? 1;
  }
}
