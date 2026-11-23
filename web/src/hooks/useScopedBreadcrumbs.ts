// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useEffect } from 'react'
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { useServiceScope } from '@/contexts/ServiceScopeContext'

/**
 * Breadcrumbs of an observability page: the page alone server-wide, or
 * Projects › project › page inside a project.
 */
export function useScopedBreadcrumbs(title: string) {
  const scope = useServiceScope()
  const { setBreadcrumbs } = useBreadcrumbs()
  const label = scope?.label
  const href = scope?.href
  useEffect(
    () =>
      setBreadcrumbs(
        label && href
          ? [
              { label: 'Projects', href: '/projects' },
              { label, href },
              { label: title },
            ]
          : [{ label: title }]
      ),
    [title, label, href, setBreadcrumbs]
  )
}
