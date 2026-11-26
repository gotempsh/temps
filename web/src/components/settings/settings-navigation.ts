// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import {
  Activity,
  ArrowUpCircle,
  BarChart3,
  Boxes,
  Clock,
  Cloud,
  Gauge,
  Globe,
  Hammer,
  HardDrive,
  HeartPulse,
  Key,
  KeyRound,
  LockKeyhole,
  Network,
  Puzzle,
  RadioTower,
  ScrollText,
  Server,
  Settings2,
  Shield,
  ShieldCheck,
  Users,
  UsersRound,
  Waypoints,
} from 'lucide-react'
import type { ComponentType, ReactNode } from 'react'
import { createElement, Fragment } from 'react'
import type { ConsoleSettingsNavItem } from '@temps-sdk/console-kit'
import { SETTINGS_AREA_GROUPS, areaOwnsPath } from '@/lib/server-navigation'
import {
  AUDIT_LOGS_URL,
  PLUGINS_MANAGEMENT_URL,
} from '@/components/dashboard/primary-navigation'

/**
 * Anything renderable as `<Icon className=... />`. Built-in entries use
 * lucide components; extension entries arrive as a `ReactNode` and are
 * adapted by `mergeSettingsNavigationGroups`.
 */
export type SettingsNavigationIcon = ComponentType<{ className?: string }>

/** A page of an entry that holds several, shown as a tab above them. */
export interface SettingsNavigationTab {
  title: string
  url: string
  icon: SettingsNavigationIcon
}

export interface SettingsNavigationItem {
  /** Stable key for rendering. Built-in entries are keyed by url. */
  id?: string
  title: string
  url: string
  icon: SettingsNavigationIcon
  /** One line on what is set here, shown on the Settings overview. */
  description?: string
  featureKey?: string
  /** Extra Cmd+K search terms. */
  keywords?: string[]
  /** The entry's pages, when it has more than one. The first is `url`. */
  tabs?: SettingsNavigationTab[]
  /**
   * Set for an entry whose pages are not all below `url` (records, forms and
   * tabs at other paths): whether `pathname` is one of them.
   */
  ownsPath?: (pathname: string) => boolean
}

export interface SettingsNavigationGroup {
  label: string
  items: SettingsNavigationItem[]
}

/**
 * Canonical instance-settings navigation.
 *
 * The Settings sidebar, the Settings overview and Cmd+K all read this
 * registry, so a settings page cannot be visible in one and silently absent
 * from another.
 *
 * It opens with what the platform connects to and what agents may use
 * (Git, email, model providers): set up once, then used from projects, so
 * they are settings and not entries of the sidebar's tree. Their pages keep
 * the URLs they always had (see `lib/server-navigation.ts`).
 *
 * Servers (worker nodes) and notification channels keep their
 * `/settings/...` URLs but are not listed: servers are a branch of the
 * tree, and channels are a tab of Alerts.
 */
