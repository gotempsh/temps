// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/** Bounds on what one view holds, so a runaway answer cannot fill the page. */
export const VIEW_LIMITS = {
  /** Elements in one view. */
  elements: 60,
  rows: 100,
  columns: 8,
  items: 30,
  /** Points on one trend line. */
  points: 90,
  /** Characters in any one string. */
  text: 400,
} as const
