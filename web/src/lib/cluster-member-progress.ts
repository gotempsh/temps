// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/** The member fields that say where its provisioning is. */
export interface MemberProgressInput {
  role: string
  status: string
  node_id?: number | null
  provisioning_step?: string | null
  provisioning_error?: string | null
}

export type MemberProgress =
  { tone: 'progress'; text: string } | { tone: 'error'; text: string }

/**
 * One line telling the operator what a cluster member is doing while the
 * cluster is created, from the step the backend records on its row
 * (`member_provisioning_step` in temps-providers). `null` once the member is
 * running, so a healthy cluster shows nothing extra.
 */
export function memberProgress(
  member: MemberProgressInput
): MemberProgress | null {
  const where =
    member.node_id != null
      ? `on node ${member.node_id}`
      : 'on the control plane'

  if (member.status === 'failed' || member.provisioning_step === 'failed') {
    return {
      tone: 'error',
      text: member.provisioning_error
        ? member.provisioning_error
        : 'Provisioning stopped before this member was created.',
    }
  }
  if (member.status === 'pending') {
    return {
      tone: 'progress',
      text:
        member.role === 'monitor'
          ? 'Queued: created first.'
          : 'Queued: created after the monitor is ready.',
    }
  }
  if (member.status !== 'creating') return null

  switch (member.provisioning_step) {
    case 'waiting_for_health':
      return {
        tone: 'progress',
        text: `Container started ${where}; waiting for it to accept connections (up to 60s).`,
      }
    case 'registering_dns':
      return { tone: 'progress', text: 'Publishing its cluster DNS name.' }
    case 'done':
      return { tone: 'progress', text: 'Joining the cluster.' }
    default:
      return {
        tone: 'progress',
        text: `Pulling the image and starting the container ${where}.`,
      }
  }
}
