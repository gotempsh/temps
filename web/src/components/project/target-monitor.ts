// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { z } from 'zod'
import type {
  CheckSpec,
  CreateTargetMonitorRequest,
  DnsCheckRecordType,
  HttpMethod,
  MonitorResponse,
  UpdateTargetMonitorRequest,
} from '@/api/client'

export const TARGET_KINDS = ['http', 'tcp', 'tls', 'dns'] as const
export type TargetKind = (typeof TARGET_KINDS)[number]

export const HTTP_METHODS: HttpMethod[] = [
  'GET',
  'HEAD',
  'POST',
  'PUT',
  'PATCH',
  'DELETE',
  'OPTIONS',
]
export const DNS_RECORD_TYPES: DnsCheckRecordType[] = [
  'A',
  'AAAA',
  'CNAME',
  'MX',
  'TXT',
  'NS',
]

export const TARGET_KIND_COPY: Record<
  TargetKind,
  { label: string; hint: string; placeholder: string; targetLabel: string }
> = {
  http: {
    label: 'HTTP',
    hint: 'A URL answers with the status and content you expect',
    placeholder: 'https://api.example.com/health',
    targetLabel: 'URL',
  },
  tcp: {
    label: 'TCP port',
    hint: 'A port accepts connections, e.g. a database or queue',
    placeholder: 'db.example.com:5432',
    targetLabel: 'Host and port',
  },
  tls: {
    label: 'TLS certificate',
    hint: 'A certificate is trusted and not about to expire',
    placeholder: 'example.com:443',
    targetLabel: 'Host (port 443 unless given)',
  },
  dns: {
    label: 'DNS record',
    hint: 'A name resolves to the value you expect',
    placeholder: 'example.com',
    targetLabel: 'Host name',
  },
}

/** Comma- or newline-separated values, trimmed, empties dropped. */
export function splitList(value: string): string[] {
  return value
    .split(/[,\n]/)
    .map((item) => item.trim())
    .filter(Boolean)
}

const optionalMs = z
  .string()
  .trim()
  .refine(
    (value) => value === '' || /^\d+$/.test(value),
    'Enter milliseconds, like 1500'
  )

