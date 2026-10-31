// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Navigation model for a project (`/projects/:slug/*`): one menu grouped by
 * job (Ship, Observe, Configure), each section's pages shown as underline
 * tabs above the page. A tab's URL is the page's sub-path below the project
 * (`deployments`, `errors/feedback`, …); `aliases` are older or nested paths
 * that belong to the same tab.
 */

/** How the shared environment bar applies to a section's pages. */
export type EnvironmentScope =
  /** The page shows one environment; the bar picks which. */
  | 'single'
  /** The page lists records; the bar can also select "All environments". */
  | 'all'
  /** The page is project-wide; the bar is informational. */
  | 'project'

import type { PageWidth } from '@temps-sdk/ds'
import {
  Activity,
  AlertOctagon,
  HardDrive,
  ArrowLeftRight,
  BarChart3,
  BellRing,
  Bot,
  CalendarCheck,
  CalendarRange,
  CircleDollarSign,
  CirclePlay,
  Clock,
  Database,
  Earth,
  FileCode,
  FileText,
  Filter,
  Gauge,
  GitBranch,
  GitFork,
  Globe,
  Hammer,
  HeartPulse,
  History,
  Home,
  KeyRound,
  Layers,
  LayoutGrid,
  ListFilter,
  Lock,
  MessageSquare,
  Network,
  Plug,
  Puzzle,
  Radar,
  Radio,
  Route,
  ScrollText,
  Settings,
  ShieldCheck,
  SlidersHorizontal,
  Sparkles,
  Tag,
  Ticket,
  UserCog,
  Users,
  Variable,
  Workflow,
  type LucideIcon,
} from 'lucide-react'

/** How wide a page may grow; see `PageWidth` in the design system. */
export type { PageWidth }

export interface ProjectNavTab {
  title: string
  /**
   * Shown before the title, never instead of it. Distinct within its
   * section; the landing tab shares the section's icon.
   */
  icon: LucideIcon
  /** Overrides the section's width for this tab. */
  width?: PageWidth
  /** Sub-path below the project root (`/projects/:slug/<url>`). */
  url: string
  /** Other sub-path prefixes that belong to this tab (detail pages, legacy). */
  aliases?: string[]
}

export type ProjectSectionId =
  | 'overview'
  | 'deployments'
  | 'environments'
  | 'domains'
  | 'databases'
  | 'logs'
  | 'errors'
  | 'traces'
  | 'analytics'
  | 'monitoring'
  | 'build'
  | 'variables'
  | 'access'
  | 'automation'
  | 'general'

export interface ProjectNavSection {
  id: ProjectSectionId
  title: string
  /** The section's icon in the project menu. */
  icon: LucideIcon
  environmentScope: EnvironmentScope
  width: PageWidth
  /** The first tab is the section's landing page. */
  tabs: [ProjectNavTab, ...ProjectNavTab[]]
}

export interface ProjectNavGroup {
  /** Omitted for the ungrouped first entry (Overview). */
  label?: string
  sections: ProjectNavSection[]
}

/** Sub-path of the Build & deploy page, which embeds the delivery settings. */
export const BUILD_AND_DEPLOY_PATH = 'build-deploy'

