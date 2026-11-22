// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { CreatedLogIngestKeyResponse } from '@/api/client/types.gen'

/** Path of the NDJSON ingest endpoint, on the public router. */
export const LOG_INGEST_PATH = '/api/logs/ingest'

/**
 * The NDJSON ingest endpoint under the public ingress URL (ADR-051), never
 * the browser's origin: a collector outside Temps cannot reach a console
 * behind the admin gate.
 */
export function logIngestUrl(publicIngressBase: string): string {
  return `${publicIngressBase.replace(/\/+$/, '')}${LOG_INGEST_PATH}`
}

/** A one-line test a person can paste into a shell. */
export function curlSnippet(url: string, key: string): string {
  return [
    `curl -X POST '${url}' \\`,
    `  -H 'Authorization: Bearer ${key}' \\`,
    `  -H 'Content-Type: application/x-ndjson' \\`,
    `  --data-binary '{"level":"info","message":"hello from curl","service":"example"}'`,
  ].join('\n')
}

/** A Vector `http` sink writing to the endpoint. */
export function vectorSnippet(url: string, key: string): string {
  return [
    '[sinks.temps]',
    'type = "http"',
    'inputs = ["my_source"]',
    `uri = "${url}"`,
    'method = "post"',
    'encoding.codec = "json"',
    'framing.method = "newline_delimited"',
    'auth.strategy = "bearer"',
    `auth.token = "${key}"`,
    // Vector's defaults (10 MB per batch, no event cap) exceed the
    // endpoint's 5 MiB / 10,000-line limits; leave room for JSON framing.
    'batch.max_bytes = 4000000',
    'batch.max_events = 5000',
  ].join('\n')
}

/** A Fluent Bit `http` output writing to the endpoint. */
export function fluentBitSnippet(url: string, key: string): string {
  const parsed = new URL(url)
  const tls = parsed.protocol === 'https:'
  const port = parsed.port || (tls ? '443' : '80')
  return [
    '[OUTPUT]',
    '    Name   http',
    '    Match  *',
    `    Host   ${parsed.hostname}`,
    `    Port   ${port}`,
    `    URI    ${parsed.pathname}`,
    '    Format json_lines',
    '    json_date_key timestamp',
    '    json_date_format iso8601',
    `    Header Authorization Bearer ${key}`,
    `    tls    ${tls ? 'On' : 'Off'}`,
  ].join('\n')
}

/** A key minted for one service, remembered with the service it belongs to. */
export type CreatedKeyFor = {
  projectId: number
  key: CreatedLogIngestKeyResponse
}

/**
 * The just-created key to show, if it belongs to the service the dialog is
 * now on. A plaintext key is never shown under another service's name.
 */
export function createdKeyFor(
  created: CreatedKeyFor | null,
  projectId: number | undefined
): CreatedLogIngestKeyResponse | null {
  return created !== null && created.projectId === projectId
    ? created.key
    : null
}
