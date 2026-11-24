// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { ExternalServiceInfo, ProjectServiceInfo } from '@/api/client'

/**
 * Databases on this server the service could link: not already linked, and
 * matching `search` by name or type.
 */
export function linkableDatabases(
  all: readonly ExternalServiceInfo[],
  linked: readonly Pick<ProjectServiceInfo, 'service'>[],
  search: string
): ExternalServiceInfo[] {
  const term = search.trim().toLowerCase()
  return all.filter(
    (service) =>
      !linked.some((link) => link.service.id === service.id) &&
      `${service.name} ${service.service_type}`.toLowerCase().includes(term)
  )
}
