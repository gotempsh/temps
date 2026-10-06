// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Keeps an unsaved form in sessionStorage while the user leaves to set up a
 * prerequisite (e.g. a notification provider) and comes back via `returnTo`.
 *
 * Drafts are written only when the user follows such a link, read once when
 * the form mounts, and are tab-scoped by sessionStorage. Storage being
 * unavailable (private mode, quota) silently degrades to "no draft".
 */

const PREFIX = 'temps:form-draft:'

export function saveFormDraft(key: string, value: unknown): void {
  try {
    window.sessionStorage.setItem(PREFIX + key, JSON.stringify(value))
  } catch {
    /* storage unavailable */
  }
}

export function readFormDraft(key: string): unknown {
  try {
    const raw = window.sessionStorage.getItem(PREFIX + key)
    return raw === null ? null : JSON.parse(raw)
  } catch {
    return null
  }
}

export function clearFormDraft(key: string): void {
  try {
    window.sessionStorage.removeItem(PREFIX + key)
  } catch {
    /* storage unavailable */
  }
}
