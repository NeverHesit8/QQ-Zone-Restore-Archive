import { invoke } from "@tauri-apps/api/core";

export interface HistoryProbe {
  label: string;
  offset: number;
  count: number;
  beginTime: number;
  endTime: number;
  httpStatus: number | null;
  apiCode: number | null;
  subcode: number | null;
  hasMore: boolean | null;
  totalNumber: number | null;
  recordCount: number;
  oldestTime: number | null;
  newestTime: number | null;
  sameAsBaseline: boolean | null;
  timeUpperBoundMatches: boolean | null;
  note: string;
}

export interface HistoryDiagnosticsReport {
  version: number;
  status: "idle" | "running" | "completed" | "cancelled";
  message: string;
  boundaryOffset: number;
  plannedRequests: number;
  rows: HistoryProbe[];
}

export const getHistoryDiagnostics = () => invoke<HistoryDiagnosticsReport>("get_history_diagnostics");
export const cancelHistoryDiagnostics = () => invoke<void>("cancel_history_diagnostics");
export const startHistoryDiagnostics = (boundaryOffset: number, endTime: number | undefined, intervalMs: number) =>
  invoke<HistoryDiagnosticsReport>("start_history_diagnostics", { boundaryOffset, endTime, intervalMs });
