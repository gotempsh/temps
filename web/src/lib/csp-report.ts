// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
export function readCspReport(data: unknown): Record<string, unknown> | null {
  if (!data || typeof data !== 'object' || !('sentry' in data)) return null
  const event = data.sentry
  if (!event || typeof event !== 'object' || !('csp' in event)) return null
  const report = event.csp
  if (!report || typeof report !== 'object') return null
  return report as Record<string, unknown>
}
