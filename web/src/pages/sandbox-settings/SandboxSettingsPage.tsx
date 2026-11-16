// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { ArrowLeft } from 'lucide-react'
import { Link, useLocation, useNavigate } from 'react-router'
import { PageContainer, PageHeader } from '@/components/layout/PageContainer'
import { PreviewGatewayCard } from '@/components/settings/PreviewGatewayCard'
import { Button } from '@/components/ui/button'
import { Tabs, TabsList, TabsTrigger } from '@/components/ui/tabs'
import { usePageTitle } from '@/hooks/usePageTitle'
import { SandboxRuntimeSettings } from './SandboxRuntimeSettings'
import { sandboxSettingsSection, SANDBOX_SETTINGS_SECTIONS } from './sections'

/**
 * Platform-wide settings for sandboxes: the runtime they start with and the
 * gateway that serves their preview URLs. These used to sit under the
 * harness settings hub; they belong to sandboxes, so they live beside them.
 */
export function SandboxSettingsPage() {
  const location = useLocation()
  const navigate = useNavigate()
  const current = sandboxSettingsSection(location.pathname)
  const section = SANDBOX_SETTINGS_SECTIONS.find((s) => s.id === current)
  usePageTitle(section?.title ?? 'Sandbox settings')

  return (
    <PageContainer width="form">
      <PageHeader
        title="Sandbox settings"
        actions={
          <Button variant="outline" size="sm" asChild>
            <Link to="/sandboxes">
              <ArrowLeft className="mr-1.5 h-4 w-4" />
              Sandboxes
            </Link>
          </Button>
        }
      />
      <Tabs
        value={current}
        onValueChange={(id) => {
          const target = SANDBOX_SETTINGS_SECTIONS.find((s) => s.id === id)
          if (target) navigate(target.path)
        }}
      >
        <TabsList>
          {SANDBOX_SETTINGS_SECTIONS.map((s) => (
            <TabsTrigger key={s.id} value={s.id}>
              {s.label}
            </TabsTrigger>
          ))}
        </TabsList>
      </Tabs>
      {current === 'preview' ? (
        <PreviewGatewayCard />
      ) : (
        <SandboxRuntimeSettings />
      )}
    </PageContainer>
  )
}
