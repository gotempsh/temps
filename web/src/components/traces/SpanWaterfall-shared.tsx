// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { CheckCircle2, XCircle } from 'lucide-react'

// ── Shared span display helpers ─────────────────────────────────────
// Extracted alongside the waterfall so both the single-project
// (`TraceDetail`) and cross-project (`CrossProjectTraceDetail`) views
// format spans identically.

export function statusIcon(status?: string) {
  switch (status?.toUpperCase()) {
    case 'ERROR':
      return <XCircle className="h-3.5 w-3.5 text-red-500" />
    case 'OK':
      return <CheckCircle2 className="h-3.5 w-3.5 text-green-500" />
    default:
      return null
  }
}

export function kindLabel(kind?: string): string {
  switch (kind) {
    case 'Server':
      return 'SERVER'
    case 'Client':
      return 'CLIENT'
    case 'Producer':
      return 'PRODUCER'
    case 'Consumer':
      return 'CONSUMER'
    case 'Internal':
      return 'INTERNAL'
    default:
      return kind || 'UNSPECIFIED'
  }
}

export function formatDuration(ms: number): string {
  if (ms < 1) return `${(ms * 1000).toFixed(0)}µs`
  if (ms < 1000) return `${ms.toFixed(1)}ms`
  return `${(ms / 1000).toFixed(2)}s`
}

export function formatTimestamp(ts: string): string {
  const d = new Date(ts)
  return d.toLocaleString(undefined, {
    hour: '2-digit',
    minute: '2-digit',
    second: '2-digit',
  })
}

// Stable per-service colour (Datadog/OpenObserve colour-code spans by service).
export const SERVICE_COLORS = [
  '#6366f1', // indigo
  '#10b981', // emerald
  '#f59e0b', // amber
  '#ec4899', // pink
  '#06b6d4', // cyan
  '#8b5cf6', // violet
  '#ef4444', // red
  '#14b8a6', // teal
]

export function serviceColor(name?: string): string {
  if (!name) return '#94a3b8'
  let h = 0
  for (let i = 0; i < name.length; i++) h = (h * 31 + name.charCodeAt(i)) | 0
  return SERVICE_COLORS[Math.abs(h) % SERVICE_COLORS.length]
}
