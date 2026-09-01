// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

export function currentRetainedContainers<T extends { is_current: boolean }>(
  containers: T[] | undefined
): T[] {
  return containers?.filter((container) => container.is_current) ?? []
}

export function retainedContainerLogsPath(
  projectSlug: string,
  environmentId: number,
  deploymentId: number,
  containerId: string
): string {
  return `/projects/${projectSlug}/environments/containers/${containerId}?env=${environmentId}&deployment=${deploymentId}`
}

export function toggleRetainedContainerLogs(
  expandedContainerId: string | null,
  selectedContainerId: string
): string | null {
  return expandedContainerId === selectedContainerId
    ? null
    : selectedContainerId
}
