// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/** The NDJSON ingest endpoint on the server the console is served from. */
export function logIngestUrl(origin: string): string {
  return `${origin.replace(/\/+$/, '')}/api/logs/ingest`
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
