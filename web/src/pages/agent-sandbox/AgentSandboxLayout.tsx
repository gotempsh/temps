// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { Outlet, useLocation, useNavigate } from 'react-router'
import { Box, Eye, Home, KeyRound, Terminal } from 'lucide-react'
import { usePageTitle } from '@/hooks/usePageTitle'
import { PageHeader } from '@/components/layout/PageContainer'
import { Tabs, TabsList, TabsTrigger } from '@/components/ui/tabs'
import { harnessSectionHref } from './harness-onboarding'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'

// Sub-route definitions for the AI runtime settings hub. The order here drives the
// tab order on desktop and the select-options order on mobile. Keep section
// labels short — they share a row.
const sections = [
  { id: 'overview', label: 'Overview', path: '/agent-sandbox', icon: Home },
  {
    id: 'providers',
    label: 'Harnesses',
    path: '/agent-sandbox/providers',
    icon: Terminal,
  },
  {
    id: 'sandbox',
    label: 'Sandbox',
    path: '/agent-sandbox/sandbox',
    icon: Box,
  },
  {
    id: 'preview',
    label: 'Preview gateway',
    path: '/agent-sandbox/preview',
    icon: Eye,
  },
  {
    id: 'secrets',
    label: 'Secrets',
    path: '/agent-sandbox/secrets',
    icon: KeyRound,
  },
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
  usePageTitle('AI runtime settings')
  const location = useLocation()
  const navigate = useNavigate()
  const current = activeSection(location.pathname)

  const handleChange = (id: string) => {
    const target = sections.find((s) => s.id === id)
    if (target) navigate(harnessSectionHref(target.path, location.search))
  }

  return (
    <div className="space-y-6 px-4 sm:px-0">
      <PageHeader
        title={current === 'providers' ? 'Harnesses' : 'AI runtime settings'}
        description={
          current === 'providers'
            ? 'Connect your AI accounts and configure workspace access.'
            : 'AI providers, sandbox runtime, secrets, and the workspace preview gateway. Each surface owns its own status and settings.'
        }
      />

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
              <TabsTrigger key={s.id} value={s.id} icon={s.icon}>
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
