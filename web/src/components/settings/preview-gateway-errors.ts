// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { extractProblemDetails } from '@/utils/errorHandling'

const PORT_CONFLICT_MARKERS = [
  'address already in use',
  'port is already allocated',
  'port is already in use',
  'failed to bind port',
  'ports are not available',
]

const SECRET_ASSIGNMENT =
  /(["']?\b[a-z0-9_-]*(?:password|passwd|token|secret|api[_-]?key|authorization)[a-z0-9_-]*["']?)(\s*[=:]\s*)(?:"[^"]*"|'[^']*'|[^\s,;}\]]+)/gi
const BEARER_TOKEN = /\bbearer\s+[^\s,;]+/gi
const AUTHORIZATION_HEADER =
  /((?:authorization|proxy-authorization|x-registry-auth)\s*[=:]\s*)[^\r\n,;]+/gi
const URL_CREDENTIALS = /([a-z][a-z0-9+.-]*:\/\/)[^/@\s]+@/gi
const SCHEMELESS_URL_CREDENTIALS =
  /\b[a-z0-9._~-]+:[^/@\s]+@(?=[a-z0-9.-]+(?:[/:]|\b))/gi
const URL_QUERY = /([a-z][a-z0-9+.-]*:\/\/[^\s?]+)\?[^\s]+/gi
const MAX_DETAIL_LENGTH = 600

export type GatewayAction = 'refresh' | 'restart' | 'upgrade' | 'save' | 'logs'

export interface GatewayActionError {
  action: GatewayAction
  title: string
  message: string
}

export function gatewayErrorAfterSuccessfulAction(
  error: GatewayActionError | null,
  action: GatewayAction
): GatewayActionError | null {
  return error?.action === action ? null : error
}

const DOCKER_DAEMON_MARKERS = [
  'docker daemon',
  'docker socket',
  'docker.sock',
  'unix:///var/run/docker',
]

/**
 * Remove common credential shapes before a server-provided diagnostic is
 * rendered. The gateway API is privileged, but Docker errors can include
 * registry URLs or request metadata that should not be copied into the UI.
 */
export function sanitizeGatewayDetail(detail: string): string {
  const sanitized = detail
    .replace(URL_CREDENTIALS, '$1[redacted]@')
    .replace(SCHEMELESS_URL_CREDENTIALS, '[redacted]@')
    .replace(URL_QUERY, '$1?[redacted]')
    .replace(BEARER_TOKEN, 'Bearer [redacted]')
    .replace(AUTHORIZATION_HEADER, '$1[redacted]')
    .replace(SECRET_ASSIGNMENT, '$1$2[redacted]')
    .replace(/\s+/g, ' ')
    .trim()

  if (sanitized.length <= MAX_DETAIL_LENGTH) return sanitized
  return `${sanitized.slice(0, MAX_DETAIL_LENGTH - 1)}…`
}

export function previewGatewayErrorMessage(
  error: unknown,
  fallback: string,
  configuredPort?: number | null
): string {
  const problem = extractProblemDetails(error)
  const rawDetail = problem?.detail?.trim() ?? ''
  const normalizedDetail = rawDetail.toLowerCase()
  const mentionsDockerDaemon = DOCKER_DAEMON_MARKERS.some((marker) =>
    normalizedDetail.includes(marker)
  )

  if (
    PORT_CONFLICT_MARKERS.some((marker) => normalizedDetail.includes(marker))
  ) {
    const port = configuredPort ? ` ${configuredPort}` : ''
    return `Host port${port} is already in use, so Docker could not start the preview gateway. Change the configured host port below, save settings, and try again.`
  }

  if (
    mentionsDockerDaemon &&
    (normalizedDetail.includes('permission denied') ||
      normalizedDetail.includes('access denied'))
  ) {
    return 'Docker denied the gateway operation. Check that the Temps process can access the Docker daemon, then try again.'
  }

  if (
    normalizedDetail.includes('cannot connect to the docker daemon') ||
    (mentionsDockerDaemon &&
      (normalizedDetail.includes('failed to connect') ||
        normalizedDetail.includes('connection refused')))
  ) {
    return 'Temps could not reach the Docker daemon. Check that Docker is running and accessible, then try again.'
  }

  if (rawDetail) return sanitizeGatewayDetail(rawDetail)

  if (problem?.title && problem.title !== 'Preview gateway error') {
    return sanitizeGatewayDetail(problem.title)
  }

  return fallback
}
