import { MockRmsApi } from "./mockRmsApi";
import { HttpRmsApi, type RmsApi } from "./rmsApi";

export function createRmsApi(): RmsApi {
  const apiBase = import.meta.env.VITE_RMS_API_BASE?.trim();
  return isMockMode() ? new MockRmsApi() : new HttpRmsApi(apiBase!);
}

export function isMockMode(): boolean {
  const apiBase = import.meta.env.VITE_RMS_API_BASE?.trim();
  const mockSetting = import.meta.env.VITE_RMS_USE_MOCK?.trim().toLowerCase();
  return mockSetting === "true" || !apiBase;
}

export type { RmsApi } from "./rmsApi";
