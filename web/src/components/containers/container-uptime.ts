// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * When a container's uptime counts from, or `null` when it has none to show.
 * Only a running container has uptime: an exited one would otherwise keep
 * counting from its start for as long as the page stays open.
 */
export function uptimeStart(
  status: string | null | undefined,
  startedAt: string | null | undefined
): string | null {
  return status === 'running' && startedAt ? startedAt : null
}
