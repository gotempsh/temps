import {
  useConsoleExtensions,
  type ConsoleNavItem,
} from '@temps-sdk/console-kit'
import {
  Sidebar,
  SidebarContent,
  SidebarFooter,
  SidebarGroup,
  SidebarGroupLabel,
  SidebarHeader,
  SidebarMenu,
  SidebarMenuButton,
  SidebarMenuItem,
  useSidebar,
} from '@/components/ui/sidebar'
import {
  Activity,
  ArrowLeft,
  ArrowUpCircle,
  BadgeCheck,
  BarChart3,
  Bell,
  Bot,
  Boxes,
  ChevronsUpDown,
  Clock,
  Check,
  Database,
  DatabaseBackup,
  Folder,
  Gauge,
  GitBranch,
  GitFork,
  Globe,
  HardDrive,
  Home,
  Key,
  KeyRound,
  Layers,
  LogOut,
  Monitor,
  Moon,
  Network,
  Puzzle,
  Search,
  ScrollText,
  MessageSquare,
  Server,
  Settings,
  Settings2,
  Shield,
  ShieldAlert,
  Sun,
  Sparkles,
  Terminal,
  Users,
  UsersRound,
  Wand2,
} from 'lucide-react'

import { ProjectResponse } from '@/api/client'
import { getProjectBySlugOptions } from '@/api/client/@tanstack/react-query.gen'
import { useAuth } from '@/contexts/AuthContext'
import { useGettingStarted } from '@/hooks/useGettingStarted'
import { useProjectSetup } from '@/hooks/useProjectSetup'
import { usePinnedProjectTools } from '@/hooks/usePinnedProjectTools'
import { usePluginsContext } from '@/contexts/PluginsContext'
import { isPlatformToolsRoute } from '@/lib/platform-navigation'
import { resolvePluginIcon } from '@/lib/pluginIcons'
import {
  isProjectToolsRoute,
  resolveProjectPrimaryRoute,
} from '@/lib/project-navigation'
import { cn } from '@/lib/utils'
import {
  flattenProjectTools,
  projectToolGroups,
  type ProjectToolItem,
} from '@/components/project/project-tools'
import { useQuery } from '@tanstack/react-query'
import { ChevronRight, type LucideIcon } from 'lucide-react'
import { useMemo, useState } from 'react'
import { Link, useLocation } from 'react-router'
import { Avatar, AvatarFallback, AvatarImage } from '../ui/avatar'
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuGroup,
  DropdownMenuItem,
  DropdownMenuLabel,
  DropdownMenuSeparator,
  DropdownMenuSub,
  DropdownMenuSubContent,
  DropdownMenuSubTrigger,
  DropdownMenuTrigger,
} from '../ui/dropdown-menu'
import { useTheme } from 'next-themes'
import { FeatureMaturityBadge } from '@/components/feature-maturity/FeatureMaturityBadge'

interface PlatformNavItem {
  title: string
  url: string
  icon: LucideIcon
  activeWhen?: (pathname: string) => boolean
  featureKey?: string
}

interface PlatformNavGroup {
  label: string
  items: PlatformNavItem[]
}

// The fresh-install sidebar stays focused on daily work, grouped by intent so
// the short list is easy to scan. Every secondary capability remains visible
// on /tools and in Cmd+K.
const primaryPlatformGroups: PlatformNavGroup[] = [
  {
    label: 'Build & deliver',
    items: [
      { title: 'Projects', url: '/projects', icon: Folder },
      { title: 'Git providers', url: '/git-providers', icon: GitBranch },
      { title: 'Domains', url: '/domains', icon: Globe },
    ],
  },
  {
    label: 'Data',
    items: [
      { title: 'Databases', url: '/storage', icon: Database },
      { title: 'Backups', url: '/backups', icon: DatabaseBackup },
    ],
  },
  {
    label: 'Observe',
    items: [
      {
        title: 'Monitoring',
        url: '/monitoring/alerts',
        icon: Gauge,
        activeWhen: (pathname) => pathname.startsWith('/monitoring'),
        featureKey: 'alerts-metric-alerts',
      },
      { title: 'Proxy', url: '/proxy', icon: Activity },
    ],
  },
  {
    label: 'More',
    items: [
      {
        title: 'All platform tools',
        url: '/tools',
        icon: Boxes,
        activeWhen: isPlatformToolsRoute,
      },
    ],
  },
]

// Full grouped settings tree — mirrors SettingsLayout
interface SettingsGroupDef {
  label: string
  items: PlatformNavItem[]
}
// Settings drill-down contains instance configuration rather than runtime
// tools, which live on the All platform tools page.
const settingsGroups: SettingsGroupDef[] = [
  {
    label: 'General',
    items: [
      { title: 'Platform', url: '/settings', icon: Settings2 },
      { title: 'Version', url: '/settings/version', icon: ArrowUpCircle },
      { title: 'Notifications', url: '/settings/notifications', icon: Bell },
    ],
  },
  {
    label: 'Access',
    items: [
      { title: 'Users', url: '/settings/users', icon: Users },
      {
        title: 'Teams',
        url: '/settings/teams',
        icon: UsersRound,
        featureKey: 'teams',
      },
      { title: 'Authentication', url: '/settings/auth', icon: KeyRound },
      { title: 'API Keys', url: '/settings/keys', icon: Key },
    ],
  },
  {
    label: 'Infrastructure',
    items: [
      { title: 'Load Balancer', url: '/settings/load-balancer', icon: Server },
      {
        title: 'Docker Registry',
        url: '/settings/docker-registry',
        icon: Boxes,
      },
      { title: 'Build Limits', url: '/settings/build-limits', icon: Gauge },
      {
        title: 'Request Timeouts',
        url: '/settings/request-timeouts',
        icon: Clock,
      },
      {
        title: 'Worker Nodes',
        url: '/settings/nodes',
        icon: Network,
        featureKey: 'multi-node-worker-join',
      },
      {
        title: 'Plugins',
        url: '/settings/plugins',
        icon: Puzzle,
        featureKey: 'plugin-system',
      },
    ],
  },
  {
    label: 'Security',
    items: [
      { title: 'Security Headers', url: '/settings/security', icon: Shield },
      { title: 'Rate Limiting', url: '/settings/rate-limiting', icon: Monitor },
      {
        title: 'Disk Monitoring',
        url: '/settings/disk-monitoring',
        icon: HardDrive,
      },
      {
        title: 'Metrics Monitoring',
        url: '/settings/metrics-monitoring',
        icon: BarChart3,
      },
    ],
  },
]

