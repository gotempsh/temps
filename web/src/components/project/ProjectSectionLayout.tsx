// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { ReactNode } from 'react'
import { useLocation } from 'react-router'
import type { ProjectResponse } from '@/api/client'
import { usePluginsContext } from '@/contexts/PluginsContext'
import { useConsoleExtensions } from '@temps-sdk/console-kit'
import { SectionNavLayout } from '@/components/layout/SectionNavLayout'
import {
  Activity,
  BarChart3,
  ScrollText,
  GitFork,
  Gauge,
  Users,
  FileText,
  Play,
  Filter,
  Bell,
  Database,
  ShieldAlert,
  Home,
  History,
  HardDrive,
  DollarSign,
  Settings2,
  Rocket,
  Globe,
  KeyRound,
  Radio,
  Bot,
  Plug,
  type LucideIcon,
} from 'lucide-react'

const sectionIcons: Record<string, LucideIcon> = {
  project: Home,
  observe: History,
  runtime: ScrollText,
  'request-logs': FileText,
  'telemetry-logs': Radio,
  errors: ShieldAlert,
  traces: GitFork,
  'ai-gateway?tab=activity': Bot,
  metrics: Gauge,
  monitors: Activity,
  'errors/alert-rules': Bell,
  analytics: BarChart3,
  'analytics/visitors': Users,
  'analytics/activity': Activity,
  'analytics/pages': FileText,
  'analytics/replays': Play,
  'analytics/funnels': Filter,
  speed: Gauge,
  'analytics/ai-agents': Bot,
  'analytics/api-traffic': Activity,
  'ai-crawlers': Bot,
  revenue: DollarSign,
  storage: Database,
  'services/kv': KeyRound,
  'services/blob': HardDrive,
  security: ShieldAlert,
  'settings/security': ShieldAlert,
  'settings/access': Users,
  'settings/general': Settings2,
  'settings/delivery': Rocket,
  domains: Globe,
  'settings/variables': KeyRound,
  'settings/automation': Bot,
  'settings/integrations': Plug,
  'settings/telemetry': Radio,
}

import {
  PROJECT_SECTION_LINKS,
  resolveProjectPrimaryRoute,
  resolveProjectSectionLink,
} from '@/lib/project-navigation'

/** One flat contextual navigation beside the project page; never a nested menu. */
export function ProjectSectionLayout({
  project,
  children,
}: {
  project: ProjectResponse
  children: ReactNode
}) {
  const location = useLocation()
  const route = location.pathname.slice(`/projects/${project.slug}/`.length)
  const section = resolveProjectPrimaryRoute(route)
  const { projectNavEntries } = usePluginsContext()
  const { projectToolLinks } = useConsoleExtensions()
  const base = PROJECT_SECTION_LINKS[section]
  if (!base || base.length < 2) return <>{children}</>
  const title =
    section === 'project'
      ? 'Overview'
      : section === 'storage'
        ? 'Databases'
        : section[0].toUpperCase() + section.slice(1)
  const extensionActive =
    section === 'settings' &&
    (projectNavEntries.some(
      (entry) => route === entry.path || route.startsWith(`${entry.path}/`)
    ) ||
      (projectToolLinks ?? []).some(
        (entry) => location.pathname === entry.href(project)
      ))
  const active = extensionActive
    ? 'settings/integrations'
    : resolveProjectSectionLink(section, route, location.search)
  const links = base.map((link) => {
    const href = `/projects/${project.slug}/${link.url}`
    return {
      key: link.url,
      title: link.title,
      to: href,
      icon: sectionIcons[link.url] ?? Plug,
      active:
        active === link.url || (!active && location.pathname === href),
    }
  })
  // Keyed by section so the page filter starts empty in each section.
  return (
    <SectionNavLayout key={section} title={title} links={links}>
      {children}
    </SectionNavLayout>
  )
}
