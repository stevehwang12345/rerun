import { MockRmsApi } from "./mockRmsApi";
import { HttpRmsApi, type RmsApi } from "./rmsApi";

export function createRmsApi(): RmsApi {
  const apiBase = import.meta.env.VITE_RMS_API_BASE?.trim();
  return isMockMode() ? new MockRmsApi() : new HttpRmsApi(apiBase || "/api");
}

export function isMockMode(): boolean {
  const mockSetting = import.meta.env.VITE_RMS_USE_MOCK?.trim().toLowerCase();
  return mockSetting === "true";
}

export { HttpRmsApi, MockRmsApi };
export type {
  AssignDataSourceInput,
  AssignDeviceInput,
  ControlApi,
  CreateIntegrationInput,
  CreateLiveSessionInput,
  CreateProjectInput,
  CreateReplaySessionInput,
  IntegrationApi,
  LiveApi,
  ProjectApi,
  RegisterDataSourceInput,
  RegisterDeviceInput,
  ReplayApi,
  RmsApi,
} from "./rmsApi";
