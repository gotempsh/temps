// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { createContext, useContext } from 'react'

export interface ScopedService {
  id: number
  name: string
  slug: string
  /** For its icon: a monitored service has no deployment to show. */
  source_type?: string | null
}

/**
 * Narrows a server-wide observability page (Logs, Errors, Traces,
 * Analytics) to one project's services. Absent, the page is server-wide.
 */
export interface ServiceScope {
  /** The project's name, for headings and the service picker. */
  label: string
  /** The project's overview, for breadcrumbs. */
  href: string
  services: ScopedService[]
  /** The same page for every service on the server. */
  serverWide: { label: string; title: string; url: string }
}

const ServiceScopeContext = createContext<ServiceScope | null>(null)

export const ServiceScopeProvider = ServiceScopeContext.Provider

export function useServiceScope(): ServiceScope | null {
  return useContext(ServiceScopeContext)
}

/**
 * The service a scoped page shows: the requested one when it belongs to the
 * scope, otherwise the scope's first service. `allowAll` keeps "no service"
 * (every service in the scope) for pages whose API takes several services.
 * `undefined` means "every service in the scope" (or, with no services,
 * nothing to show).
 */
export function scopedServiceId(
  scope: Pick<ServiceScope, 'services'>,
  requested: number | undefined,
  { allowAll = false }: { allowAll?: boolean } = {}
): number | undefined {
  if (requested !== undefined && scope.services.some((s) => s.id === requested))
    return requested
  if (allowAll) return undefined
  return scope.services[0]?.id
}
