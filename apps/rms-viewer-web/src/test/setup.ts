import { webcrypto } from "node:crypto";

if (!globalThis.crypto?.randomUUID) {
  Object.defineProperty(globalThis, "crypto", {
    configurable: true,
    value: webcrypto,
});
}
