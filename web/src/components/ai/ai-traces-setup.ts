// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Where "Configure telemetry" goes from the AI activity view: the traces
 * setup of the service being looked at, which is where OpenTelemetry is
 * connected. Without a service there is nothing to configure yet, so it goes
 * to the service list.
 */
export function aiTracesSetupHref(projectSlug: string | null | undefined) {
  return projectSlug
    ? `/projects/${encodeURIComponent(projectSlug)}/traces#traces-setup`
    : '/projects'
}