// AI drill-down — swapped in for /ai-gateway, /chat, /agent-sandbox,
// /skills, /mcp-servers, /ai-workflows, mirroring the Settings sidebar
// swap so AI's several pages read as one coherent area instead of a
// scattered set of sidebar entries.
const AI_MODE_PREFIXES = [
  '/ai-gateway',
  '/chat',
  '/agent-sandbox',
  '/skills',
  '/mcp-servers',
  '/ai-workflows',
]
const aiNavItems: PlatformNavItem[] = [
  {
    title: 'Providers',
    url: '/ai-gateway',
    icon: Sparkles,
    featureKey: 'ai-gateway',
  },
  {
    title: 'Usage',
    url: '/ai-gateway/usage',
    icon: BarChart3,
    featureKey: 'ai-gateway',
  },
  {
    title: 'Activity',
    url: '/ai-gateway/activity',
    icon: Activity,
    featureKey: 'ai-gateway',
  },
  {
    title: 'Setup',
    url: '/ai-gateway/setup',
    icon: Terminal,
    featureKey: 'ai-gateway',
  },
  {
    title: 'Chats',
    url: '/chat',
    icon: MessageSquare,
    featureKey: 'ai-chat',
  },
  {
    title: 'Workflows',
    url: '/ai-workflows',
    icon: Bot,
    featureKey: 'ai-agents-workflows',
  },
  {
    title: 'Skills',
    url: '/skills',
    icon: Wand2,
    featureKey: 'ai-foundation-api-tools',
  },
  { title: 'MCP Servers', url: '/mcp-servers', icon: Server },
]

function NavPlugins({
  items,
}: {
  items: { title: string; url: string; icon: LucideIcon }[]
}) {
  const location = useLocation()
  const { isMinimal, isMobile } = useSidebar()

  if (items.length === 0) return null

  return (
    <SidebarGroup
      className={
        isMinimal && !isMobile ? '' : 'group-data-[collapsible=icon]:hidden'
      }
    >
      <SidebarGroupLabel className={isMinimal && !isMobile ? 'hidden' : ''}>
        Plugins
      </SidebarGroupLabel>
      <SidebarMenu>
        {items.map((item) => {
          const isActive =
            location.pathname === item.url ||
            (location.pathname.startsWith(item.url + '/') &&
              !items.some(
                (other) =>
                  other.url !== item.url &&
                  other.url.startsWith(item.url + '/') &&
                  (location.pathname === other.url ||
                    location.pathname.startsWith(other.url + '/'))
              ))
          return (
            <SidebarMenuItem key={item.title}>
              <SidebarMenuButton
                asChild
                tooltip={isMinimal && !isMobile ? item.title : undefined}
                className={cn(
                  'justify-center',
                  (!isMinimal || isMobile) && 'justify-start',
                  isActive && 'bg-sidebar-accent text-sidebar-accent-foreground'
                )}
              >
                <Link to={item.url}>
                  <item.icon />
                  {(!isMinimal || isMobile) && <span>{item.title}</span>}
                </Link>
              </SidebarMenuButton>
            </SidebarMenuItem>
          )
        })}
      </SidebarMenu>
    </SidebarGroup>
  )
}

// Command palette trigger pinned at the top of the sidebar.
// Styled like Vercel's sidebar Find input: bordered, full-width, with a
// keyboard-hint badge on the right.
function NavCommandTrigger() {
  const { isMinimal, isMobile } = useSidebar()
  const compact = isMinimal && !isMobile
  const triggerCommand = () => {
    document.dispatchEvent(
      new KeyboardEvent('keydown', { key: 'k', metaKey: true })
    )
  }
  if (compact) {
    return (
      <SidebarGroup className="pb-0">
        <SidebarMenu>
          <SidebarMenuItem>
            <SidebarMenuButton
              tooltip="Find (⌘K)"
              onClick={triggerCommand}
              className="justify-center text-muted-foreground hover:text-foreground"
            >
              <Search />
            </SidebarMenuButton>
          </SidebarMenuItem>
        </SidebarMenu>
      </SidebarGroup>
    )
  }
  return (
    <SidebarGroup className="pb-0">
      <button
        type="button"
        onClick={triggerCommand}
        className="flex h-8 w-full items-center gap-2 rounded-md border border-sidebar-border bg-transparent px-2 text-sm text-muted-foreground transition-colors hover:border-sidebar-border/80 hover:bg-sidebar-accent/40 hover:text-foreground"
      >
        <Search className="size-4 shrink-0" />
        <span className="flex-1 text-left">Find…</span>
        <kbd className="rounded border border-sidebar-border bg-sidebar/60 px-1.5 py-0.5 text-[10px] tabular-nums text-muted-foreground">
          ⌘K
        </kbd>
      </button>
    </SidebarGroup>
  )
}

