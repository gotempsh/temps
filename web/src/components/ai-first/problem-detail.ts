// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

export function problemDetail(error: unknown, fallback: string): string {
  const problem = error as { detail?: unknown; message?: unknown } | null
  if (typeof problem?.detail === 'string' && problem.detail.trim()) {
    return problem.detail
  }
  if (typeof problem?.message === 'string' && problem.message.trim()) {
    return problem.message
  }
  return fallback
}
