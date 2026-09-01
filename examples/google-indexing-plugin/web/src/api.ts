// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type {
  PluginSettings,
  UpdateSettings,
  ServiceAccountInfo,
  SubmissionResponse,
  SubmissionResult,
  QuotaStatus,
  UrlStatus,
} from "./types";

const API_BASE = "/api/x/google-indexing";

class ApiError extends Error {
  status: number;
  constructor(message: string, status: number) {
    super(message);
    this.status = status;
    this.name = "ApiError";
  }
}

async function request<T>(
  path: string,
  options?: RequestInit
): Promise<T> {
  const response = await fetch(`${API_BASE}${path}`, {
    headers: { "Content-Type": "application/json" },
    ...options,
  });

  if (!response.ok) {
    const body = await response.json().catch(() => ({}));
    throw new ApiError(
      body.error || `Request failed with status ${response.status}`,
      response.status
    );
  }

  // Handle 204 No Content
  if (response.status === 204) {
    return undefined as T;
  }

  return response.json();
}

export const api = {
  // Settings
  getSettings(): Promise<PluginSettings> {
    return request("/settings");
  },

  updateSettings(update: UpdateSettings): Promise<PluginSettings> {
    return request("/settings", {
      method: "PATCH",
      body: JSON.stringify(update),
    });
  },

  // Service account
  uploadServiceAccount(keyJson: string): Promise<ServiceAccountInfo> {
    return request("/service-account", {
      method: "POST",
      body: JSON.stringify({ keyJson }),
    });
  },

  deleteServiceAccount(): Promise<void> {
    return request("/service-account", { method: "DELETE" });
  },

  // Submissions
  listSubmissions(): Promise<SubmissionResponse[]> {
    return request("/submissions");
  },

  deleteSubmission(url: string): Promise<void> {
    return request(`/submissions?url=${encodeURIComponent(url)}`, {
      method: "DELETE",
    });
  },

  // Submit
  submitUrls(
    urls: string[],
    notificationType?: string
  ): Promise<SubmissionResult> {
    return request("/submit", {
      method: "POST",
      body: JSON.stringify({ urls, notificationType }),
    });
  },

  // Status
  checkUrlStatus(url: string): Promise<UrlStatus> {
    return request("/status", {
      method: "POST",
      body: JSON.stringify({ url }),
    });
  },

  // Quota
  getQuota(): Promise<QuotaStatus> {
    return request("/quota");
  },
};