export default function AppSidebar() {
  const { isMinimal, isMobile } = useSidebar()
  const { platformNavEntries } = usePluginsContext()
  const location = useLocation()
  const { logoBadge } = useConsoleExtensions()

  // Convert plugin nav entries to sidebar item format
  const pluginItems = useMemo(
    () =>
      platformNavEntries.map((entry) => ({
        title: entry.label,
        url: entry.path,
        icon: resolvePluginIcon(entry.icon),
      })),
    [platformNavEntries]
  )

  // Route-driven sidebar swap.
  //   /settings/*       → settings nav (back → default)
  //   /projects/:slug/* → project nav  (back → default)
  //   anything else     → default workspace nav
  // /projects (the list) and /projects/new keep the default nav.
  const settingsMode = location.pathname.startsWith('/settings')
  const aiMode = AI_MODE_PREFIXES.some((p) => location.pathname.startsWith(p))
  const projectMatch = location.pathname.match(/^\/projects\/([^/]+)(?:\/.*)?$/)
  const projectSlug =
    projectMatch &&
    !['new', 'import-wizard', 'import'].includes(projectMatch[1])
      ? projectMatch[1]
      : null

  // Override: user pressed Back from a route-driven swap; show DefaultNav
  // only for that exact navigation. Keyed on `location.key` rather than
  // pathname — a Link back to the very same URL (e.g. clicking "AI" again
  // right after backing out of it) still produces a new history entry with
  // a new key, so the override correctly drops and the swap re-triggers.
  // Comparing by pathname alone left the sidebar stuck on DefaultNav until
  // the user navigated somewhere else first.
  const [forceDefaultKey, setForceDefaultKey] = useState<string | null>(null)
  const forceDefault = forceDefaultKey === location.key

  const compact = isMinimal && !isMobile

  const showDefault = forceDefault || (!settingsMode && !aiMode && !projectSlug)

  return (
    <Sidebar>
      <SidebarHeader>
        <SidebarMenu>
          <SidebarMenuItem>
            <Link
              to="/"
              className={cn(
                'flex items-center gap-2 rounded-md transition-colors hover:bg-sidebar-accent/40',
                compact && 'justify-center'
              )}
            >
              <div
                className={cn(
                  'flex aspect-square size-8 items-center justify-center rounded-lg',
                  compact && 'w-6 h-6'
                )}
              >
                <img
                  src="/svg/temps-icon.svg"
                  alt="logo"
                  className="size-full"
                />
              </div>
              {!compact && (
                <div className="grid flex-1 text-left text-sm leading-tight">
                  <span className="flex items-center gap-1.5 truncate font-semibold">
                    Temps
                    {logoBadge}
                  </span>
                  <span className="truncate text-xs">
                    {import.meta.env.TEMPS_VERSION}
                  </span>
                </div>
              )}
            </Link>
          </SidebarMenuItem>
        </SidebarMenu>
      </SidebarHeader>
      <SidebarContent>
        <NavCommandTrigger />
        <GettingStartedNavItem />
        {showDefault ? (
          <DefaultNav
            pluginItems={pluginItems}
            pinnedProjectSlug={forceDefault && projectSlug ? projectSlug : null}
            onReturnToProject={() => setForceDefaultKey(null)}
          />
        ) : settingsMode ? (
          <SettingsNav onBack={() => setForceDefaultKey(location.key)} />
        ) : aiMode ? (
          <AiNav onBack={() => setForceDefaultKey(location.key)} />
        ) : projectSlug ? (
          <ProjectNav
            slug={projectSlug}
            onBack={() => setForceDefaultKey(location.key)}
          />
        ) : null}
      </SidebarContent>
      <SidebarFooter>
        <NavUser />
      </SidebarFooter>
    </Sidebar>
  )
}

/**
 * Reusable labeled nav section used by variants 2-4.
 * Mirrors NavObserve styling so it inherits hover/active states.
 */
function NavSection({
  label,
  items,
  siblingUrls,
}: {
  label: string
  items: {
    title: string
    url: string
    icon: LucideIcon
    activeWhen?: (pathname: string) => boolean
    featureKey?: string
  }[]
  // URLs of items in OTHER sections that share the sidebar. Used so a
  // parent-like url (e.g. `/settings`) doesn't light up when a more
  // specific sibling (`/settings/keys`) in a different section matches.
  siblingUrls?: string[]
}) {
  const location = useLocation()
  const { isMinimal, isMobile } = useSidebar()
  const compact = isMinimal && !isMobile
  const allUrls = useMemo(
    () => [...items.map((i) => i.url), ...(siblingUrls ?? [])],
    [items, siblingUrls]
  )
  // Active = the single longest url (across this section + siblings)
  // that is either an exact match or a path-prefix of the current
  // pathname. Keeps only the most specific match highlighted.
  const activeUrl = useMemo(
    () =>
      allUrls
        .filter(
          (url) =>
            location.pathname === url || location.pathname.startsWith(url + '/')
        )
        .reduce<string | null>(
          (best, url) =>
            best === null || url.length > best.length ? url : best,
          null
        ),
    [allUrls, location.pathname]
  )
  return (
    <SidebarGroup
      className={compact ? '' : 'group-data-[collapsible=icon]:hidden'}
    >
      <SidebarGroupLabel className={compact ? 'hidden' : ''}>
        {label}
      </SidebarGroupLabel>
      <SidebarMenu>
        {items.map((item) => {
          const isActive =
            item.activeWhen?.(location.pathname) ?? item.url === activeUrl
          return (
            <SidebarMenuItem key={item.title}>
              <SidebarMenuButton
                asChild
                tooltip={compact ? item.title : undefined}
                className={cn(
                  compact ? 'justify-center' : 'justify-start',
                  isActive && 'bg-sidebar-accent text-sidebar-accent-foreground'
                )}
              >
                <Link to={item.url}>
                  <item.icon />
                  {!compact && <span>{item.title}</span>}
                  {!compact && (
                    <FeatureMaturityBadge
                      featureKey={item.featureKey}
                      compact
                    />
                  )}
                </Link>
              </SidebarMenuButton>
            </SidebarMenuItem>
          )
        })}
      </SidebarMenu>
    </SidebarGroup>
  )
}

