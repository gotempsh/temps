// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * What runs a container, as the containers API reports it in `runtime`.
 * Responses from a server that predates the field carry none: every
 * container such a server lists is a Docker container.
 */
export function containerRuntime(
  container: { runtime?: string | null } | null | undefined
): string {
  const runtime = container?.runtime?.trim().toLowerCase()
  return runtime ? runtime : 'docker'
}

const LABELS: Record<string, string> = {
  docker: 'Docker',
  firecracker: 'Firecracker microVM',
  microsandbox: 'microsandbox microVM',
}

/** The name shown beside the runtime's logo. */
export function containerRuntimeLabel(runtime: string): string {
  return LABELS[runtime] ?? runtime
}
