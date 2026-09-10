// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { McpServerCard } from '@/components/settings/McpServerCard'
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { usePageTitle } from '@/hooks/usePageTitle'
import { useEffect } from 'react'

export function McpServerPage() {
  const { setBreadcrumbs } = useBreadcrumbs()

  useEffect(() => {
    setBreadcrumbs([{ label: 'Settings', href: '/settings' }, { label: 'MCP Server' }])
  }, [setBreadcrumbs])

  usePageTitle('MCP Server')

  return (
    <div className="space-y-6">
      <McpServerCard />
    </div>
  )
}
