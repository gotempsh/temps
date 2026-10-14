// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * What the Edit Resource Limits dialog tells the operator after a save.
 *
 * Pure, so every combination is unit-testable without rendering the dialog.
 * Several things can go wrong at once: a new /dev/shm size can be refused
 * (replacing the container would not keep its data, #1415) while another
 * container rejected a cap, or still needs a restart to drop one. Each of
 * those gets its own message; none hides another.
 */

/** The parts of a member outcome the messages need. */
export interface MemberOutcomeLike {
  container_name: string
  outcome: string
  error?: string | null
}

/** The parts of the update response the messages need. */
export interface ResourceUpdateLike {
  applied?: MemberOutcomeLike[] | null
  shm_not_applied?: string | null
}

export interface ResourceUpdateMessage {
  kind: 'success' | 'warning'
  title: string
  description: string
  /** Milliseconds; set for messages that need time to read. */
  duration?: number
}

export const RECREATE_REQUIRED_DESCRIPTION =
  "Some changes (shared memory, or removing a memory cap) can't be applied live. Restart the service to recreate the container and apply them."

export function resourceUpdateMessages(
  response: ResourceUpdateLike
): ResourceUpdateMessage[] {
  const members = response.applied ?? []
  const failed = members.filter((m) => m.outcome === 'failed')
  // Docker can't remove a memory cap on a running container: only a
  // container recreate picks that change up.
  const recreate = members.filter((m) => m.outcome === 'requires_recreate')
  const messages: ResourceUpdateMessage[] = []

  if (response.shm_not_applied) {
    messages.push({
      kind: 'warning',
      title: 'Limits saved, but the shared memory size was not applied',
      description: response.shm_not_applied,
      duration: 15000,
    })
  }
  if (failed.length > 0) {
    messages.push({
      kind: 'warning',
      title: 'Limits saved, but some containers rejected the update',
      description: failed
        .map((f) => `${f.container_name}: ${f.error ?? 'failed'}`)
        .join('\n'),
    })
  }
  if (recreate.length > 0) {
    messages.push({
      kind: 'warning',
      title: 'Limits saved — restart required',
      description: RECREATE_REQUIRED_DESCRIPTION,
    })
  }
  if (messages.length > 0) return messages

  if (members.length === 0) {
    return [
      {
        kind: 'success',
        title: 'Resource limits saved',
        description:
          'No running containers found — caps will apply on next start.',
      },
    ]
  }
  const live = members.filter((m) => m.outcome === 'applied').length
  const stopped = members.filter((m) => m.outcome === 'stopped').length
  const missing = members.filter((m) => m.outcome === 'missing').length
  const parts = [
    live > 0 ? `${live} live` : null,
    stopped > 0 ? `${stopped} on next start` : null,
    missing > 0 ? `${missing} not yet created` : null,
  ].filter(Boolean) as string[]
  return [
    {
      kind: 'success',
      title: 'Resource limits applied',
      description: parts.join(' · '),
    },
  ]
}
