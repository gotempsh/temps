// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { PluginSettings, ReportSummary, SeoReport, UpdateSettings } from "./types";

// The plugin API is proxied through Temps at /api/x/seo-analyzer.
// This works in both production (iframe inside Temps) and local dev
// (vite proxies to the running plugin binary or Temps backend).
const API_BASE = "/api/x/seo-analyzer";

class ApiError extends Error {
  constructor(
    public status: number,
    message: string,
  ) {
    super(message);
    this.name = "ApiError";
  }
}

async function request<T>(
  path: string,
  options?: RequestInit,
): Promise<T> {
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

// Reports

export async function listReports(): Promise<ReportSummary[]> {
  return request("/reports");
}

export async function getReport(id: string): Promise<SeoReport> {
  return request(`/reports/${id}`);
}

export async function deleteReport(id: string): Promise<void> {
  return request(`/reports/${id}`, { method: "DELETE" });
}

export async function getReportPrompt(id: string): Promise<string> {
  const res = await fetch(`${API_BASE}/reports/${id}/prompt`);
  if (!res.ok) {
    throw new ApiError(res.status, await res.text() || res.statusText);
  }
  return res.text();
}

export async function startAnalysis(
  url: string,
  maxPages?: number,
): Promise<{ id: string; status: string; message: string }> {
  const body: Record<string, unknown> = { url };
  if (maxPages !== undefined) {
    body.max_pages = maxPages;
  }
  return request("/analyze", {
    method: "POST",
    body: JSON.stringify(body),
  });
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