// Persistent link to platform setup progress, pinned just below the Find
// (⌘K) box at the top of the sidebar content so it shows on every page
// regardless of which nav mode (default/settings/project) is active. Styled
// as a bordered callout card (not a plain nav row) with a mini progress bar
// so it reads as a distinct "you have setup left" prompt. Full checklist
// detail lives on its own /setup page. Renders nothing once dismissed or
// fully complete (same visibility rule as the /setup page).
function GettingStartedNavItem() {
  const { isMinimal, isMobile } = useSidebar()
  const compact = isMinimal && !isMobile
  const { completedCount, totalCount, visible } = useGettingStarted()

  if (!visible) return null

  const pct = Math.round((completedCount / totalCount) * 100)

  if (compact) {
    return (
      <SidebarGroup className="pb-0">
        <SidebarMenu>
          <SidebarMenuItem>
            <SidebarMenuButton
              asChild
              tooltip={`Finish setup — ${completedCount}/${totalCount}`}
              className="justify-center"
            >
              <Link to="/setup">
                <BadgeCheck />
              </Link>
            </SidebarMenuButton>
          </SidebarMenuItem>
        </SidebarMenu>
      </SidebarGroup>
    )
  }

  return (
    <SidebarGroup className="pb-0">
      <Link
        to="/setup"
        className="group flex flex-col gap-2 rounded-lg border border-sidebar-border bg-sidebar-accent/40 px-3 py-2.5 transition-colors hover:border-primary/40 hover:bg-sidebar-accent/70"
      >
        <div className="flex items-center gap-2">
          <BadgeCheck className="size-4 shrink-0 text-primary" />
          <span className="flex-1 text-sm font-medium">Finish setup</span>
          <span className="text-xs tabular-nums text-muted-foreground">
            {completedCount}/{totalCount}
          </span>
        </div>
        <div className="h-1 w-full overflow-hidden rounded-full bg-sidebar-border">
          <div
            className="h-full rounded-full bg-primary transition-all duration-500"
            style={{ width: `${pct}%` }}
          />
        </div>
      </Link>
    </SidebarGroup>
  )
}

/** Light / Dark / System, nested under the account menu. */
function ThemeSubmenu() {
  const { theme, setTheme } = useTheme()
  const options = [
    { value: 'light', label: 'Light', icon: Sun },
    { value: 'dark', label: 'Dark', icon: Moon },
    { value: 'system', label: 'System', icon: Monitor },
  ] as const
  const current = options.find((o) => o.value === theme) ?? options[2]
  return (
    <DropdownMenuSub>
      <DropdownMenuSubTrigger>
        <current.icon className="mr-2 h-4 w-4" />
        <span>Appearance</span>
      </DropdownMenuSubTrigger>
      <DropdownMenuSubContent>
        {options.map((o) => (
          <DropdownMenuItem key={o.value} onClick={() => setTheme(o.value)}>
            <o.icon className="mr-2 h-4 w-4" />
            <span>{o.label}</span>
            {theme === o.value && <Check className="ml-auto h-4 w-4" />}
          </DropdownMenuItem>
        ))}
      </DropdownMenuSubContent>
    </DropdownMenuSub>
  )
}

