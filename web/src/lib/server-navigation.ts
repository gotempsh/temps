// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Every server-wide page, and where it lives.
 *
 * The sidebar is a tree of what the user owns (`lib/entity-tree.ts`): its
 * projects, each with its services and the databases they use, then one
 * Server row. Data is not an entry of it. What is seen across every project
 * (logs, errors, traces, analytics, alerts) is a tab of the Projects page;
 * every record of the server (its nodes, databases and domains) is a tab of
 * the Server page; what is set up once and then used from those things
 * (Git, email, model providers) is a page of Settings.
 *
 * Every page has exactly one home here. The tree, the tab strips, the
 * Settings menu and the command palette all read this one list, so they
 * cannot disagree.
 */

import {
  Activity,
  AlertOctagon,
  BarChart3,
  BellRing,
  Box,
  BrainCircuit,
  CircleDollarSign,
  Cloud,
  Cpu,
  Database,
  DatabaseBackup,
  Folder,
  FolderGit2,
  Gauge,
  GitFork,
  Globe,
  History,
  LayoutGrid,
  ListChecks,
  Mail,
  MessageSquare,
  Network,
  Plug,
  Radio,
  ScrollText,
  Send,
  Server,
  ShieldCheck,
  SlidersHorizontal,
  Bot,
  ToggleRight,
  Truck,
  Vault,
  Waypoints,
  Workflow,
  type LucideIcon,
} from 'lucide-react'
import type { PageWidth } from '@temps-sdk/ds'

/** The Alerts area opens on what is firing. */
export const ALERTS_URL = '/monitoring/alarms'

/**
 * Notification providers and routes. The page keeps its `/settings` URL,
 * which the server and many pages link to, and is a tab of Alerts.
 */
export const NOTIFICATION_CHANNELS_URL = '/settings/notifications'

export interface ServerNavTab {
  title: string
  icon: LucideIcon
  url: string
  /** How wide the tab's page is; the strip is laid out to match. */
  width?: PageWidth
  featureKey?: string
}

export interface ServerNavArea {
  id: string
  title: string
  icon: LucideIcon
  /** The area's landing page. */
  url: string
  /** One line on what the area is for. */
  description: string
  keywords?: string[]
  featureKey?: string
  /**
   * Path prefixes that belong to the area besides its tabs (record and form
   * pages). The area's own URL and its tabs' URLs always count.
   */
  prefixes?: string[]
  /** Prefixes below `prefixes` that belong to another area. */
  except?: string[]
  /** The area's pages, as a strip above them. One page needs none. */
  tabs?: ServerNavTab[]
  /**
   * For a branch of the tree: what to offer while it lists nothing. The
   * label is a button's, a verb and a noun.
   */
  add?: { label: string; href: string }
}

/** The first entry, and the page `/` opens. */
export const SERVER_HOME: ServerNavArea = {
  id: 'projects',
  title: 'Projects',
  icon: Folder,
  url: '/projects',
  description: 'Deploy and operate applications.',
  keywords: ['apps', 'deployments', 'sites', 'services'],
  prefixes: ['/p'],
  add: { label: 'Create service', href: '/projects/new' },
}

/** Read whatever else is set up, so always tabs. */
const SERVER_EVERYDAY: ServerNavArea[] = [
  {
    id: 'logs',
    title: 'Logs',
    icon: ScrollText,
    url: '/logs',
    description: 'Search application and database logs of every service.',
  },
  {
    id: 'errors',
    title: 'Errors',
    icon: AlertOctagon,
    url: '/errors',
    description: 'Review application issues across services.',
    keywords: ['issues', 'exceptions', 'crashes'],
  },
]

/**
 * What is seen across every project: the tabs of the Projects page.
 */
export const SERVER_VIEWS: ServerNavArea[] = [
  ...SERVER_EVERYDAY,
  {
    id: 'traces',
    title: 'Traces',
    icon: GitFork,
    url: '/traces',
    description: 'Follow a request across services.',
    keywords: ['spans', 'opentelemetry', 'latency'],
  },
  {
    id: 'analytics',
    title: 'Analytics',
    icon: BarChart3,
    url: '/analytics',
    description: 'Compare traffic and revenue across services.',
    keywords: ['visitors', 'traffic', 'mrr', 'stripe', 'payments'],
    tabs: [
      { title: 'Analytics', icon: BarChart3, url: '/analytics' },
      {
        title: 'Revenue',
        icon: CircleDollarSign,
        url: '/revenue',
        featureKey: 'revenue-tracking',
      },
    ],
  },
  {
    id: 'alerts',
    title: 'Alerts',
    icon: BellRing,
    url: ALERTS_URL,
    description: 'What is firing, the rules behind it and where it is sent.',
    keywords: ['monitoring', 'alarms', 'health', 'notifications'],
    featureKey: 'alerts-metric-alerts',
    prefixes: ['/monitoring', NOTIFICATION_CHANNELS_URL],
    except: ['/monitoring/server'],
    tabs: [
      { title: 'Alarms', icon: BellRing, url: ALERTS_URL },
      { title: 'Rules', icon: ListChecks, url: '/monitoring/rules' },
      { title: 'Events', icon: ToggleRight, url: '/monitoring/alerts' },
      { title: 'Channels', icon: Send, url: NOTIFICATION_CHANNELS_URL },
      {
        title: 'Preferences',
        icon: SlidersHorizontal,
        url: '/monitoring/notifications',
      },
    ],
  },
]