export const targetMonitorSchema = z
  .object({
    name: z
      .string()
      .trim()
      .min(1, 'Give the monitor a name')
      .max(100, 'At most 100 characters'),
    kind: z.enum(TARGET_KINDS),
    target: z.string().trim().min(1, 'Enter what to check').max(2048),
    intervalSeconds: z.number().int().min(30, 'At least 30 seconds'),
    method: z.enum(HTTP_METHODS as [HttpMethod, ...HttpMethod[]]),
    expectedStatus: z.string(),
    keyword: z.string().max(200, 'At most 200 characters'),
    keywordPresent: z.boolean(),
    degradedAfterMs: optionalMs,
    /**
     * The saved timeout, which the form does not edit (the API and CLI set
     * it). Kept so an edit sends it back instead of resetting it.
     */
    timeoutMs: z.number().int().positive().nullable(),
    headers: z.array(
      z.object({
        name: z.string().trim().max(100),
        value: z.string().max(4096),
      })
    ),
    /** When editing: keep the stored headers and body unless changed. */
    replaceRequest: z.boolean(),
    probeFromNodes: z.boolean(),
    body: z.string().max(65536, 'At most 64 KiB'),
    warnDays: z.number().int().min(1).max(90),
    recordType: z.enum(
      DNS_RECORD_TYPES as [DnsCheckRecordType, ...DnsCheckRecordType[]]
    ),
    expectedValues: z.string(),
  })
  .superRefine((values, ctx) => {
    const problem = targetProblem(values.kind, values.target)
    if (problem)
      ctx.addIssue({ code: 'custom', path: ['target'], message: problem })
    if (values.kind === 'http') {
      for (const pattern of splitList(values.expectedStatus)) {
        if (!/^([1-5]xx|[1-5]\d\d)$/i.test(pattern)) {
          ctx.addIssue({
            code: 'custom',
            path: ['expectedStatus'],
            message: `"${pattern}" is not a status code (200) or class (2xx)`,
          })
          break
        }
      }
      if (values.keyword.trim() && values.method === 'HEAD') {
        ctx.addIssue({
          code: 'custom',
          path: ['keyword'],
          message: 'A HEAD response has no body to search',
        })
      }
      values.headers.forEach((header, index) => {
        if (
          header.name.trim() &&
          !/^[!#$%&'*+.^_`|~0-9A-Za-z-]+$/.test(header.name.trim())
        ) {
          ctx.addIssue({
            code: 'custom',
            path: ['headers', index, 'name'],
            message: 'Not a header name',
          })
        }
      })
    }
    if (values.kind === 'dns') {
      const expected = splitList(values.expectedValues)
      if (expected.length > 10) {
        ctx.addIssue({
          code: 'custom',
          path: ['expectedValues'],
          message: 'At most 10 values',
        })
      }
    }
  })

export type TargetMonitorValues = z.infer<typeof targetMonitorSchema>

/** Why `target` does not fit `kind`, or null. The server checks more. */
export function targetProblem(kind: TargetKind, raw: string): string | null {
  const target = raw.trim()
  if (!target) return 'Enter what to check'
  if (kind === 'http') {
    let url: URL
    try {
      url = new URL(target)
    } catch {
      return 'Enter a full URL, like https://api.example.com/health'
    }
    if (url.protocol !== 'http:' && url.protocol !== 'https:') {
      return 'Use an http:// or https:// URL'
    }
    if (url.username || url.password) {
      return 'Put credentials in a request header, not in the URL'
    }
    if (url.hash) return 'Remove the #fragment'
    return null
  }
  if (kind === 'dns') {
    return /[/:]/.test(target) ? 'Enter a host name, like example.com' : null
  }
  if (target.includes('://')) return 'Enter a host and port, not a URL'
  const hasPort = /^\[[^\]]+\]:\d+$/.test(target) || /^[^:]+:\d+$/.test(target)
  if (kind === 'tcp' && !hasPort) return 'Add a port, like db.example.com:5432'
  return null
}

export function newTargetMonitorValues(
  kind: TargetKind = 'http'
): TargetMonitorValues {
  return {
    name: '',
    kind,
    target: '',
    intervalSeconds: 60,
    method: 'GET',
    expectedStatus: '',
    keyword: '',
    keywordPresent: true,
    degradedAfterMs: '',
    timeoutMs: null,
    headers: [],
    replaceRequest: true,
    probeFromNodes: false,
    body: '',
    warnDays: 14,
    recordType: 'A',
    expectedValues: '',
  }
}

/** The form values of an existing target monitor. Header values are never sent back, so they start empty. */
export function targetMonitorValues(
  monitor: MonitorResponse
): TargetMonitorValues {
  const values = newTargetMonitorValues()
  values.name = monitor.name
  values.intervalSeconds = monitor.check_interval_seconds
  values.replaceRequest = false
  values.probeFromNodes = monitor.probe_from_nodes
  const view = monitor.check
  if (!view) return values
  values.target = view.target
  const check = view.check
  values.kind = check.kind
  values.timeoutMs = check.timeout_ms ?? null
  switch (check.kind) {
    case 'http':
      values.method = check.method ?? 'GET'
      values.expectedStatus = (check.expected_status ?? []).join(', ')
      values.keyword = check.keyword?.text ?? ''
      values.keywordPresent = check.keyword?.present ?? true
      values.degradedAfterMs = check.degraded_after_ms?.toString() ?? ''
      values.headers = view.header_names.map((name) => ({ name, value: '' }))
      break
    case 'tcp':
      values.degradedAfterMs = check.degraded_after_ms?.toString() ?? ''
      break
    case 'tls':
      values.warnDays = check.warn_days ?? 14
      break
    case 'dns':
      values.recordType = check.record_type
      values.expectedValues = (check.expected ?? []).join(', ')
      break
  }
  return values
}

