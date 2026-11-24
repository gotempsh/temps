// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * The first required field, in form order, that is empty or only whitespace,
 * with the message to show for it. The browser's `required` check accepts
 * "   ", so a form that silently returned on `!value.trim()` left the user
 * clicking a button that did nothing.
 */
export function blankRequiredField(
  fields: ReadonlyArray<readonly [label: string, value: string]>
): string | null {
  for (const [label, value] of fields) {
    if (!value.trim()) return `${label} is required and cannot be only spaces`
  }
  return null
}
