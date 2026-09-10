// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

export function clampPage(page: number, totalPages: number): number {
  if (!Number.isFinite(page) || !Number.isFinite(totalPages)) return 1

  return Math.min(Math.max(Math.trunc(page), 1), Math.max(totalPages, 1))
}
