// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { AuditStatus, DiagnosticSeverity } from "./types";

export function scoreClass(score: number): string {
  if (score >= 90) return "score-good";
  if (score >= 50) return "score-ok";
  return "score-bad";
}

export function scoreColor(score: number): string {
  if (score >= 90) return "var(--success)";
  if (score >= 50) return "var(--warning)";
  return "var(--danger)";
}

export function severityBadgeClass(severity: DiagnosticSeverity): string {
  return `badge badge-${severity}`;
}

export function statusBadgeClass(status: AuditStatus): string {
  return `badge badge-${status}`;
}

export function timeAgo(dateStr: string): string {
  const seconds = Math.floor(
    (Date.now() - new Date(dateStr).getTime()) / 1000,
  );
  if (seconds < 60) return "just now";
  if (seconds < 3600) return `${Math.floor(seconds / 60)}m ago`;
  if (seconds < 86400) return `${Math.floor(seconds / 3600)}h ago`;
  return `${Math.floor(seconds / 86400)}d ago`;
}

export function formatMs(ms: number): string {
  if (ms < 1000) return `${ms}ms`;
  return `${(ms / 1000).toFixed(1)}s`;
}

export function formatMetric(value: number | null, unit: string): string {
  if (value === null) return "--";
  if (unit === "s") return `${(value / 1000).toFixed(2)}s`;
  if (unit === "ms") return `${Math.round(value)}ms`;
  if (unit === "") return value.toFixed(3);
  return `${value}${unit}`;
}

export function metricRating(
  key: string,
  value: number | null,
): "good" | "needs-improvement" | "poor" | "none" {
  if (value === null) return "none";
  switch (key) {
    case "lcp":
      return value <= 2500 ? "good" : value <= 4000 ? "needs-improvement" : "poor";
    case "fcp":
      return value <= 1800 ? "good" : value <= 3000 ? "needs-improvement" : "poor";
    case "tbt":
      return value <= 200 ? "good" : value <= 600 ? "needs-improvement" : "poor";
    case "cls":
      return value <= 0.1 ? "good" : value <= 0.25 ? "needs-improvement" : "poor";
    case "si":
      return value <= 3400 ? "good" : value <= 5800 ? "needs-improvement" : "poor";
    case "tti":
      return value <= 3800 ? "good" : value <= 7300 ? "needs-improvement" : "poor";
    default:
      return "none";
  }
}