export const PROJECT_NAV_GROUPS: ProjectNavGroup[] = [
  {
    sections: [
      {
        id: 'overview',
        title: 'Overview',
        icon: Home,
        environmentScope: 'single',
        width: 'full',
        tabs: [
          // A monitoring-only project's overview also answers `integrations`.
          {
            title: 'Overview',
            icon: Home,
            url: 'project',
            aliases: ['integrations'],
          },
          { title: 'Activity', icon: History, url: 'observe' },
        ],
      },
    ],
  },
  {
    label: 'Ship',
    sections: [
      {
        id: 'deployments',
        title: 'Deployments',
        icon: GitBranch,
        environmentScope: 'all',
        width: 'full',
        tabs: [
          {
            title: 'Deployments',
            icon: GitBranch,
            url: 'deployments',
            aliases: ['drop'],
          },
        ],
      },
      {
        id: 'environments',
        title: 'Environments',
        icon: Layers,
        environmentScope: 'single',
        width: 'full',
        tabs: [{ title: 'Environments', icon: Layers, url: 'environments' }],
      },
      {
        id: 'domains',
        title: 'Domains',
        icon: Globe,
        environmentScope: 'project',
        width: 'full',
        tabs: [
          {
            title: 'Domains',
            icon: Globe,
            url: 'domains',
            aliases: ['settings/domains'],
          },
        ],
      },
      {
        id: 'databases',
        title: 'Databases',
        icon: Database,
        environmentScope: 'project',
        width: 'full',
        tabs: [
          {
            title: 'Databases',
            icon: Database,
            url: 'storage',
            aliases: ['databases'],
          },
          { title: 'Key-value store', icon: KeyRound, url: 'services/kv' },
          { title: 'Blob storage', icon: HardDrive, url: 'services/blob' },
        ],
      },
    ],
  },
  {
    label: 'Observe',
    sections: [
      {
        id: 'logs',
        title: 'Logs',
        icon: ScrollText,
        environmentScope: 'single',
        width: 'full',
        tabs: [
          { title: 'Application', icon: ScrollText, url: 'runtime' },
          {
            title: 'Requests',
            icon: ArrowLeftRight,
            url: 'request-logs',
            aliases: ['logs'],
          },
          { title: 'Telemetry', icon: Radio, url: 'telemetry-logs' },
        ],
      },
      {
        id: 'errors',
        title: 'Errors',
        icon: AlertOctagon,
        // Its pages filter by the bar, "All environments" included.
        environmentScope: 'all',
        width: 'full',
        tabs: [
          { title: 'Issues', icon: AlertOctagon, url: 'errors' },
          { title: 'Alerts', icon: BellRing, url: 'errors/alert-rules' },
          { title: 'Feedback', icon: MessageSquare, url: 'errors/feedback' },
          { title: 'Releases', icon: Tag, url: 'releases' },
          { title: 'Source maps', icon: FileCode, url: 'errors/source-maps' },
          { title: 'SDK health', icon: HeartPulse, url: 'client-outcomes' },
          { title: 'Setup', icon: Plug, url: 'errors/setup' },
        ],
      },
      {
        id: 'traces',
        title: 'Traces',
        icon: GitFork,
        // Its pages filter by the bar, "All environments" included.
        environmentScope: 'all',
        width: 'full',
        tabs: [
          { title: 'Traces', icon: GitFork, url: 'traces' },
          { title: 'AI traces', icon: Sparkles, url: 'ai-gateway' },
        ],
      },
      {
        id: 'analytics',
        title: 'Analytics',
        icon: BarChart3,
        // Its pages filter by the bar, "All environments" included.
        environmentScope: 'all',
        width: 'full',
        tabs: [
          { title: 'Overview', icon: BarChart3, url: 'analytics' },
          { title: 'Visitors', icon: Users, url: 'analytics/visitors' },
          { title: 'Live', icon: Radio, url: 'analytics/live-visitors' },
          { title: 'Globe', icon: Earth, url: 'analytics/live' },
          { title: 'Journey', icon: Route, url: 'analytics/journey' },
          {
            title: 'Sessions',
            icon: CirclePlay,
            url: 'analytics/replays',
            width: 'full',
          },
          { title: 'Pages', icon: FileText, url: 'analytics/pages' },
          { title: 'Funnels', icon: Filter, url: 'analytics/funnels' },
          {
            title: 'Activity report',
            icon: CalendarRange,
            url: 'analytics/activity',
          },
          { title: 'Web performance', icon: Gauge, url: 'speed' },
          { title: 'AI visitors', icon: Bot, url: 'analytics/ai-agents' },
          { title: 'AI crawlers', icon: Radar, url: 'ai-crawlers' },
          { title: 'API traffic', icon: Network, url: 'analytics/api-traffic' },
          { title: 'Revenue', icon: CircleDollarSign, url: 'revenue' },
          {
            title: 'Traffic filters',
            icon: SlidersHorizontal,
            url: 'analytics/settings',
          },
        ],
      },
      {
        id: 'monitoring',
        title: 'Monitoring',
        icon: Activity,
        environmentScope: 'single',
        width: 'full',
        tabs: [
          // The Metrics page's own Explore / Dashboards / Alerts strip is
          // folded into these tabs, so there is one row of tabs, not two.
          {
            title: 'Metrics',
            icon: Activity,
            url: 'metrics/explore',
            aliases: ['metrics'],
          },
          {
            title: 'Dashboards',
            icon: LayoutGrid,
            url: 'metrics/dashboards',
            aliases: ['dashboards'],
          },
          { title: 'Alerts', icon: BellRing, url: 'metrics/alerts' },
          { title: 'Uptime', icon: HeartPulse, url: 'monitors' },
          {
            title: 'Cron jobs',
            icon: Clock,
            url: 'cron-jobs',
            aliases: ['settings/cron-jobs'],
          },
          // Jobs scheduled elsewhere, reporting through the Sentry SDK: a
          // different feature from the jobs Temps runs, so its own tab.
          { title: 'Check-ins', icon: CalendarCheck, url: 'check-ins' },
        ],
      },
    ],
  },
  {
    label: 'Configure',
    sections: [
      {
        id: 'build',
        title: 'Build & deploy',
        icon: Hammer,
        environmentScope: 'single',
        width: 'form',
        tabs: [
          {
            title: 'Build & deploy',
            icon: Hammer,
            url: BUILD_AND_DEPLOY_PATH,
            aliases: [
              'settings/delivery',
              'settings/build',
              'settings/git',
              'build',
              'git',
              'connect-repository',
              'flags',
            ],
          },
        ],
      },
      {
        id: 'variables',
        title: 'Variables & secrets',
        icon: Variable,
        environmentScope: 'project',
        // Tables of keys and values, not forms.
        width: 'full',
        tabs: [
          {
            title: 'Environment variables',
            icon: Variable,
            url: 'environment-variables',
            aliases: ['settings/environment-variables'],
          },
          {
            title: 'Secrets',
            icon: KeyRound,
            url: 'settings/variables',
            aliases: ['settings/secrets'],
          },
          {
            title: 'Deployment tokens',
            icon: Ticket,
            url: 'settings/deployment-tokens',
          },
        ],
      },
      {
        id: 'access',
        title: 'Security & access',
        icon: Lock,
        environmentScope: 'project',
        width: 'form',
        tabs: [
          { title: 'Protection', icon: Lock, url: 'settings/security' },
          {
            title: 'Edge rules',
            icon: ListFilter,
            url: 'settings/edge-rules',
            width: 'full',
          },
          { title: 'Access', icon: UserCog, url: 'settings/access' },
          { title: 'Scans', icon: ShieldCheck, url: 'security', width: 'full' },
        ],
      },
      {
        id: 'automation',
        title: 'Automation & integrations',
        icon: Workflow,
        environmentScope: 'project',
        width: 'form',
        tabs: [
          {
            title: 'Automation',
            icon: Workflow,
            url: 'settings/automation',
            aliases: ['agents', 'autofixer'],
          },
          {
            title: 'Integrations',
            icon: Puzzle,
            url: 'settings/integrations',
            aliases: [
              'settings/webhooks',
              'settings/skills',
              'settings/mcp-servers',
            ],
          },
          { title: 'Telemetry', icon: Radio, url: 'settings/telemetry' },
        ],
      },
      {
        id: 'general',
        title: 'General',
        icon: Settings,
        environmentScope: 'project',
        width: 'form',
        tabs: [
          {
            title: 'General',
            icon: Settings,
            url: 'settings/general',
            aliases: ['settings', 'tools'],
          },
        ],
      },
    ],
  },
]

