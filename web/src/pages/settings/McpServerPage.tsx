// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { PageHeader, SettingsSurface } from '@temps-sdk/ds'
import { McpServerCard } from '@/components/settings/McpServerCard'
import { useBreadcrumbs } from '@/contexts/BreadcrumbContext'
import { usePageTitle } from '@/hooks/usePageTitle'
import { useEffect } from 'react'

export function McpServerPage() {
  const { setBreadcrumbs } = useBreadcrumbs()

  useEffect(() => {
    setBreadcrumbs([
      { label: 'Settings', href: '/settings' },
      { label: 'MCP server' },
    ])
  }, [setBreadcrumbs])

  usePageTitle('MCP server')

  return (
    <div className="space-y-6">
      <PageHeader
        title="MCP server"
        description={
          <>
            Lets AI clients (Claude Code, Claude Desktop, Codex, Cursor, VS
            Code, Windsurf, Zed) connect to this Temps instance over the Model
            Context Protocol — e.g. ask &quot;list my Temps services&quot; or
            &quot;deploy the latest commit&quot; and have the assistant call
            real tools against this instance. Every write action still requires
            a separate confirmation before it executes.
          </>
        }
      />
      <SettingsSurface>
        <McpServerCard />
      </SettingsSurface>
    </div>
  )
}