const SERVER_NODES: ServerNavArea = {
  id: 'servers',
  title: 'Nodes',
  icon: Server,
  url: '/settings/nodes',
  description: 'The machines that build and run your services.',
  add: { label: 'Add server', href: '/settings/nodes' },
  keywords: [
    'servers',
    'worker nodes',
    'cpu',
    'memory',
    'disk',
    'docker',
    'host',
  ],
  tabs: [
    {
      title: 'Nodes',
      icon: Network,
      url: '/settings/nodes',
      featureKey: 'multi-node-worker-join',
    },
    { title: 'This server', icon: Cpu, url: '/monitoring/server' },
  ],
}

/**
 * Every record of the server, whichever project uses it: the tabs of the
 * Server page. A project lists the databases its services use, and a
 * service its custom domains; these lists are for the admin who needs all
 * of them, and are where a record no project uses is found.
 */
export const SERVER_ENTITIES: ServerNavArea[] = [
  SERVER_NODES,
  {
    id: 'databases',
    title: 'Databases',
    icon: Database,
    url: '/storage',
    description: 'Managed databases, caches and object storage.',
    keywords: ['postgres', 'mysql', 'redis', 'storage', 's3', 'restore'],
    add: { label: 'Add database', href: '/storage/create' },
    tabs: [
      { title: 'Databases', icon: Database, url: '/storage' },
      { title: 'Backups', icon: DatabaseBackup, url: '/backups' },
    ],
  },
  {
    id: 'domains',
    title: 'Domains',
    icon: Globe,
    url: '/domains',
    description: 'Hostnames, their certificates and how they are served.',
    keywords: ['hostname', 'dns', 'tls', 'ssl', 'https', 'cdn'],
    add: { label: 'Add domain', href: '/domains/add' },
    tabs: [
      { title: 'Domains', icon: Globe, url: '/domains' },
      { title: 'Certificates', icon: ShieldCheck, url: '/certificates' },
      { title: 'DNS providers', icon: Cloud, url: '/dns-providers' },
      { title: 'Delivery', icon: Truck, url: '/delivery-profiles' },
    ],
  },
]

/**
 * The Server row of the tree, below the projects. It opens on the nodes,
 * the machines everything else runs on.
 */
export const SERVER_PAGE = {
  title: 'Server',
  icon: Server,
  url: SERVER_NODES.url,
} as const

/**
 * Set up once and then used from the things above, so they are pages of
 * Settings: what the platform connects to, and what agents may use.
 */
export const SETTINGS_AREA_GROUPS: { label: string; areas: ServerNavArea[] }[] =
  [
    {
      label: 'Platform',
      areas: [
        {
          id: 'git',
          title: 'Git',
          icon: FolderGit2,
          url: '/git-providers',
          description: 'The source providers services deploy from.',
          keywords: ['github', 'gitlab', 'providers', 'source', 'repository'],
        },
        {
          id: 'email',
          title: 'Email',
          icon: Mail,
          url: '/email',
          description: 'Send transactional email from your services.',
          keywords: ['smtp', 'transactional'],
        },
        {
          id: 'status-pages',
          title: 'Status pages',
          icon: Radio,
          url: '/status-pages',
          description: 'Public uptime pages for your services.',
          keywords: ['uptime', 'incidents', 'status', 'public', 'outage'],
          featureKey: 'status-pages',
        },
        {
          id: 'workflows',
          title: 'Workflows',
          icon: Workflow,
          url: '/workflows',
          description: 'Durable background work your services start.',
          keywords: ['durable', 'runs', 'jobs'],
        },
        {
          id: 'proxy',
          title: 'Proxy',
          icon: Activity,
          url: '/proxy',
          description: 'Traffic through the reverse proxy, request by request.',
          keywords: ['traffic', 'requests', 'http', 'proxy logs'],
          tabs: [
            { title: 'Traffic', icon: Activity, url: '/proxy' },
            { title: 'Requests', icon: Waypoints, url: '/proxy-logs' },
          ],
        },
      ],
    },
    {
      label: 'AI',
      areas: [
        {
          id: 'providers',
          title: 'Providers',
          icon: BrainCircuit,
          url: '/ai-gateway',
          description: 'Model providers, their usage, limits and activity.',
          keywords: ['models', 'usage', 'limits', 'gateway', 'openai'],
          featureKey: 'ai-gateway',
          prefixes: ['/agent-sandbox'],
          tabs: [
            { title: 'Providers', icon: BrainCircuit, url: '/ai-gateway' },
            { title: 'Usage', icon: BarChart3, url: '/ai-gateway/usage' },
            { title: 'Models', icon: LayoutGrid, url: '/ai-gateway/models' },
            { title: 'Limits', icon: Gauge, url: '/ai-gateway/limits' },
            { title: 'Activity', icon: History, url: '/ai-gateway/activity' },
          ],
        },
        {
          id: 'connect',
          title: 'Connect',
          icon: Plug,
          url: '/setup/ai',
          description: 'Let your own coding agent operate this server.',
          keywords: [
            'mcp',
            'model context protocol',
            'claude',
            'cursor',
            'codex',
            'skill',
            'api key',
            'agent',
            'harness',
          ],
          tabs: [
            { title: 'Agent', icon: Plug, url: '/setup/ai', width: 'form' },
            {
              title: 'MCP server',
              icon: Bot,
              url: '/settings/mcp-server',
              width: 'form',
            },
          ],
        },
        {
          id: 'sandboxes',
          title: 'Sandboxes',
          icon: Box,
          url: '/sandboxes',
          description: 'Isolated environments for agents and development.',
          keywords: ['development', 'runtime', 'workspace', 'shell'],
          featureKey: 'sandboxes-preview-environments',
          prefixes: ['/workspaces'],
        },
        {
          id: 'vault',
          title: 'Vault',
          icon: Vault,
          url: '/vault',
          description:
            'Credentials agents and sandboxes may use, and for how long.',
          keywords: ['secrets', 'access', 'bundles', 'sessions'],
          prefixes: ['/access-services'],
        },
      ],
    },
  ]