function ms(value: string): number | null {
  return value.trim() ? Number(value.trim()) : null
}

export function checkSpec(values: TargetMonitorValues): CheckSpec {
  // Sent only when one is saved; otherwise the server default applies.
  const timeout =
    values.timeoutMs === null ? {} : { timeout_ms: values.timeoutMs }
  switch (values.kind) {
    case 'http':
      return {
        kind: 'http',
        method: values.method,
        expected_status: splitList(values.expectedStatus),
        keyword: values.keyword.trim()
          ? { text: values.keyword.trim(), present: values.keywordPresent }
          : null,
        degraded_after_ms: ms(values.degradedAfterMs),
        ...timeout,
      }
    case 'tcp':
      return {
        kind: 'tcp',
        degraded_after_ms: ms(values.degradedAfterMs),
        ...timeout,
      }
    case 'tls':
      return { kind: 'tls', warn_days: values.warnDays, ...timeout }
    case 'dns':
      return {
        kind: 'dns',
        record_type: values.recordType,
        expected: splitList(values.expectedValues),
        ...timeout,
      }
  }
}

function checkRequest(values: TargetMonitorValues) {
  return {
    headers: values.headers
      .filter((header) => header.name.trim())
      .map((header) => ({ name: header.name.trim(), value: header.value })),
    body: values.body ? values.body : null,
  }
}

export function createTargetMonitorRequest(
  values: TargetMonitorValues
): CreateTargetMonitorRequest {
  return {
    name: values.name.trim(),
    target: values.target.trim(),
    check: checkSpec(values),
    request: values.kind === 'http' ? checkRequest(values) : null,
    check_interval_seconds: values.intervalSeconds,
    probe_from_nodes: values.probeFromNodes,
  }
}

/**
 * The edit body. Headers and body are only sent when the user chose to
 * replace them: their stored values are never shown, so an untouched form
 * must not wipe them.
 */
export function updateTargetMonitorRequest(
  values: TargetMonitorValues
): UpdateTargetMonitorRequest {
  return {
    name: values.name.trim(),
    target: values.target.trim(),
    check: checkSpec(values),
    request:
      values.kind === 'http' && values.replaceRequest
        ? checkRequest(values)
        : null,
    check_interval_seconds: values.intervalSeconds,
    probe_from_nodes: values.probeFromNodes,
  }
}

/** One line describing what a target monitor checks. */
export function targetSummary(monitor: MonitorResponse): string | null {
  const view = monitor.check
  if (!view) return null
  const check = view.check
  switch (check.kind) {
    case 'http':
      return `${check.method ?? 'GET'} ${view.target}`
    case 'tcp':
      return `TCP ${view.target}`
    case 'tls':
      return `TLS certificate of ${view.target}`
    case 'dns':
      return `${check.record_type} record of ${view.target}`
  }
}

/** A location's name, with its region when it has one. */
export function locationLabel(result: {
  location?: string
  name?: string
  region?: string | null
}): string {
  const name = result.location ?? result.name ?? ''
  return result.region ? `${name} (${result.region})` : name
}

/**
 * One line on where the latest check ran from and which locations failed,
 * for a monitor checked from worker nodes; `null` otherwise.
 */
export function locationSummary(monitor: MonitorResponse): string | null {
  const results = monitor.location_results?.results
  if (!monitor.probe_from_nodes) return null
  if (!results?.length) return 'Checked from worker nodes; not checked yet'
  const answered = results.filter((result) => result.status != null)
  const failing = answered.filter((result) => result.status !== 'operational')
  const silent = results.length - answered.length
  const parts = [
    `${answered.length} of ${results.length} location${results.length === 1 ? '' : 's'} answered`,
  ]
  if (failing.length) {
    parts.push(`failing from ${failing.map(locationLabel).join(', ')}`)
  }
  if (silent) parts.push(`${silent} did not answer`)
  return parts.join('; ')
}
