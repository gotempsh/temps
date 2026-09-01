// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type {
  AuditSummary,
  LighthouseAudit,
  PluginSettings,
  ScoreHistoryPoint,
  UpdateSettings,
} from "./types";

const API_BASE = "/api/x/lighthouse";

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

// Audits

export async function listAudits(): Promise<AuditSummary[]> {
  return request("/audits");
}

export async function getAudit(id: string): Promise<LighthouseAudit> {
  return request(`/audits/${id}`);
}

export async function deleteAudit(id: string): Promise<void> {
  return request(`/audits/${id}`, { method: "DELETE" });
}

export async function getRawJson(id: string): Promise<string> {
  const res = await fetch(`${API_BASE}/audits/${id}/raw`);
  if (!res.ok) {
    throw new ApiError(res.status, await res.text() || res.statusText);
  }
  return res.text();
}

export async function startAudit(
  url: string,
  device?: string,
  categories?: string[],
): Promise<{ id: string; status: string; message: string }> {
  const body: Record<string, unknown> = { url };
  if (device) body.device = device;
  if (categories) body.categories = categories;
  return request("/audit", {
    method: "POST",
    body: JSON.stringify(body),
  });
}

// History

export async function getScoreHistory(): Promise<ScoreHistoryPoint[]> {
  return request("/history");
}

// Status

export async function getStatus(): Promise<{ lighthouse_available: boolean }> {
  return request("/status");
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
