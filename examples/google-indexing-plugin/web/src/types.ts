// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

export interface PluginSettings {
  serviceAccountConfigured: boolean;
  serviceAccountEmail: string | null;
  autoSubmit: boolean;
  maxUrlsPerDeploy: number;
  dailyQuota: number;
  urlsSubmittedToday: number;
}

export interface UpdateSettings {
  autoSubmit?: boolean;
  maxUrlsPerDeploy?: number;
  dailyQuota?: number;
}

export interface ServiceAccountInfo {
  clientEmail: string;
  projectId: string;
}

export interface SubmissionResponse {
  url: string;
  host: string;
  notificationType: string;
  submittedAt: string;
  googleResponseStatus: number | null;
  notifyTime: string | null;
  submissionCount: number;
  deploymentId: number | null;
  projectId: number | null;
}

export interface SubmitRequest {
  urls: string[];
  notificationType?: string;
  projectId?: number;
  environmentId?: number;
  deploymentId?: number;
}

export interface SubmissionResult {
  submittedCount: number;
  skippedCount: number;
  failedCount: number;
  results: UrlSubmissionResult[];
  error: string | null;
  remainingQuota: number;
}

export interface UrlSubmissionResult {
  url: string;
  success: boolean;
  statusCode: number | null;
  error: string | null;
  notifyTime: string | null;
}

export interface QuotaStatus {
  dailyLimit: number;
  usedToday: number;
  remaining: number;
  resetsAt: string;
}

export interface UrlStatus {
  url: string;
  latestUpdate: NotificationInfo | null;
  latestRemove: NotificationInfo | null;
}

export interface NotificationInfo {
  url: string;
  notificationType: string;
  notifyTime: string;
}