function NavUser() {
  const { user } = useAuth()
  const { isMobile, isMinimal, setOpenMobile } = useSidebar()
  const { logout } = useAuth()
  if (!user) return null

  // Mobile renders inside a Radix Sheet (Dialog) with z-[9999] on the
  // overlay. A nested DropdownMenu portals to body and inherits z-50,
  // so the menu pops up behind the sheet and is invisible/unclickable.
  // Skip the dropdown on mobile: tap the row → /account directly,
  // with Log out as a sibling icon button so it's still one tap.
  // The desktop dropdown is unchanged.
  if (isMobile) {
    return (
      <SidebarMenu>
        <SidebarMenuItem>
          <div className="flex items-center gap-1">
            <SidebarMenuButton
              size="lg"
              asChild
              className="flex-1"
              onClick={() => setOpenMobile(false)}
            >
              <Link to="/account" aria-label="Open account settings">
                <Avatar className="h-8 w-8 rounded-lg">
                  <AvatarImage
                    src={user.avatar_url || ''}
                    alt={user.username || ''}
                  />
                  <AvatarFallback className="rounded-lg">
                    {user.username?.slice(0, 2).toUpperCase() || 'U'}
                  </AvatarFallback>
                </Avatar>
                <div className="grid min-w-0 flex-1 text-left text-sm leading-tight">
                  <span className="truncate font-semibold">
                    {user.username || 'User'}
                  </span>
                  <span className="truncate text-xs">{user.email}</span>
                </div>
              </Link>
            </SidebarMenuButton>
            <button
              type="button"
              onClick={async () => {
                await logout()
              }}
              className="inline-flex h-9 w-9 shrink-0 items-center justify-center rounded-md text-muted-foreground transition-colors hover:bg-sidebar-accent hover:text-sidebar-accent-foreground"
              aria-label="Log out"
              title="Log out"
            >
              <LogOut className="h-4 w-4" />
            </button>
          </div>
        </SidebarMenuItem>
      </SidebarMenu>
    )
  }

  return (
    <SidebarMenu>
      <SidebarMenuItem>
        <DropdownMenu>
          <DropdownMenuTrigger asChild>
            <SidebarMenuButton
              size="lg"
              className="data-[state=open]:bg-sidebar-accent data-[state=open]:text-sidebar-accent-foreground"
            >
              <Avatar className="h-8 w-8 rounded-lg">
                <AvatarImage
                  src={user.avatar_url || ''}
                  alt={user.username || ''}
                />
                <AvatarFallback className="rounded-lg">
                  {user.username?.slice(0, 2).toUpperCase() || 'U'}
                </AvatarFallback>
              </Avatar>
              {!isMinimal && (
                <div className="grid flex-1 text-left text-sm leading-tight">
                  <span className="truncate font-semibold">
                    {user.username || 'User'}
                  </span>
                  <span className="truncate text-xs">{user.email}</span>
                </div>
              )}
              <ChevronsUpDown className="ml-auto size-4" />
            </SidebarMenuButton>
          </DropdownMenuTrigger>
          <DropdownMenuContent
            className="w-(--radix-dropdown-menu-trigger-width) min-w-56 rounded-lg"
            side="right"
            align="end"
            sideOffset={4}
          >
            <DropdownMenuLabel className="p-0 font-normal">
              <div className="flex items-center gap-2 px-1 py-1.5 text-left text-sm">
                <Avatar className="h-8 w-8 rounded-lg">
                  <AvatarImage
                    src={user.avatar_url || ''}
                    alt={user.username || ''}
                  />
                  <AvatarFallback className="rounded-lg">
                    {user.username?.slice(0, 2).toUpperCase() || 'U'}
                  </AvatarFallback>
                </Avatar>
                <div className="grid flex-1 text-left text-sm leading-tight">
                  <span className="truncate font-semibold">
                    {user.username || 'User'}
                  </span>
                  <span className="truncate text-xs">{user.email}</span>
                </div>
              </div>
            </DropdownMenuLabel>
            <DropdownMenuSeparator />

            <DropdownMenuGroup>
              <DropdownMenuItem>
                <Link to="/account" className="flex items-center">
                  <BadgeCheck className="mr-2 h-4 w-4" />
                  <span>Account</span>
                </Link>
              </DropdownMenuItem>
              {/* Appearance lives with the account rather than as a fourth
                  icon in the header — it's a per-user preference you set once,
                  not something you reach for while working. */}
              <ThemeSubmenu />
            </DropdownMenuGroup>
            <DropdownMenuSeparator />
            <DropdownMenuItem
              onClick={async () => {
                await logout()
              }}
            >
              <LogOut />
              Log out
            </DropdownMenuItem>
          </DropdownMenuContent>
        </DropdownMenu>
      </SidebarMenuItem>
    </SidebarMenu>
  )
}

// ─────────────────────────────────────────────────────────────────────────────
// Default workspace nav (root /, /sandboxes, /proxy, plugins, …).
// ─────────────────────────────────────────────────────────────────────────────

interface NavProps {
  pluginItems: { title: string; url: string; icon: LucideIcon }[]
  // Slug of the project the user is currently viewing (URL still
  // points inside `/projects/:slug/...`) but has temporarily swapped
  // the sidebar to default via Back. When set, render a pinned row at
  // the top so they can return to the project sidebar in one click.
  pinnedProjectSlug?: string | null
  onReturnToProject?: () => void
}

function ExtensionNav({ items }: { items?: ConsoleNavItem[] }) {
  const location = useLocation()
  const { isMinimal, isMobile } = useSidebar()
  const compact = isMinimal && !isMobile

  if (!items || items.length === 0) return null

  const sections: string[] = []
  const bySection = new Map<string, ConsoleNavItem[]>()
  for (const item of items) {
    const key = item.section ?? 'Enterprise'
    if (!bySection.has(key)) {
      bySection.set(key, [])
      sections.push(key)
    }
    bySection.get(key)!.push(item)
  }

  return (
    <>
      {sections.map((section) => (
        <SidebarGroup
          key={section}
          className={compact ? '' : 'group-data-[collapsible=icon]:hidden'}
        >
          <SidebarGroupLabel className={compact ? 'hidden' : ''}>
            {section}
          </SidebarGroupLabel>
          <SidebarMenu>
            {bySection.get(section)!.map((item) => {
              const isActive =
                location.pathname === item.path ||
                location.pathname.startsWith(item.path + '/')
              return (
                <SidebarMenuItem key={item.id}>
                  <SidebarMenuButton
                    asChild
                    tooltip={compact ? item.label : undefined}
                    className={cn(
                      'justify-center',
                      !compact && 'justify-start',
                      isActive &&
                        'bg-sidebar-accent text-sidebar-accent-foreground'
                    )}
                  >
                    <Link to={item.path}>
                      {item.icon}
                      {!compact && <span>{item.label}</span>}
                    </Link>
                  </SidebarMenuButton>
                </SidebarMenuItem>
              )
            })}
          </SidebarMenu>
        </SidebarGroup>
      ))}
    </>
  )
}

