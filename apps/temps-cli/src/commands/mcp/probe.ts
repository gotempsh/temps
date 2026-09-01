// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

export interface McpProbeResult {
  supported: boolean
  groups?: Array<{ key: string; label: string }>
  reason?: string
}

/**
 * Unauthenticated capability check against `GET /mcp/tools` (see ADR-039).
 * Deliberately does not use the generated API client -- this route is not
 * part of the REST OpenAPI spec, and the probe must work even when the
 * caller has no API key yet.
 */
export async function probeMcpEndpoint(apiUrl: string, timeoutMs = 5000): Promise<McpProbeResult> {
  const base = apiUrl.replace(/\/+$/, '')
  const controller = new AbortController()
  const timer = setTimeout(() => controller.abort(), timeoutMs)
  try {
    const res = await fetch(`${base}/mcp/tools`, { signal: controller.signal })
    if (res.status === 404) {
      return { supported: false, reason: 'no /mcp/tools route -- this instance predates MCP support or the feature flag is off' }
    }
    if (!res.ok) {
      return { supported: false, reason: `unexpected status ${res.status}` }
    }
    const body = (await res.json()) as { groups?: Array<{ key: string; label: string }> }
    return { supported: true, groups: body.groups }
  } catch (error) {
    const message = error instanceof Error ? error.message : String(error)
    return { supported: false, reason: message }
  } finally {
    clearTimeout(timer)
  }
}
