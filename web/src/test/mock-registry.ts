// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

// Kept free of imports: it is loaded by the test preload, which must not pull
// in the API client or any UI module before the DOM exists.
const active = new Set<() => void>()

export function registerMockRestore(restore: () => void) {
  active.add(restore)
}

export function unregisterMockRestore(restore: () => void): boolean {
  return active.delete(restore)
}

/** Restores every installed `mockApi`, newest first. */
export function restoreAllMockApis() {
  for (const restore of [...active].reverse()) restore()
}