export const settingsNavigationGroups: SettingsNavigationGroup[] = [
  ...SETTINGS_AREA_GROUPS.map((group) => ({
    label: group.label,
    items: group.areas.map((area): SettingsNavigationItem => ({
      id: area.id,
      title: area.title,
      url: area.url,
      icon: area.icon,
      description: area.description,
      featureKey: area.featureKey,
      keywords: area.keywords,
      ownsPath: (pathname) => areaOwnsPath(area, pathname),
    })),
  })),
  {
    label: 'Access',
    items: [
      {
        title: 'Users',
        url: '/settings/users',
        icon: Users,
        description: 'Who can sign in, and with which role.',
      },
      {
        title: 'Teams',
        url: '/settings/teams',
        icon: UsersRound,
        description: 'Group users and scope them to projects.',
        featureKey: 'teams',
      },
      {
        title: 'Sign-in',
        url: '/settings/auth',
        icon: KeyRound,
        description: 'Passwords, single sign-on and identity providers.',
        keywords: ['authentication', 'oidc', 'sso', 'login', 'identity'],
      },
      {
        title: 'API keys',
        url: '/settings/keys',
        icon: Key,
        description: 'Keys for the CLI, CI and scripts.',
        keywords: ['tokens', 'cli', 'automation'],
      },
      {
        title: 'Audit log',
        url: AUDIT_LOGS_URL,
        icon: ScrollText,
        description: 'Who changed what, and when.',
        keywords: ['audit logs', 'security', 'activity', 'history'],
      },
    ],
  },
  {
    label: 'Security',
    items: [
      {
        title: 'Security',
        url: '/settings/security',
        icon: Shield,
        description: 'Who can reach the console, and what Shield blocks.',
      },
      {
        title: 'Shield proposals',
        url: '/settings/shield-proposals',
        icon: ShieldCheck,
        description: 'Approve what Shield learned from attacks.',
        keywords: ['learning', 'attackers', 'approve', 'block', 'review'],
      },
      {
        title: 'Network policy',
        url: '/settings/network-policy',
        icon: Network,
        description: 'What services may reach, and what may reach them.',
        keywords: ['egress', 'ingress', 'firewall', 'rules', 'isolation'],
      },
      {
        title: 'Rate limiting',
        url: '/settings/rate-limiting',
        icon: Gauge,
        description: 'Request limits per IP, and IP access control.',
      },
      {
        title: 'On-demand TLS',
        url: '/settings/on-demand-tls',
        icon: LockKeyhole,
        description: 'Issue a certificate when a hostname is first requested.',
        keywords: ['certificates', 'https', 'ssl', "let's encrypt", 'acme'],
      },
    ],
  },
  {
    label: 'Instance',
    items: [
      {
        title: 'Instance',
        url: '/settings',
        icon: Settings2,
        description: 'The address of this server and its preview domain.',
        keywords: ['platform', 'general', 'external url', 'preview domain'],
      },
      {
        title: 'Version',
        url: '/settings/version',
        icon: ArrowUpCircle,
        description: 'The running version and available updates.',
        keywords: ['update', 'upgrade', 'release'],
      },
      {
        title: 'Load balancer',
        url: '/settings/load-balancer',
        icon: Server,
        description: 'Custom routes served by the proxy.',
      },
      {
        title: 'Docker registry',
        url: '/settings/docker-registry',
        icon: Boxes,
        description: 'The registry built images are pushed to.',
      },
      {
        title: 'DNS',
        url: '/settings/dns',
        icon: Globe,
        description: 'Name resolution for services, and its queries.',
        keywords: [
          'cluster dns',
          'resolver',
          'temps.local',
          'nxdomain',
          'servfail',
          'name resolution',
        ],
      },
      {
        title: 'Traefik discovery',
        url: '/settings/traefik-discovery',
        icon: Waypoints,
        description: 'Route containers that carry Traefik labels.',
      },
      {
        title: 'Limits',
        url: '/settings/build-limits',
        icon: Gauge,
        description: 'Build resources, request timeouts and health checks.',
        keywords: [
          'build limits',
          'request timeouts',
          'service health checks',
          'healthcheck',
          'docker healthcheck',
          'health check interval',
          'database health',
        ],
        tabs: [
          { title: 'Builds', url: '/settings/build-limits', icon: Hammer },
          {
            title: 'Request timeouts',
            url: '/settings/request-timeouts',
            icon: Clock,
          },
          {
            title: 'Health checks',
            url: '/settings/service-healthchecks',
            icon: HeartPulse,
          },
        ],
      },
    ],
  },
  {
    label: 'Data',
    items: [
      {
        title: 'Metrics collection',
        url: '/settings/metrics-monitoring',
        icon: BarChart3,
        description: 'How metrics are sampled, compressed and kept.',
        keywords: ['metrics monitoring', 'retention', 'sampling'],
      },
      {
        title: 'Disk monitoring',
        url: '/settings/disk-monitoring',
        icon: HardDrive,
        description: 'Warn before a disk fills up.',
      },
      {
        title: 'OTel pipeline',
        url: '/settings/otel-pipeline',
        icon: Activity,
        description: 'State of trace, metric and log ingest.',
        keywords: ['opentelemetry', 'ingest', 'traces'],
      },
      {
        title: 'Usage reporting',
        url: '/settings/telemetry',
        icon: RadioTower,
        description: 'The anonymous usage data this server sends.',
        keywords: [
          'telemetry',
          'privacy',
          'anonymous',
          'usage data',
          'opt out',
        ],
      },
      {
        title: 'Temps Cloud',
        url: '/settings/cloud',
        icon: Cloud,
        description: 'What this server mirrors to Temps Cloud.',
      },
      {
        title: 'Extensions',
        url: PLUGINS_MANAGEMENT_URL,
        icon: Puzzle,
        description: 'Installed plugins and what each adds.',
        keywords: ['plugins', 'install', 'add-ons'],
      },
    ],
  },
]

