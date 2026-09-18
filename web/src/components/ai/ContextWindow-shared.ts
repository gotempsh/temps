// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

export interface ContextUsage {
  used_tokens: number
  limit_tokens?: number | null
  model?: string | null
  estimated?: boolean
  source: string
  updated_at: string
}

export function contextPercentage(usage?: ContextUsage | null): number | null {
  if (
    !usage ||
    !Number.isSafeInteger(usage.used_tokens) ||
    usage.used_tokens < 0 ||
    !Number.isSafeInteger(usage.limit_tokens) ||
    !usage.limit_tokens ||
    usage.limit_tokens <= 0
  )
    return null
  return Math.round((usage.used_tokens / usage.limit_tokens) * 100)
}
