// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Checks the priority typed for a new group → role rule. An empty box means
 * "use the suggested priority"; anything typed must be a whole number, which
 * is all the API accepts.
 */
export function rolePriorityError(draft: number | undefined): string | null {
  if (draft === undefined) return null
  return Number.isInteger(draft)
    ? null
    : `Priority must be a whole number (got ${draft})`
}
