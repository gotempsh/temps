// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Wording and state for a managed database's server TLS (ADR 093), shared by
 * the service page's TLS card and its tests.
 */

import type { StatusTone } from '@temps-sdk/ds'
import type {
  ServerTlsMode,
  ServiceTlsMemberResponse,
  ServiceTlsStatusResponse,
} from '@/api/client/types.gen'

export interface TlsModeOption {
  value: ServerTlsMode
  title: string
  description: string
  /** Why the mode cannot be chosen yet, if it cannot. */
  unavailable?: string
}

/** The three modes, in the order the server's setting describes them. */
export const TLS_MODE_OPTIONS: TlsModeOption[] = [
  {
    value: 'off',
    title: 'Off',
    description: 'The database accepts only plaintext connections.',
  },
  {
    value: 'available',
    title: 'Available',
    description:
      'The database offers TLS. Clients that request it are encrypted; clients that do not still connect in plaintext.',
  },
  {
    value: 'required',
    title: 'Required',
    description: 'The database refuses connections that do not use TLS.',
    unavailable: 'Not supported yet: linked apps still connect in plaintext.',
  },
]

/** A cluster's data nodes always offer TLS; Off means unverifiable TLS. */
const CLUSTER_OFF_DESCRIPTION =
  "Data nodes serve pg_auto_failover's self-signed certificate: clients can encrypt but cannot verify the server. Plaintext is accepted."

/** The modes as they apply to this service's topology. */
export function tlsModeOptions(
  status: ServiceTlsStatusResponse
): TlsModeOption[] {
  if (status.change_restarts_database) return TLS_MODE_OPTIONS
  return TLS_MODE_OPTIONS.map((option) =>
    option.value === 'off'
      ? { ...option, description: CLUSTER_OFF_DESCRIPTION }
      : option
  )
}

/** Example client connection string that verifies the server certificate. */
export const VERIFY_FULL_EXAMPLE =
  'postgresql://user:password@host:5432/db?sslmode=verify-full&sslrootcert=temps-managed-services-ca.pem'

/** The one-word answer to "is this database's TLS what was asked for?". */
export function tlsVerdict(status: ServiceTlsStatusResponse): {
  tone: StatusTone
  label: string
} {
  if (!status.supported) return { tone: 'idle', label: 'Not supported' }
  if (status.pending_restart) {
    return status.change_restarts_database
      ? { tone: 'warn', label: 'Restart pending' }
      : { tone: 'warn', label: 'Not on every node' }
  }
  if (status.mode === 'off') return { tone: 'idle', label: 'Off' }
  if (status.active === false) return { tone: 'warn', label: 'Not active' }
  return {
    tone: 'ok',
    label: status.mode === 'required' ? 'Required' : 'Available',
  }
}

/** Plain-language state of a cluster's data nodes against the setting. */
export function clusterTlsLine(status: ServiceTlsStatusResponse): string {
  const members = status.members
  if (members.length === 0) return 'The cluster has no data nodes yet.'
  const managed = members.filter((m) => m.active === true).length
  const unknown = members.filter(
    (m) => m.active === null || m.active === undefined
  ).length
  const total = members.length
  const nodes = `data node${total === 1 ? '' : 's'}`
  const parts = [
    status.mode === 'off'
      ? `Self-signed certificate on ${total - managed - unknown} of ${total} ${nodes}.`
      : `Managed certificate on ${managed} of ${total} ${nodes}.`,
  ]
  if (unknown > 0) parts.push(`${unknown} could not be checked.`)
  if (status.pending_restart) {
    parts.push('The rest get it at the next start of the cluster.')
  }
  return parts.join(' ')
}

/** Plain-language state of the running container against the setting. */
export function containerTlsLine(status: ServiceTlsStatusResponse): string {
  if (!status.change_restarts_database) return clusterTlsLine(status)
  if (status.active === null || status.active === undefined) {
    return 'The database container was not found, so whether it serves TLS is unknown.'
  }
  if (status.pending_restart) {
    return status.active
      ? 'The running container still offers TLS; it turns off at the next restart.'
      : 'The running container does not offer TLS yet; it starts offering it at the next start.'
  }
  return status.active
    ? 'The running container offers TLS with the certificate below.'
    : 'The running container does not offer TLS.'
}

/** One data node's TLS state, in words. */
export function memberTlsLabel(member: ServiceTlsMemberResponse): string {
  if (member.active === null || member.active === undefined) {
    return member.detail ? `Unknown: ${member.detail}` : 'Unknown'
  }
  const what = member.active
    ? 'Managed certificate'
    : member.self_signed
      ? 'Self-signed certificate'
      : 'No TLS'
  return member.pending ? `${what} (pending)` : what
}

export function confirmCopy(
  serviceName: string,
  mode: ServerTlsMode,
  restartsDatabase: boolean
) {
  const how = restartsDatabase
    ? 'If the database is running it is restarted with brief downtime.'
    : 'Each running data node switches certificate in place; nothing restarts.'
  return mode === 'off'
    ? {
        title: restartsDatabase
          ? `Turn TLS off for ${serviceName}?`
          : `Stop serving the managed certificate on ${serviceName}?`,
        description: restartsDatabase
          ? `${how} Clients that require TLS can no longer connect.`
          : `${how} Clients that verify the server with the CA can no longer connect.`,
        confirmLabel: restartsDatabase ? 'Turn TLS off' : 'Use self-signed',
      }
    : {
        title: `Offer TLS on ${serviceName}?`,
        description: `${how} Clients that do not request TLS keep connecting as before.`,
        confirmLabel: 'Offer TLS',
      }
}