function DefaultNav({
  pluginItems,
  pinnedProjectSlug,
  onReturnToProject,
}: NavProps) {
  const { isMinimal, isMobile } = useSidebar()
  const compact = isMinimal && !isMobile

  const { navItems: extraNavItems } = useConsoleExtensions()
  const platformUrls = useMemo(
    () =>
      primaryPlatformGroups.flatMap((group) =>
        group.items.map((item) => item.url)
      ),
    []
  )

  return (
    <>
      {pinnedProjectSlug && onReturnToProject && (
        <CurrentProjectPin
          slug={pinnedProjectSlug}
          onReturn={onReturnToProject}
        />
      )}
      {primaryPlatformGroups.map((group) => (
        <NavSection
          key={group.label}
          label={group.label}
          items={group.items}
          siblingUrls={platformUrls.filter(
            (url) => !group.items.some((item) => item.url === url)
          )}
        />
      ))}
      <NavPlugins items={pluginItems} />
      <ExtensionNav items={extraNavItems} />
      <SidebarGroup className="mt-auto">
        <SidebarMenu>
          <SidebarMenuItem>
            <SidebarMenuButton
              asChild
              tooltip={compact ? 'Settings' : undefined}
              className={compact ? 'justify-center' : 'justify-start'}
            >
              <Link to="/settings">
                <Settings />
                {!compact && <span>Settings</span>}
              </Link>
            </SidebarMenuButton>
          </SidebarMenuItem>
        </SidebarMenu>
      </SidebarGroup>
    </>
  )
}

// ─────────────────────────────────────────────────────────────────────────────
// Settings nav — replaces the whole sidebar when on /settings/*.
// Back button returns to root.
// ─────────────────────────────────────────────────────────────────────────────

function SettingsNav({ onBack }: { onBack: () => void }) {
  // Every url across every settings group. Each section gets the list
  // minus its own items so active-state resolution sees the full tree
  // (prevents `/settings` lighting up on `/settings/keys`).
  const allSettingsUrls = settingsGroups.flatMap((g) =>
    g.items.map((i) => i.url)
  )
  return (
    <>
      <SwapHeader title="Settings" onBack={onBack} backLabel="Back to menu" />
      {settingsGroups.map((group) => {
        const ownUrls = new Set(group.items.map((i) => i.url))
        const siblings = allSettingsUrls.filter((u) => !ownUrls.has(u))
        return (
          <NavSection
            key={group.label}
            label={group.label}
            items={group.items}
            siblingUrls={siblings}
          />
        )
      })}
    </>
  )
}

// ─────────────────────────────────────────────────────────────────────────────
// AI nav — replaces the whole sidebar for the AI area (Providers, Usage,
// Chats, Workflows, Skills, MCP Servers). Back button returns to root.
// ─────────────────────────────────────────────────────────────────────────────

function AiNav({ onBack }: { onBack: () => void }) {
  return (
    <>
      <SwapHeader title="AI" onBack={onBack} backLabel="Back to menu" />
      <NavSection label="AI" items={aiNavItems} />
    </>
  )
}

// ─────────────────────────────────────────────────────────────────────────────
// Project nav — replaces the whole sidebar when on /projects/:slug/*.
// ─────────────────────────────────────────────────────────────────────────────

type ProjectNavItem = ProjectToolItem

interface ProjectNavGroup {
  label: string
  items: ProjectNavItem[]
}

// The permanent sidebar is intentionally short and task-first. These are the
// destinations operators reach for every day, and each one opens the data in a
// single click. Everything else remains visible in the grouped tools menu
// below, without replacing the sidebar or hiding behind route-driven state.
const projectPrimaryGroups: ProjectNavGroup[] = [
  {
    label: 'Project',
    items: [
      { title: 'Overview', url: 'project', icon: Home },
      { title: 'Deployments', url: 'deployments', icon: GitBranch },
    ],
  },
  {
    label: 'Data',
    items: [
      {
        title: 'Analytics',
        url: 'analytics',
        icon: BarChart3,
        featureKey: 'web-analytics',
      },
      {
        title: 'Errors',
        url: 'errors',
        icon: ShieldAlert,
        featureKey: 'error-tracking',
      },
      {
        title: 'Traces',
        url: 'traces',
        icon: Network,
        featureKey: 'otel-traces-metrics',
      },
      { title: 'Logs', url: 'runtime', icon: ScrollText },
      { title: 'Databases', url: 'storage', icon: Database },
    ],
  },
  {
    label: 'Configure',
    items: [
      { title: 'Environments', url: 'environments', icon: Layers },
      { title: 'Domains', url: 'domains', icon: Globe },
      { title: 'Git', url: 'git', icon: GitFork },
      { title: 'Build & Deploy', url: 'build', icon: Settings2 },
      { title: 'Project settings', url: 'settings', icon: Settings },
    ],
  },
]

