// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

export function serviceProjectId(rawProjectId: string | null): number | null {
  const projectId = Number(rawProjectId)
  return Number.isInteger(projectId) && projectId > 0 ? projectId : null
}

export function serviceProjectLink(
  projectId: number | null
): { project_id: number } | Record<string, never> {
  return projectId === null ? {} : { project_id: projectId }
}

export function serviceCreateHref(
  serviceType: string,
  projectId: number | null
): string {
  const params = new URLSearchParams({ type: serviceType })
  if (projectId !== null) params.set('project_id', String(projectId))
  return `/storage/create?${params.toString()}`
}
