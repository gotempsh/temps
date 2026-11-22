// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/** The number in `text`, or `undefined` when it is empty or not a number. */
export function parseNumberInput(text: string): number | undefined {
  if (text.trim() === '') return undefined
  const parsed = Number(text)
  return Number.isFinite(parsed) ? parsed : undefined
}