function ProjectSetupNavItem({ project }: { project: ProjectResponse }) {
  const { isMinimal, isMobile, setOpenMobile } = useSidebar()
  const compact = isMinimal && !isMobile
  const setup = useProjectSetup(project)
  const remainingSteps = setup.steps.filter((step) => !step.done)

  if (setup.isLoading || remainingSteps.length === 0) return null

  if (compact) {
    return (
      <SidebarGroup className="py-0 pb-2">
        <SidebarMenu>
          <SidebarMenuItem>
            <SidebarMenuButton
              asChild
              tooltip={`Project setup — ${setup.completedCount}/${setup.totalCount}`}
              className="justify-center"
            >
              <Link
                to={`/projects/${project.slug}/setup`}
                onClick={() => isMobile && setOpenMobile(false)}
              >
                <BadgeCheck />
              </Link>
            </SidebarMenuButton>
          </SidebarMenuItem>
        </SidebarMenu>
      </SidebarGroup>
    )
  }

  return (
    <SidebarGroup className="py-0 pb-1">
      <Link
        to={`/projects/${project.slug}/setup`}
        onClick={() => isMobile && setOpenMobile(false)}
        className="group rounded-md px-2 py-2 transition-colors hover:bg-sidebar-accent focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-sidebar-ring"
      >
        <div className="flex items-center gap-2">
          <BadgeCheck className="size-4 shrink-0 text-primary" />
          <span className="flex-1 text-sm font-medium">Finish setup</span>
          <span className="text-xs tabular-nums text-muted-foreground">
            {setup.completedCount}/{setup.totalCount}
          </span>
        </div>
        <div
          className="mt-1.5 h-0.5 w-full overflow-hidden rounded-full bg-sidebar-border"
          role="progressbar"
          aria-label={`${project.name} setup progress`}
          aria-valuemin={0}
          aria-valuemax={setup.totalCount}
          aria-valuenow={setup.completedCount}
        >
          <div
            className="h-full rounded-full bg-primary transition-all duration-500"
            style={{ width: `${setup.percent}%` }}
          />
        </div>
      </Link>
    </SidebarGroup>
  )
}

function ProjectNav({ slug, onBack }: { slug: string; onBack: () => void }) {
  const { data: project } = useQuery({
    ...getProjectBySlugOptions({ path: { slug } }),
  })
  const { projectNavEntries } = usePluginsContext()
  const location = useLocation()
  const { isMinimal, isMobile, setOpenMobile } = useSidebar()
  const compact = isMinimal && !isMobile
  const pluginItems = useMemo<ProjectNavItem[]>(
    () =>
      projectNavEntries.map((entry) => ({
        title: entry.label,
        url: entry.path,
        icon: resolvePluginIcon(entry.icon),
      })),
    [projectNavEntries]
  )
  const availableTools = useMemo(
    () => [...flattenProjectTools(projectToolGroups), ...pluginItems],
    [pluginItems]
  )
  const { pinnedUrls } = usePinnedProjectTools(
    slug,
    availableTools.map((tool) => tool.url)
  )
  const toolByUrl = useMemo(
    () => new Map(availableTools.map((tool) => [tool.url, tool])),
    [availableTools]
  )
  const pinnedItems = pinnedUrls
    .map((url) => toolByUrl.get(url))
    .filter((tool): tool is ProjectToolItem => tool !== undefined)

  const activeRoute = useMemo(() => {
    if (!project) return ''
    const parts = location.pathname.split('/')
    const slugIdx = parts.indexOf(project.slug)
    if (slugIdx === -1) return ''
    return parts.slice(slugIdx + 1).join('/')
  }, [location.pathname, project])

  const pinnedActiveUrl = pinnedItems.find((item) => {
    const route = item.url.split('?')[0]
    return activeRoute === route || activeRoute.startsWith(`${route}/`)
  })?.url
  const primaryActiveUrl = pinnedActiveUrl
    ? null
    : resolveProjectPrimaryRoute(activeRoute)

  if (!project) {
    return (
      <>
        <SwapHeader title="Loading…" onBack={onBack} backLabel="Back to menu" />
      </>
    )
  }

  const toolsActive = isProjectToolsRoute(activeRoute) && !pinnedActiveUrl

  return (
    <>
      <SwapHeader
        title={project.name}
        onBack={onBack}
        backLabel="Back to menu"
      />
      <ProjectSetupNavItem project={project} />
      {projectPrimaryGroups.map((group) => (
        <SidebarGroup key={group.label} className="py-1">
          <SidebarGroupLabel className={compact ? 'hidden' : ''}>
            {group.label}
          </SidebarGroupLabel>
          <SidebarMenu>
            {group.items.map((item) => {
              const active = primaryActiveUrl === item.url.split('?')[0]
              return (
                <SidebarMenuItem
                  key={item.url}
                  data-tour={item.url.split('?')[0]}
                >
                  <SidebarMenuButton
                    asChild
                    tooltip={compact ? item.title : undefined}
                    className={cn(
                      compact ? 'justify-center' : 'justify-start',
                      active &&
                        'bg-sidebar-accent text-sidebar-accent-foreground'
                    )}
                  >
                    <Link
                      to={`/projects/${project.slug}/${item.url}`}
                      onClick={() => isMobile && setOpenMobile(false)}
                    >
                      <item.icon />
                      {!compact && <span>{item.title}</span>}
                      {!compact && (
                        <FeatureMaturityBadge
                          featureKey={item.featureKey}
                          compact
                        />
                      )}
                    </Link>
                  </SidebarMenuButton>
                </SidebarMenuItem>
              )
            })}
          </SidebarMenu>
        </SidebarGroup>
      ))}
      {pinnedItems.length > 0 && (
        <SidebarGroup className="py-1">
          <SidebarGroupLabel className={compact ? 'hidden' : ''}>
            Pinned
          </SidebarGroupLabel>
          <SidebarMenu>
            {pinnedItems.map((item) => (
              <SidebarMenuItem key={item.url}>
                <SidebarMenuButton
                  asChild
                  tooltip={compact ? item.title : undefined}
                  className={cn(
                    compact ? 'justify-center' : 'justify-start',
                    pinnedActiveUrl === item.url &&
                      'bg-sidebar-accent text-sidebar-accent-foreground'
                  )}
                >
                  <Link
                    to={
                      item.url.startsWith('/')
                        ? item.url
                        : `/projects/${project.slug}/${item.url}`
                    }
                    onClick={() => isMobile && setOpenMobile(false)}
                  >
                    <item.icon />
                    {!compact && <span>{item.title}</span>}
                    {!compact && (
                      <FeatureMaturityBadge
                        featureKey={item.featureKey}
                        compact
                      />
                    )}
                  </Link>
                </SidebarMenuButton>
              </SidebarMenuItem>
            ))}
          </SidebarMenu>
        </SidebarGroup>
      )}
      <SidebarGroup className="py-1">
        <SidebarMenu>
          <SidebarMenuItem data-tour="all-tools">
            <ProjectToolsMenu
              slug={project.slug}
              active={toolsActive}
              compact={compact}
              onNavigate={() => setOpenMobile(false)}
            />
          </SidebarMenuItem>
        </SidebarMenu>
      </SidebarGroup>
    </>
  )
}

