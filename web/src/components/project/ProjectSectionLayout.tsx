// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useState, type ReactNode } from 'react'
import { Link, useLocation } from 'react-router'
import type { ProjectResponse } from '@/api/client'
import { usePluginsContext } from '@/contexts/PluginsContext'
import { useConsoleExtensions } from '@temps-sdk/console-kit'
import { Input } from '@/components/ui/input'
import { Button } from '@/components/ui/button'
import {
  Sheet,
  SheetContent,
  SheetHeader,
  SheetTitle,
  SheetTrigger,
} from '@/components/ui/sheet'
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
  Menu,
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
import { cn } from '@/lib/utils'

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
  const [search, setSearch] = useState('')
  const [open, setOpen] = useState(false)
  const [previousSection, setPreviousSection] = useState(section)
  if (previousSection !== section) {
    setPreviousSection(section)
    setSearch('')
  }
  const base = PROJECT_SECTION_LINKS[section]
  if (!base || base.length < 2) return <>{children}</>
  const title =
    section === 'project'
      ? 'Overview'
      : section === 'storage'
        ? 'Databases'
        : section[0].toUpperCase() + section.slice(1)
  const links = base.map((link) => ({
    ...link,
    href: `/projects/${project.slug}/${link.url}`,
  }))
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
  const navigation = (
    <>
      <p className="mb-3 text-sm font-semibold">{title}</p>
      {links.length > 7 && (
        <Input
          aria-label={`Find ${title.toLowerCase()} page`}
          placeholder="Find a page…"
          value={search}
          onChange={(event) => setSearch(event.target.value)}
          className="mb-2 h-8 text-xs"
        />
      )}
      <nav aria-label={`${title} pages`} className="space-y-0.5">
        {links
          .filter((link) =>
            `${link.title} ${link.url}`
              .toLowerCase()
              .includes(search.toLowerCase())
          )
          .map((link) => (
            <Link
              key={link.href}
              to={link.href}
              aria-current={
                active === link.url ||
                (!active && location.pathname === link.href)
                  ? 'page'
                  : undefined
              }
              onClick={() => {
                setOpen(false)
                setSearch('')
              }}
              className={cn(
                'flex items-center gap-2 rounded-md px-2.5 py-2 text-sm text-muted-foreground transition-colors hover:bg-muted hover:text-foreground focus-visible:outline focus-visible:outline-2 focus-visible:outline-ring',
                active === link.url && 'bg-muted font-medium text-foreground'
              )}
            >
              {(() => {
                const Icon = sectionIcons[link.url] ?? Plug
                return <Icon aria-hidden="true" className="size-4 shrink-0" />
              })()}
              <span className="min-w-0 truncate" title={link.title}>
                {link.title}
              </span>
            </Link>
          ))}
        {!links.some((link) =>
          `${link.title} ${link.url}`
            .toLowerCase()
            .includes(search.toLowerCase())
        ) && (
          <p className="px-2 py-3 text-xs text-muted-foreground">
            No matching pages.
          </p>
        )}
      </nav>
    </>
  )
  return (
    <div className="grid min-w-0 gap-6 lg:grid-cols-[184px_minmax(0,1fr)]">
      <aside
        className="hidden self-start border-r pr-4 lg:sticky lg:top-0 lg:block lg:max-h-[calc(100vh-180px)] lg:overflow-y-auto"
        aria-label={`${title} navigation`}
      >
        {navigation}
      </aside>
      <div className="min-w-0">
        <div className="mb-4 lg:hidden">
          <Sheet open={open} onOpenChange={setOpen}>
            <SheetTrigger asChild>
              <Button variant="outline" size="sm">
                <Menu className="mr-2 size-4" />
                {title} pages
              </Button>
            </SheetTrigger>
            <SheetContent side="left" className="overflow-y-auto">
              <SheetHeader>
                <SheetTitle>{title}</SheetTitle>
              </SheetHeader>
              <div className="mt-4">{navigation}</div>
            </SheetContent>
          </Sheet>
        </div>
        {children}
      </div>
    </div>
  )
}
