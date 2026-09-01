// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type {
  PluginSettings,
  UpdateSettings,
  SubmissionResponse,
  SubmissionResult,
  SuggestionsResponse,
} from "./types";

const API_BASE = "/api/x/indexnow";

class ApiError extends Error {
  constructor(
    public status: number,
    message: string,
  ) {
    super(message);
    this.name = "ApiError";
  }
}

async function request<T>(path: string, options?: RequestInit): Promise<T> {
  const res = await fetch(`${API_BASE}${path}`, {
    ...options,
    headers: {
      "Content-Type": "application/json",
      ...options?.headers,
    },
  });

  if (!res.ok) {
    const body = await res.text();
    throw new ApiError(res.status, body || res.statusText);
  }

  if (res.status === 204) return null as T;
  return res.json();
}

// Settings

export async function getSettings(): Promise<PluginSettings> {
  return request("/settings");
}

export async function updateSettings(
  update: UpdateSettings,
): Promise<PluginSettings> {
  return request("/settings", {
    method: "PATCH",
    body: JSON.stringify(update),
  });
}

// Submissions

export async function listSubmissions(
  host?: string,
  projectId?: number,
  limit?: number,
): Promise<SubmissionResponse[]> {
  const params = new URLSearchParams();
  if (host) params.set("host", host);
  if (projectId !== undefined) params.set("projectId", String(projectId));
  if (limit !== undefined) params.set("limit", String(limit));
  const qs = params.toString();
  return request(`/submissions${qs ? `?${qs}` : ""}`);
}

export async function deleteSubmission(url: string): Promise<void> {
  const params = new URLSearchParams({ url });
  return request(`/submissions?${params}`, { method: "DELETE" });
}

// Submit

export async function submitUrls(
  urls?: string[],
  siteUrl?: string,
): Promise<SubmissionResult> {
  const body: Record<string, unknown> = {};
  if (urls) body.urls = urls;
  if (siteUrl) body.siteUrl = siteUrl;
  return request("/submit", {
    method: "POST",
    body: JSON.stringify(body),
  });
}

// Suggestions

export async function getSuggestions(
  siteUrl: string,
): Promise<SuggestionsResponse> {
  return request("/suggestions", {
    method: "POST",
    body: JSON.stringify({ siteUrl }),
  });
}