export const PROJECT_NAV_SECTIONS: ProjectNavSection[] =
  PROJECT_NAV_GROUPS.flatMap((group) => group.sections)

function matchesPrefix(route: string, prefix: string): boolean {
  return route === prefix || route.startsWith(`${prefix}/`)
}

export interface ProjectNavLocation {
  section: ProjectNavSection
  tab: ProjectNavTab
}

/**
 * The section and tab a sub-path belongs to. The longest matching prefix
 * wins, so `errors/alert-rules` resolves to the Alerts tab rather than Issues
 * and `metrics/dashboards/3` to Dashboards rather than Metrics. An unknown
 * sub-path falls back to the Overview so the menu always highlights something.
 */
export function resolveProjectLocation(route: string): ProjectNavLocation {
  const normalized = route.replace(/^\/+|\/+$/g, '')
  let best: (ProjectNavLocation & { length: number }) | undefined
  for (const section of PROJECT_NAV_SECTIONS) {
    for (const tab of section.tabs) {
      for (const prefix of [tab.url, ...(tab.aliases ?? [])]) {
        if (
          matchesPrefix(normalized, prefix) &&
          (!best || prefix.length > best.length)
        ) {
          best = { section, tab, length: prefix.length }
        }
      }
    }
  }
  if (best) return { section: best.section, tab: best.tab }
  const overview = PROJECT_NAV_SECTIONS[0]
  return { section: overview, tab: overview.tabs[0] }
}

