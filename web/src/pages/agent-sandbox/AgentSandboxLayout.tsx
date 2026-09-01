// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { Outlet, useLocation, useNavigate } from 'react-router'
import { usePageTitle } from '@/hooks/usePageTitle'
import { Tabs, TabsList, TabsTrigger } from '@/components/ui/tabs'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'

// Sub-route definitions for the AI Workflows hub. The order here drives the
// tab order on desktop and the select-options order on mobile. Keep section
// labels short — they share a row.
const sections = [
  { id: 'overview', label: 'Overview', path: '/agent-sandbox' },
  { id: 'providers', label: 'AI Providers', path: '/agent-sandbox/providers' },
  { id: 'sandbox', label: 'Sandbox', path: '/agent-sandbox/sandbox' },
  { id: 'preview', label: 'Preview Gateway', path: '/agent-sandbox/preview' },
  { id: 'secrets', label: 'Secrets', path: '/agent-sandbox/secrets' },
] as const

type SectionId = (typeof sections)[number]['id']

function activeSection(pathname: string): SectionId {
  // Strip trailing slash so /agent-sandbox/ matches the overview tab.
  const normalized = pathname.replace(/\/$/, '')
  if (normalized.startsWith('/agent-sandbox/providers')) return 'providers'
  if (normalized.startsWith('/agent-sandbox/sandbox')) return 'sandbox'
  if (normalized.startsWith('/agent-sandbox/preview')) return 'preview'
  if (normalized.startsWith('/agent-sandbox/secrets')) return 'secrets'
  return 'overview'
}

export function AgentSandboxLayout() {
  usePageTitle('AI Workflows')
  const location = useLocation()
  const navigate = useNavigate()
  const current = activeSection(location.pathname)

  const handleChange = (id: string) => {
    const target = sections.find((s) => s.id === id)
    if (target) navigate(target.path)
  }

  return (
    <div className="space-y-6">
      <div>
        <h1 className="text-2xl font-semibold">AI Workflows</h1>
        <p className="text-sm text-muted-foreground">
          AI providers, sandbox runtime, secrets, and the workspace preview
          gateway. Each surface owns its own status and settings.
        </p>
      </div>

      {/* Mobile select */}
      <div className="sm:hidden">
        <Select value={current} onValueChange={handleChange}>
          <SelectTrigger className="w-full">
            <SelectValue />
          </SelectTrigger>
          <SelectContent>
            {sections.map((s) => (
              <SelectItem key={s.id} value={s.id}>
                {s.label}
              </SelectItem>
            ))}
          </SelectContent>
        </Select>
      </div>

      {/* Desktop tabs */}
      <div className="hidden sm:block">
        <Tabs value={current} onValueChange={handleChange}>
          <TabsList>
            {sections.map((s) => (
              <TabsTrigger key={s.id} value={s.id}>
                {s.label}
              </TabsTrigger>
            ))}
          </TabsList>
        </Tabs>
      </div>

      <Outlet />
    </div>
  )
}
