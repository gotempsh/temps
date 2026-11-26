// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useEffect } from 'react'
import type { ProjectGroupResponse } from '@/api/client/types.gen'
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { usePageTitle } from '@/hooks/usePageTitle'
import { groupHref } from '@/lib/terminology'

/** Title and breadcrumbs of a page inside a project. */
export function useGroupPageChrome(group: ProjectGroupResponse, title: string) {
  usePageTitle(`${title} · ${group.name}`)
  const { setBreadcrumbs } = useBreadcrumbs()
  useEffect(() => {
    setBreadcrumbs([
      { label: 'Projects', href: '/projects' },
      { label: group.name, href: groupHref(group.slug) },
      { label: title },
    ])
  }, [group.name, group.slug, setBreadcrumbs, title])
}