/**
 * The assistant is not a branch of the tree: "Ask", above it on every
 * page, opens the last chat or a new one.
 */
export const ASSISTANT_AREA: ServerNavArea = {
  id: 'assistant',
  title: 'Assistant',
  icon: MessageSquare,
  url: '/chat',
  description: 'Ask about your platform and change it in a chat.',
  keywords: ['ai', 'chat', 'ask', 'help'],
  featureKey: 'ai-chat',
}

export const SETTINGS_AREAS: ServerNavArea[] = SETTINGS_AREA_GROUPS.flatMap(
  (group) => group.areas
)

export const SERVER_AREAS: ServerNavArea[] = [
  SERVER_HOME,
  ...SERVER_VIEWS,
  ...SERVER_ENTITIES,
  ...SETTINGS_AREAS,
  ASSISTANT_AREA,
]

function isPathOrChild(pathname: string, base: string): boolean {
  return pathname === base || pathname.startsWith(`${base}/`)
}

/** Every URL prefix that belongs to `area`. */
function areaPrefixes(area: ServerNavArea): string[] {
  return [
    area.url,
    ...(area.tabs ?? []).map((tab) => tab.url),
    ...(area.prefixes ?? []),
  ]
}

/** Whether `pathname` is one of `area`'s pages. */
export function areaOwnsPath(area: ServerNavArea, pathname: string): boolean {
  if (area.except?.some((prefix) => isPathOrChild(pathname, prefix)))
    return false
  return areaPrefixes(area).some((prefix) => isPathOrChild(pathname, prefix))
}

export interface ServerNavLocation {
  /** The area the page belongs to. */
  area?: ServerNavArea
  /** The area's tab the page is, when the page is a tab's own. */
  tab?: ServerNavTab
}

/**
 * The area a server-wide URL belongs to. The one with the longest matching
 * prefix wins, so `/monitoring/server` is the server's, not Alerts'.
 */
export function resolveServerLocation(pathname: string): ServerNavLocation {
  let best: { area: ServerNavArea; length: number } | undefined
  for (const area of SERVER_AREAS) {
    if (area.except?.some((prefix) => isPathOrChild(pathname, prefix))) continue
    for (const prefix of areaPrefixes(area)) {
      if (
        isPathOrChild(pathname, prefix) &&
        (!best || prefix.length > best.length)
      )
        best = { area, length: prefix.length }
    }
  }
  if (!best) return {}
  const { area } = best
  return { area, tab: area.tabs?.find((tab) => tab.url === pathname) }
}

/** Whether `area` is the Projects page or one of its tabs. */
export function isProjectsView(area: ServerNavArea | undefined): boolean {
  return (
    area !== undefined && (area === SERVER_HOME || SERVER_VIEWS.includes(area))
  )
}

/** Whether `pathname` is one of the pages listed in Settings. */
export function isSettingsArea(pathname: string): boolean {
  const { area } = resolveServerLocation(pathname)
  return area !== undefined && SETTINGS_AREAS.includes(area)
}

/** Whether `area` is a tab of the Server page. */
export function isServerPage(area: ServerNavArea | undefined): boolean {
  return area !== undefined && SERVER_ENTITIES.includes(area)
}
