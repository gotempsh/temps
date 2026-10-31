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
  Clock3,
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
  ShieldOff,
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
  Route,
  type LucideIcon,
} from 'lucide-react'

/** Icon per section link URL; a link missing here falls back to a plug. */
export const sectionIcons: Record<string, LucideIcon> = {
  project: Home,
  observe: History,
  runtime: ScrollText,
  'request-logs': FileText,
  'telemetry-logs': Radio,
  errors: ShieldAlert,
  'errors/feedback': Users,
  releases: History,
  'client-outcomes': Activity,
  'errors/source-maps': FileText,
  traces: GitFork,
  'ai-gateway?tab=activity': Bot,
  metrics: Gauge,
  monitors: Activity,
  'cron-jobs': Clock3,
  'status-pages': Globe,
  'errors/alert-rules': Bell,
  analytics: BarChart3,
  'analytics/visitors': Users,
  'analytics/live-visitors': Radio,
  'analytics/live': Globe,
  'analytics/journey': Route,
  'analytics/activity': Activity,
  'analytics/pages': FileText,
  'analytics/replays': Play,
  'analytics/funnels': Filter,
  speed: Gauge,
  'analytics/ai-agents': Bot,
  'analytics/api-traffic': Activity,
  'analytics/settings': ShieldOff,
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
  type ProjectSectionLink,
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
  if (
    project.source_type === 'external' &&
    ['integrations', 'hosting', 'connect-repository', 'setup'].some(
      (path) => route === path || route.startsWith(path + '/')
    )
  )
    return <>{children}</>
  const base: ProjectSectionLink[] | undefined =
    project.source_type === 'external'
      ? section === 'project'
        ? undefined
        : section === 'settings'
          ? [
              { title: 'General', url: 'settings/general' },
              { title: 'Telemetry', url: 'settings/telemetry' },
            ]
          : section === 'logs'
            ? undefined
            : PROJECT_SECTION_LINKS[section]?.filter(
                (link) =>
                  !['analytics/api-traffic', 'ai-crawlers'].includes(link.url)
              )
      : PROJECT_SECTION_LINKS[section]
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
      keywords: link.keywords,
      to: href,
      icon: sectionIcons[link.url] ?? Plug,
      active: active === link.url || (!active && location.pathname === href),
    }
  })
  // Keyed by section so the page filter starts empty in each section.
  return (
    <SectionNavLayout key={section} title={title} links={links}>
      {children}
    </SectionNavLayout>
  )
}
