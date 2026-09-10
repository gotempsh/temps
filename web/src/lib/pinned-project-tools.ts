// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

const PINNED_PROJECT_TOOLS_PREFIX = 'temps:pinned-project-tools:'

export const PINNED_PROJECT_TOOLS_CHANGED = 'temps:pinned-project-tools-changed'

export function pinnedProjectToolsStorageKey(projectSlug: string): string {
  return `${PINNED_PROJECT_TOOLS_PREFIX}${projectSlug}`
}

export function parsePinnedProjectTools(
  rawValue: string | null,
  allowedUrls: Iterable<string>
): string[] {
  if (!rawValue) return []

  try {
    const parsed: unknown = JSON.parse(rawValue)
    if (!Array.isArray(parsed)) return []

    const allowed = new Set(allowedUrls)
    return [...new Set(parsed)].filter(
      (value): value is string =>
        typeof value === 'string' && allowed.has(value)
    )
  } catch {
    return []
  }
}
