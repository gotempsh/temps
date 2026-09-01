// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

export type AuditStatus = "running" | "completed" | "failed";
export type AuditTrigger = "deployment" | "manual";
export type DiagnosticSeverity = "critical" | "warning" | "info" | "pass";

export interface CoreWebVitals {
  lcp_ms: number | null;
  fcp_ms: number | null;
  tbt_ms: number | null;
  cls: number | null;
  speed_index_ms: number | null;
  tti_ms: number | null;
}

export interface AuditDiagnostic {
  id: string;
  title: string;
  score: number | null;
  savings: string | null;
  severity: DiagnosticSeverity;
}

export interface AuditSummary {
  id: string;
  url: string;
  performance_score: number | null;
  accessibility_score: number | null;
  best_practices_score: number | null;
  seo_score: number | null;
  status: AuditStatus;
  trigger: AuditTrigger;
  project_id: number | null;
  deployment_id: number | null;
  device: string;
  created_at: string;
  duration_ms: number;
}

export interface LighthouseAudit {
  id: string;
  url: string;
  performance_score: number | null;
  accessibility_score: number | null;
  best_practices_score: number | null;
  seo_score: number | null;
  status: AuditStatus;
  trigger: AuditTrigger;
  project_id: number | null;
  deployment_id: number | null;
  metrics: CoreWebVitals | null;
  diagnostics: AuditDiagnostic[];
  raw_json_available: boolean;
  error_message: string | null;
  created_at: string;
  completed_at: string | null;
  duration_ms: number;
  device: string;
}

export interface ScoreHistoryPoint {
  id: string;
  performance_score: number | null;
  accessibility_score: number | null;
  best_practices_score: number | null;
  seo_score: number | null;
  created_at: string;
  trigger: AuditTrigger;
}

export interface PluginSettings {
  auto_audit_on_deploy: boolean;
  categories: string[];
  score_threshold: number;
  timeout_secs: number;
  chrome_flags: string;
  device: string;
}

export interface UpdateSettings {
  auto_audit_on_deploy?: boolean;
  categories?: string[];
  score_threshold?: number;
  timeout_secs?: number;
  chrome_flags?: string;
  device?: string;
}
