// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { HostFacts } from '@/api/client/types.gen'
import { formatBytes } from './onboarding'

export interface FactRow {
  label: string
  value: string
  note?: string
  tone?: 'warning' | 'error'
}

/** Rows for what the server read about its machine; nothing to fill in. */
export function hostRows(facts: HostFacts): FactRow[] {
  const rows: FactRow[] = [
    {
      label: 'Processor',
      value: `${facts.cpu_cores} ${facts.cpu_cores === 1 ? 'core' : 'cores'}`,
    },
    {
      label: 'Memory',
      value: `${formatBytes(facts.memory.total_bytes)}, ${formatBytes(facts.memory.available_bytes)} available`,
      ...(facts.memory.total_bytes < 2 * 1024 ** 3
        ? {
            note: 'Temps needs about 2 GB with an external database, 4 GB with its own.',
            tone: 'warning' as const,
          }
        : {}),
    },
  ]
  rows.push(
    facts.disk
      ? {
          label: 'Disk',
          value: `${formatBytes(facts.disk.total_bytes)}, ${formatBytes(facts.disk.available_bytes)} free on ${facts.disk.mount_point}`,
        }
      : {
          label: 'Disk',
          value: 'unknown',
          note: 'The data directory’s disk could not be read.',
        }
  )
  rows.push(
    facts.docker.running
      ? {
          label: 'Docker',
          value: `${facts.docker.version ?? 'version unknown'}, running`,
        }
      : {
          label: 'Docker',
          value: 'not reachable',
          note:
            facts.docker.error ??
            'Start Docker, then check again. Apps run as containers.',
          tone: 'error',
        }
  )
  const busy = facts.ports
    .filter((p) => p.holder === 'other')
    .map((p) => p.port)
  rows.push(
    busy.length
      ? {
          label: 'Ports 80 and 443',
          value: `${busy.join(' and ')} in use by another program`,
          note: 'Open ports won’t work until it stops. A tunnel still does.',
          tone: 'warning',
        }
      : {
          label: 'Ports 80 and 443',
          value: facts.ports.every((p) => p.holder === 'temps')
            ? 'served by Temps'
            : 'free on this box',
        }
  )
  rows.push({
    label: 'From the internet',
    value: facts.public_ip ?? 'no public address found',
    note: facts.inbound.note,
  })
  rows.push(
    facts.tailscale.running
      ? {
          label: 'Tailscale',
          value: `running · ${facts.tailscale.ip ?? ''}${facts.tailscale.machine && facts.tailscale.tailnet ? ` · ${facts.tailscale.machine}.${facts.tailscale.tailnet}` : ''}`,
          note: facts.tailscale.note ?? undefined,
        }
      : {
          label: 'Tailscale',
          value: 'not running',
          note: facts.tailscale.note ?? undefined,
        }
  )
  rows.push(
    facts.swap_total_bytes > 0
      ? { label: 'Swap', value: formatBytes(facts.swap_total_bytes) }
      : {
          label: 'Swap',
          value: 'none',
          note: 'Fine to start. Without swap, a memory spike stops a container instead of slowing it down.',
        }
  )
  return rows
}
