// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Recording a workload's outbound connections, for every kind of workload.
 *
 * The cards in this folder are written once. A page hands them an
 * {@link EgressWorkload}: what the workload is called, and the functions
 * that call its own endpoints. Sandboxes and environments keep their own
 * URLs and permissions; the server answers both with the same types.
 */
import {
  getEgressRecording,
  getEnvironmentEgressRecording,
  listEgressRecords,
  listEnvironmentEgressRecords,
  setEgressRecording,
  setEnvironmentEgressRecording,
  type EgressActivityPage,
  type EgressOutcomeFilter,
  type EgressRecordingStatus,
} from '@/api/client'
import { getSandboxQueryKey } from '@/api/client/@tanstack/react-query.gen'

/** Which page of a workload's recorded connections. */
export interface EgressRecordsQuery {
  page: number
  per_page: number
  outcome?: EgressOutcomeFilter
}

/** One workload whose outbound connections can be recorded. */
export interface EgressWorkload {
  /** What it is, for query keys: with `key`, unique across kinds. */
  kind: 'sandbox' | 'environment'
  /** Its identity within its kind, for query keys. */
  key: readonly (string | number)[]
  /** What users call one, in copy: `sandbox`, `environment`. */
  noun: string
  /**
   * What turning recording on means for this kind, when it is more than
   * "from now on": shown under the switch.
   */
  appliesNote?: string
  status: (signal?: AbortSignal) => Promise<EgressRecordingStatus>
  setEnabled: (enabled: boolean) => Promise<EgressRecordingStatus>
  records: (
    query: EgressRecordsQuery,
    signal?: AbortSignal
  ) => Promise<EgressActivityPage>
  /** Queries that show the switch elsewhere, refreshed after a change. */
  invalidates?: readonly (readonly unknown[])[]
}

export const egressStatusKey = (workload: EgressWorkload) =>
  ['egress-recording', workload.kind, ...workload.key] as const

export const egressRecordsKey = (
  workload: EgressWorkload,
  query?: EgressRecordsQuery
) =>
  query === undefined
    ? (['egress-records', workload.kind, ...workload.key] as const)
    : (['egress-records', workload.kind, ...workload.key, query] as const)

/** A sandbox, by its public id. */
export function sandboxEgress(sandboxId: string): EgressWorkload {
  const path = { id: sandboxId }
  return {
    kind: 'sandbox',
    key: [sandboxId],
    noun: 'sandbox',
    status: async (signal) =>
      (await getEgressRecording({ path, signal, throwOnError: true })).data,
    setEnabled: async (enabled) =>
      (
        await setEgressRecording({
          path,
          body: { enabled },
          throwOnError: true,
        })
      ).data,
    records: async (query, signal) =>
      (await listEgressRecords({ path, query, signal, throwOnError: true }))
        .data,
    invalidates: [getSandboxQueryKey({ path })],
  }
}

/** An environment of a project: every deployment of it, on every node. */
export function environmentEgress(
  projectId: number,
  environmentId: number
): EgressWorkload {
  const path = { project_id: projectId, environment_id: environmentId }
  return {
    kind: 'environment',
    key: [projectId, environmentId],
    noun: 'environment',
    appliesNote:
      'Applies to containers started after it is turned on: redeploy to start recording. Only HTTP(S) to public destinations goes through the recorder, and while it is on those requests depend on the node being up.',
    status: async (signal) =>
      (
        await getEnvironmentEgressRecording({
          path,
          signal,
          throwOnError: true,
        })
      ).data,
    setEnabled: async (enabled) =>
      (
        await setEnvironmentEgressRecording({
          path,
          body: { enabled },
          throwOnError: true,
        })
      ).data,
    records: async (query, signal) =>
      (
        await listEnvironmentEgressRecords({
          path,
          query,
          signal,
          throwOnError: true,
        })
      ).data,
  }
}

/** The node to open when a node is what stops recording. */
export function blockingNodeName(status: EgressRecordingStatus): string {
  return status.nodes.find((node) => !node.recording)?.node_name ?? 'the node'
}

/** What two records look like, for the onboarding state. */
export const EXAMPLE_RECORDS = [
  'CONNECT api.example.com:443 allowed (tunnel, 1.3 KiB sent, 4.5 KiB received, 230 ms)',
  'CONNECT blocked.example.com:443 refused: no_allow_rule',
]

/** What the switch may do: turning recording off is always allowed. */
export function canToggle(
  status: EgressRecordingStatus,
  disabled: boolean
): boolean {
  if (disabled) return false
  return status.enabled || status.configured
}

/** The badge variant of a connection outcome. */
export function outcomeVariant(
  outcome: string
):
  | 'success-subtle'
  | 'destructive-subtle'
  | 'warning-subtle'
  | 'neutral-subtle' {
  switch (outcome) {
    case 'allowed':
      return 'success-subtle'
    case 'refused':
      return 'destructive-subtle'
    case 'failed':
      return 'warning-subtle'
    default:
      return 'neutral-subtle'
  }
}

/**
 * Where a connection went, as the workload asked for it: `CONNECT host:port`
 * for HTTPS, `METHOD host/path` for plain HTTP (never a query string: none
 * is recorded), the relay for a Temps relay.
 */
export function describeDestination(record: {
  kind: string
  host?: string | null
  port?: number | null
  method?: string | null
  path?: string | null
  relay?: string | null
}): string {
  const host = record.host ?? 'unknown host'
  const hostPort = record.port == null ? host : `${host}:${record.port}`
  switch (record.kind) {
    case 'tunnel':
      return `CONNECT ${hostPort}`
    case 'forward': {
      const authority =
        record.port == null || record.port === 80 ? host : hostPort
      return `${record.method ?? 'GET'} ${authority}${record.path ?? '/'}`
    }
    case 'relay':
      return `Temps ${record.relay ?? ''} relay`.replace(/\s+/g, ' ')
    default:
      return hostPort
  }
}

/** `230 ms`, `1.4 s`, `2 min 5 s`. */
export function formatDuration(ms: number): string {
  if (ms < 1000) return `${ms} ms`
  if (ms < 60_000) return `${(ms / 1000).toFixed(1)} s`
  const minutes = Math.floor(ms / 60_000)
  const seconds = Math.round((ms % 60_000) / 1000)
  return seconds === 0 ? `${minutes} min` : `${minutes} min ${seconds} s`
}
