// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useEffect } from 'react'
import { BuilderNodesSettings } from '@/components/settings/BuilderNodesSettings'
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { usePageTitle } from '@/hooks/usePageTitle'

export function BuilderNodesPage() {
  const { setBreadcrumbs } = useBreadcrumbs()
  usePageTitle('Builder Nodes')
  useEffect(() => {
    setBreadcrumbs([
      { label: 'Settings', href: '/settings' },
      { label: 'Builder Nodes' },
    ])
  }, [setBreadcrumbs])
  return <BuilderNodesSettings />
}