/**
 * Whether the section's tab strip already names the page at `route`: the
 * strip is shown (two or more tabs) and `route` is a tab's own page, not a
 * record below it (`errors`, not `errors/42`). The page then does not print
 * its title a second time.
 */
export function projectTabNamesPage(
  route: string,
  location: ProjectNavLocation
): boolean {
  if (location.section.tabs.length < 2) return false
  const normalized = route.replace(/^\/+|\/+$/g, '')
  return [location.tab.url, ...(location.tab.aliases ?? [])].includes(
    normalized
  )
}

/** Root URL of a project's pages. */
export function projectRoot(slug: string): string {
  return `/projects/${slug}`
}

/** Query value of the environments page's settings view. */
const ENVIRONMENT_SETTINGS_VIEW = 'settings'

/**
 * The width a project page gets. The environment settings view is a form,
 * though the rest of the environments section is a dashboard.
 */
export function projectPageWidth(
  location: ProjectNavLocation,
  search: string
): PageWidth {
  if (location.tab.width) return location.tab.width
  if (
    location.section.id === 'environments' &&
    new URLSearchParams(search).get('view') === ENVIRONMENT_SETTINGS_VIEW
  )
    return 'form'
  return location.section.width
}

/**
 * Project-relative pages a monitoring-only (`source_type: 'external'`)
 * project registers — see the external route table in `ProjectDetail`.
 * Everything else is a hosting page (deployments, variables, delivery
 * settings, ...) that such a project answers with "Add hosting", so the
 * command palette does not offer it as a destination.
 */
const EXTERNAL_PROJECT_PAGES = [
  'project',
  'hosting',
  'analytics',
  'speed',
  'revenue',
  'errors',
  'traces',
  'ai-gateway',
  'telemetry-logs',
  'releases',
  'client-outcomes',
  'metrics',
  'monitors',
  'check-ins',
  'status-pages',
  'settings/general',
  'settings/telemetry',
] as const

/** Whether a project-relative URL is a page an external project has. */
export function isExternalProjectPage(url: string): boolean {
  const route = url.split(/[?#]/)[0]
  return EXTERNAL_PROJECT_PAGES.some((page) => matchesPrefix(route, page))
}

/** Whether `url` is a page `project` has, given its kind. */
export function projectHasPage(
  project: { source_type?: string | null },
  url: string
): boolean {
  return project.source_type !== 'external' || isExternalProjectPage(url)
}

/**
 * The menu `project` actually has: a monitoring-only project keeps only the
 * tabs it registers, and sections left without a tab disappear.
 */
export function projectNavGroupsFor(project: {
  source_type?: string | null
}): ProjectNavGroup[] {
  if (project.source_type !== 'external') return PROJECT_NAV_GROUPS
  return PROJECT_NAV_GROUPS.map((group) => ({
    ...group,
    sections: group.sections.flatMap((section): ProjectNavSection[] => {
      const tabs = section.tabs.filter((tab) =>
        projectHasPage(project, tab.url)
      )
      const [first, ...rest] = tabs
      return first ? [{ ...section, tabs: [first, ...rest] }] : []
    }),
  })).filter((group) => group.sections.length > 0)
}

/**
 * Where `route` sits in `project`'s own menu: like `resolveProjectLocation`,
 * with the section's tabs narrowed to the pages the project has.
 */
export function projectLocationFor(
  project: { source_type?: string | null },
  route: string
): ProjectNavLocation {
  const location = resolveProjectLocation(route)
  const [first, ...rest] = location.section.tabs.filter((tab) =>
    projectHasPage(project, tab.url)
  )
  return first
    ? { ...location, section: { ...location.section, tabs: [first, ...rest] } }
    : location
}