/** The entry, and its tab, whose page `pathname` is. */
export function resolveSettingsTab(
  pathname: string,
  groups: readonly SettingsNavigationGroup[] = settingsNavigationGroups
): { item: SettingsNavigationItem; tab: SettingsNavigationTab } | undefined {
  for (const group of groups) {
    for (const item of group.items) {
      const tab = item.tabs?.find((candidate) => candidate.url === pathname)
      if (tab) return { item, tab }
    }
  }
  return undefined
}

/**
 * A component that renders a fixed node regardless of props. Lets a
 * `ReactNode` icon from an extension satisfy the `SettingsNavigationIcon`
 * component contract the sidebar and Cmd+K render with. Cached per node so
 * repeated merges hand back the same component and React does not remount
 * the icon on every render.
 */
const nodeIconCache = new WeakMap<object, SettingsNavigationIcon>()
function nodeIcon(node: ReactNode): SettingsNavigationIcon {
  if (node !== null && typeof node === 'object') {
    const cached = nodeIconCache.get(node)
    if (cached) return cached
    const Icon = makeNodeIcon(node)
    nodeIconCache.set(node, Icon)
    return Icon
  }
  return makeNodeIcon(node)
}
function makeNodeIcon(node: ReactNode): SettingsNavigationIcon {
  const NodeIcon: SettingsNavigationIcon = () =>
    createElement(Fragment, null, node)
  NodeIcon.displayName = 'SettingsExtensionIcon'
  return NodeIcon
}

/**
 * Merge extension-provided Settings links into the canonical groups.
 *
 * Items whose `group` matches a built-in label are appended to that group
 * after its own entries; any other label becomes a new group after the
 * built-in ones, in first-seen order. Items whose path is not under
 * `/settings/` are dropped, since the sidebar would render them with the
 * wrong nav. Never mutates `groups`.
 */
export function mergeSettingsNavigationGroups(
  groups: readonly SettingsNavigationGroup[],
  extensions: readonly ConsoleSettingsNavItem[] | undefined
): SettingsNavigationGroup[] {
  if (!extensions || extensions.length === 0) return [...groups]
  const merged: SettingsNavigationGroup[] = groups.map((g) => ({
    ...g,
    items: [...g.items],
  }))
  const byLabel = new Map(merged.map((g) => [g.label, g]))
  for (const ext of extensions) {
    if (!ext.path.startsWith('/settings/')) continue
    let group = byLabel.get(ext.group)
    if (!group) {
      group = { label: ext.group, items: [] }
      byLabel.set(ext.group, group)
      merged.push(group)
    }
    group.items.push({
      id: ext.id,
      title: ext.label,
      url: ext.path,
      icon: nodeIcon(ext.icon),
      keywords: ext.keywords,
    })
  }
  return merged
}