function ProjectToolsMenu({
  slug,
  active,
  compact,
  onNavigate,
}: {
  slug: string
  active: boolean
  compact: boolean
  onNavigate: () => void
}) {
  return (
    <SidebarMenuButton
      asChild
      tooltip={compact ? 'All project tools' : undefined}
      className={cn(
        compact ? 'justify-center' : 'justify-start',
        active && 'bg-sidebar-accent text-sidebar-accent-foreground'
      )}
    >
      <Link to={`/projects/${slug}/tools`} onClick={onNavigate}>
        <Boxes />
        {!compact && (
          <>
            <span className="flex-1 text-left">All project tools</span>
            <ChevronRight className="size-4 text-muted-foreground" />
          </>
        )}
      </Link>
    </SidebarMenuButton>
  )
}

// Inverse of SwapHeader: shown at the top of DefaultNav when the user
// pressed Back from a project sidebar but the URL is still inside that
// project. One click restores the project sidebar without navigating.
function CurrentProjectPin({
  slug,
  onReturn,
}: {
  slug: string
  onReturn: () => void
}) {
  const { isMinimal, isMobile } = useSidebar()
  const compact = isMinimal && !isMobile
  const { data: project } = useQuery({
    ...getProjectBySlugOptions({ path: { slug } }),
  })
  const label = project?.name ?? slug
  if (compact) {
    return (
      <SidebarGroup className="pb-0">
        <SidebarMenu>
          <SidebarMenuItem>
            <SidebarMenuButton
              tooltip={`Open ${label}`}
              onClick={onReturn}
              className="justify-center"
            >
              <Folder />
            </SidebarMenuButton>
          </SidebarMenuItem>
        </SidebarMenu>
      </SidebarGroup>
    )
  }
  return (
    <SidebarGroup className="pb-0">
      <button
        type="button"
        onClick={onReturn}
        className="flex h-8 w-full items-center gap-2 rounded-md px-2 text-left text-sm transition-colors hover:bg-sidebar-accent"
      >
        <Folder className="size-4 shrink-0 text-muted-foreground" />
        <span className="min-w-0 flex-1 truncate font-medium text-foreground">
          {label}
        </span>
        <ChevronRight className="size-4 shrink-0 text-muted-foreground" />
      </button>
    </SidebarGroup>
  )
}

// Shared back-arrow header used by Settings, Project, and drill-down
// sub-views. `onBack` is a state callback — it never navigates.
//
// `backLabel` names the destination, and is only surfaced when the sidebar is
// collapsed: expanded, the row reads "← Analytics" (where you *are*, which the
// surrounding items already imply), but collapsed there is nothing but an arrow,
// so the tooltip has to say where it goes.
function SwapHeader({
  title,
  onBack,
  backLabel = 'Back',
}: {
  title: string
  onBack: () => void
  backLabel?: string
}) {
  const { isMinimal, isMobile } = useSidebar()
  const compact = isMinimal && !isMobile
  // Collapsed, this used to render nothing at all — leaving a second-level nav
  // (e.g. a project's Analytics sub-items) with no way back out except
  // re-expanding the sidebar or using the breadcrumb.
  if (compact) {
    return (
      <SidebarGroup className="pb-0">
        <SidebarMenu>
          <SidebarMenuItem>
            <SidebarMenuButton
              tooltip={backLabel}
              aria-label={backLabel}
              onClick={onBack}
              className="justify-center text-muted-foreground hover:text-foreground"
            >
              <ArrowLeft />
            </SidebarMenuButton>
          </SidebarMenuItem>
        </SidebarMenu>
      </SidebarGroup>
    )
  }
  return (
    <SidebarGroup className="pb-0">
      <button
        type="button"
        onClick={onBack}
        className="flex h-8 w-full items-center gap-2 rounded-md px-2 text-left text-sm text-muted-foreground transition-colors hover:bg-sidebar-accent hover:text-foreground"
      >
        <ArrowLeft className="size-4" />
        <span className="truncate font-medium text-foreground">{title}</span>
      </button>
    </SidebarGroup>
  )
}
