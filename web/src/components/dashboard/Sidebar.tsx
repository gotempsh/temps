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
  AlarmClock,
  ArrowLeft,
  ArrowUpCircle,
  BadgeCheck,
  BarChart3,
  Bell,
  Bot,
  Box,
  Boxes,
  ChevronsUpDown,
  Clock,
  Cloud,
  Check,
  CreditCard,
  Database,
  DatabaseBackup,
  FileText,
  FileLock2,
  Filter,
  Flag,
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
  LineChart,
  LogOut,
  Mail,
  Monitor,
  Moon,
  Network,
  Play,
  Puzzle,
  Radio,
  Rss,
  Search,
  ScrollText,
  MessageSquare,
  Server,
  Settings,
  Settings2,
  Shield,
  ShieldAlert,
  Sun,
  ShieldCheck,
  SlidersHorizontal,
  Sparkles,
  Terminal,
  Users,
  UsersRound,
  Wand2,
  Webhook,
  Workflow,
  Zap,
} from 'lucide-react'

import { ProjectResponse } from '@/api/client'
import { getProjectBySlugOptions } from '@/api/client/@tanstack/react-query.gen'
import { useAuth } from '@/contexts/AuthContext'
import { useGettingStarted } from '@/hooks/useGettingStarted'
import { useProjectSetup } from '@/hooks/useProjectSetup'
import { useCanViewAuditLogs } from '@/hooks/useAuditAccess'
import { usePluginsContext } from '@/contexts/PluginsContext'
import { resolvePluginIcon } from '@/lib/pluginIcons'
import { cn } from '@/lib/utils'
import { useQuery } from '@tanstack/react-query'
import { ChevronRight, Eye, type LucideIcon } from 'lucide-react'
import { useEffect, useMemo, useRef, useState } from 'react'
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

// Daily-use root: short, scannable list. Dense areas (AI, Source) drill
// down into sub-views per the §6.12 sidebar standard.
interface PlatformNavItem {
  title: string
  url: string
  icon: LucideIcon
  subItems?: { title: string; url: string; icon: LucideIcon }[]
}

const navWorkflow: PlatformNavItem[] = [
  { title: 'Projects', url: '/projects', icon: Folder },
  { title: 'Sandboxes', url: '/sandboxes', icon: Box },
  {
    title: 'Domains',
    url: '/domains',
    icon: Globe,
    subItems: [
      { title: 'Domains', url: '/domains', icon: Globe },
      { title: 'Certificates', url: '/certificates', icon: ShieldCheck },
    ],
  },
  {
    title: 'Storage',
    url: '/storage',
    icon: Database,
    subItems: [
      { title: 'Databases', url: '/storage', icon: Database },
      { title: 'Backups', url: '/backups', icon: DatabaseBackup },
    ],
  },
  { title: 'Email', url: '/email', icon: Mail },
  { title: 'AI', url: '/ai-gateway', icon: Sparkles },
  {
    title: 'Source',
    url: '/git-providers',
    icon: GitBranch,
    subItems: [
      { title: 'Git Providers', url: '/git-providers', icon: GitBranch },
      { title: 'DNS Providers', url: '/dns-providers', icon: Cloud },
    ],
  },
]

// Observability section
const navObservability = [
  { title: 'Monitoring', url: '/monitoring', icon: Gauge },
  { title: 'Proxy', url: '/proxy', icon: Activity },
  { title: 'Proxy Logs', url: '/proxy-logs', icon: Network },
  { title: 'Audit Logs', url: '/audit-logs', icon: ScrollText },
]

// Full grouped settings tree — mirrors SettingsLayout
interface SettingsGroupDef {
  label: string
  items: { title: string; url: string; icon: LucideIcon }[]
}
// Settings drill-down only contains items NOT already surfaced in the
// main app sidebar (Platform / Storage / AI / Source sections in
// `navWorkflow`). Anything reachable from the root sidebar is omitted
// here to avoid duplicate entry points.
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
      { title: 'Teams', url: '/settings/teams', icon: UsersRound },
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
      { title: 'Worker Nodes', url: '/settings/nodes', icon: Network },
      { title: 'Plugins', url: '/settings/plugins', icon: Puzzle },
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
const aiNavItems: { title: string; url: string; icon: LucideIcon }[] = [
  { title: 'Providers', url: '/ai-gateway', icon: Sparkles },
  { title: 'Usage', url: '/ai-gateway/usage', icon: BarChart3 },
  { title: 'Activity', url: '/ai-gateway/activity', icon: Activity },
  { title: 'Setup', url: '/ai-gateway/setup', icon: Terminal },
  { title: 'Chats', url: '/chat', icon: MessageSquare },
  { title: 'Workflows', url: '/ai-workflows', icon: Bot },
  { title: 'Skills', url: '/skills', icon: Wand2 },
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
  items: { title: string; url: string; icon: LucideIcon }[]
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
          const isActive = item.url === activeUrl
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
// Default workspace nav (root /, /sandboxes, /monitoring, plugins, …).
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

  // Split flat items from grouped items. Items with subItems render as
  // their own labeled sub-section (parent title becomes the group
  // label, children become flat links). Items without subItems stay in
  // the main "Platform" group at the top.
  const flatItems = navWorkflow.filter((it) => !it.subItems?.length)
  const grouped = navWorkflow.filter((it) => it.subItems?.length)
  const { navItems: extraNavItems } = useConsoleExtensions()
  const canViewAuditLogs = useCanViewAuditLogs()
  const observabilityItems = canViewAuditLogs
    ? navObservability
    : navObservability.filter((it) => it.url !== '/audit-logs')

  return (
    <>
      {pinnedProjectSlug && onReturnToProject && (
        <CurrentProjectPin
          slug={pinnedProjectSlug}
          onReturn={onReturnToProject}
        />
      )}
      <NavSection label="Platform" items={flatItems} />
      {grouped.map((group) => (
        <NavSection
          key={group.title}
          label={group.title}
          items={group.subItems!}
        />
      ))}
      <NavSection label="Observe" items={observabilityItems} />
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

interface ProjectNavItem {
  title: string
  url: string
  icon: LucideIcon
  subItems?: { title: string; url: string; icon: LucideIcon }[]
  // When true, clicking the row navigates to `url`; the chevron is the
  // only affordance that opens the drill-down submenu.
  navigateOnClick?: boolean
}

const projectBaseNav: ProjectNavItem[] = [
  { title: 'Overview', url: 'project', icon: Home },
  { title: 'Deployments', url: 'deployments', icon: GitBranch },
  { title: 'Environments', url: 'environments', icon: Layers },
  {
    title: 'Analytics',
    url: 'analytics',
    icon: BarChart3,
    navigateOnClick: true,
    subItems: [
      { title: 'Overview', url: 'analytics', icon: BarChart3 },
      { title: 'Visitors', url: 'analytics/visitors', icon: Users },
      { title: 'Pages', url: 'analytics/pages', icon: FileText },
      { title: 'AI Agents', url: 'analytics/ai-agents', icon: Bot },
      { title: 'Funnels', url: 'analytics/funnels', icon: Filter },
      { title: 'Session Replays', url: 'analytics/replays', icon: Play },
      { title: 'API Traffic', url: 'analytics/api-traffic', icon: Server },
      { title: 'Speed', url: 'speed', icon: Zap },
      { title: 'Revenue', url: 'revenue', icon: CreditCard },
    ],
  },
  { title: 'Databases', url: 'storage', icon: Database },
  {
    title: 'Environment Variables',
    url: 'environment-variables',
    icon: KeyRound,
  },
  { title: 'Feature Flags', url: 'flags', icon: Flag },
  { title: 'Domains', url: 'domains', icon: Globe },
  { title: 'Git', url: 'git', icon: GitFork },
  { title: 'Build & Deploy', url: 'build', icon: Settings2 },
  { title: 'Logs', url: 'runtime', icon: ScrollText },
  {
    title: 'OpenTelemetry',
    url: 'observe',
    icon: Radio,
    navigateOnClick: true,
    subItems: [
      { title: 'Observe', url: 'observe', icon: Eye },
      { title: 'Traces', url: 'traces', icon: Network },
      { title: 'AI Traces', url: 'ai-gateway?tab=activity', icon: Bot },
      { title: 'Metrics', url: 'metrics', icon: LineChart },
      { title: 'Logs', url: 'telemetry-logs', icon: ScrollText },
      { title: 'Error Tracking', url: 'errors', icon: ShieldAlert },
    ],
  },
  {
    title: 'Monitoring',
    url: 'monitors',
    icon: Gauge,
    navigateOnClick: true,
    subItems: [
      { title: 'Uptime', url: 'monitors', icon: Activity },
      { title: 'Request Logs', url: 'request-logs', icon: Rss },
      { title: 'AI Crawlers', url: 'ai-crawlers', icon: Bot },
    ],
  },
  { title: 'AI Workflows', url: 'agents', icon: Workflow },
  {
    title: 'Settings',
    url: 'settings',
    icon: Settings,
    subItems: [
      { title: 'General', url: 'settings/general', icon: SlidersHorizontal },
      { title: 'Secrets', url: 'settings/secrets', icon: FileLock2 },
      { title: 'Security', url: 'settings/security', icon: Shield },
      { title: 'Access', url: 'settings/access', icon: Users },
      { title: 'Cron Jobs', url: 'settings/cron-jobs', icon: Clock },
      { title: 'Webhooks', url: 'settings/webhooks', icon: Webhook },
      { title: 'Skills', url: 'settings/skills', icon: Wand2 },
      { title: 'MCP Servers', url: 'settings/mcp-servers', icon: Server },
      { title: 'Alert Rules', url: 'errors/alert-rules', icon: AlarmClock },
    ],
  },
]

function ProjectSetupNavItem({ project }: { project: ProjectResponse }) {
  const { isMinimal, isMobile } = useSidebar()
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
              <Link to={`/projects/${project.slug}/setup`}>
                <BadgeCheck />
              </Link>
            </SidebarMenuButton>
          </SidebarMenuItem>
        </SidebarMenu>
      </SidebarGroup>
    )
  }

  const visibleSteps = remainingSteps.slice(0, 2)
  const hiddenCount = remainingSteps.length - visibleSteps.length

  return (
    <SidebarGroup className="py-0 pb-2">
      <div className="overflow-hidden rounded-lg border border-sidebar-border bg-sidebar-accent/30">
        <Link
          to={`/projects/${project.slug}/setup`}
          className="group block px-3 py-2.5 transition-colors hover:bg-sidebar-accent/60 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-sidebar-ring"
        >
          <div className="flex items-center gap-2">
            <BadgeCheck className="size-4 shrink-0 text-primary" />
            <span className="flex-1 text-sm font-medium">Project setup</span>
            <span className="text-xs tabular-nums text-muted-foreground">
              {setup.completedCount}/{setup.totalCount}
            </span>
            <ChevronRight className="size-3.5 shrink-0 text-muted-foreground transition-transform group-hover:translate-x-0.5" />
          </div>
          <div
            className="mt-2 h-1 w-full overflow-hidden rounded-full bg-sidebar-border"
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
        <ul role="list" className="border-t border-sidebar-border p-1.5">
          {visibleSteps.map((step) => (
            <li key={step.id}>
              <Link
                to={step.href}
                className="group flex items-center gap-2 rounded-md px-1.5 py-2 text-xs transition-colors hover:bg-sidebar-accent focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-sidebar-ring"
              >
                <step.icon className="size-3.5 shrink-0 text-muted-foreground group-hover:text-foreground" />
                <span className="min-w-0 flex-1 leading-tight">
                  {step.title}
                </span>
                <ChevronRight className="size-3.5 shrink-0 text-muted-foreground" />
              </Link>
            </li>
          ))}
          {hiddenCount > 0 && (
            <li>
              <Link
                to={`/projects/${project.slug}/setup`}
                className="block rounded-md px-1.5 pb-1 pt-0.5 text-[11px] text-muted-foreground transition-colors hover:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-sidebar-ring"
              >
                +{hiddenCount} more {hiddenCount === 1 ? 'step' : 'steps'}
              </Link>
            </li>
          )}
        </ul>
      </div>
    </SidebarGroup>
  )
}

function ProjectNav({ slug, onBack }: { slug: string; onBack: () => void }) {
  const { data: project } = useQuery({
    ...getProjectBySlugOptions({ path: { slug } }),
  })
  const { projectNavEntries } = usePluginsContext()
  const location = useLocation()
  const { isMinimal, isMobile } = useSidebar()
  const compact = isMinimal && !isMobile
  const items = useMemo<ProjectNavItem[]>(() => {
    const settingsIdx = projectBaseNav.length - 1
    const pluginItems: ProjectNavItem[] = projectNavEntries.map((e) => ({
      title: e.label,
      url: e.path,
      icon: resolvePluginIcon(e.icon),
    }))
    return [
      ...projectBaseNav.slice(0, settingsIdx),
      ...pluginItems,
      projectBaseNav[settingsIdx],
    ]
  }, [projectNavEntries])

  const activeRoute = useMemo(() => {
    if (!project) return ''
    const parts = location.pathname.split('/')
    const slugIdx = parts.indexOf(project.slug)
    if (slugIdx === -1) return ''
    return parts.slice(slugIdx + 1).join('/')
  }, [location.pathname, project])

  // Drill-down state: null = root project nav; string = title of the
  // parent whose sub-items are showing. Initialised lazily from the
  // current route so a deep link lands inside the right sub-view, but
  // we never re-derive afterwards — Back must always return to root,
  // even though the URL is still a sub-route.
  // Match a sub-item to the current route. Prefix-aware so a deeper route
  // (e.g. `analytics/ai-agents/all`) still resolves to its section's sub-item
  // (`analytics/ai-agents`), not just an exact match.
  const matchesSubRoute = (subUrl: string, route: string) =>
    route === subUrl || route.startsWith(`${subUrl}/`)

  const findDrillParent = (route: string) =>
    projectBaseNav.find((it) =>
      it.subItems?.some((s) => matchesSubRoute(s.url, route))
    )?.title ?? null

  const [drilledTo, setDrilledTo] = useState<string | null>(() =>
    activeRoute ? findDrillParent(activeRoute) : null
  )

  // On a hard refresh the `useState` initializer above runs before `project`
  // has loaded, so `activeRoute` is empty and `drilledTo` stays null — leaving
  // the sidebar on the root nav even though the URL is a deep sub-route. Re-sync
  // exactly once, the first time `activeRoute` becomes available, so a refreshed
  // deep link (e.g. /analytics/ai-agents) expands the right section. We gate on
  // a ref so this never fires again on later route changes — that would fight
  // the Back arrow, which intentionally collapses to root while staying on the
  // sub-route URL.
  const didSyncDrillRef = useRef(false)
  useEffect(() => {
    if (didSyncDrillRef.current || !activeRoute) return
    didSyncDrillRef.current = true
    const parent = findDrillParent(activeRoute)
    // Reconcile the async project lookup with the route-derived initial view.
    // eslint-disable-next-line react-hooks/set-state-in-effect
    if (parent) setDrilledTo(parent)
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [activeRoute])

  // The single most-specific nav URL that the current route falls under. Among
  // all candidate URLs whose path the route matches (exactly or as a prefix),
  // the LONGEST one wins — so `analytics/ai-agents/all` highlights
  // `analytics/ai-agents`, not the shorter `analytics` Overview.
  // NOTE: must stay ABOVE the `if (!project)` early return — it's a hook.
  const bestMatchUrl = useMemo(() => {
    const candidates = projectBaseNav.flatMap((it) => [
      it.url,
      ...(it.subItems?.map((s) => s.url) ?? []),
    ])
    let best: string | null = null
    for (const c of candidates) {
      const pathOnly = c.split('?')[0]
      if (matchesSubRoute(pathOnly, activeRoute)) {
        if (best === null || pathOnly.length > best.length) best = pathOnly
      }
    }
    return best
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [activeRoute, projectBaseNav])

  if (!project) {
    return (
      <>
        <SwapHeader title="Loading…" onBack={onBack} backLabel="Back to menu" />
      </>
    )
  }

  const isActive = (url: string) => {
    const pathOnly = url.split('?')[0]
    if (pathOnly === 'project')
      return activeRoute === '' || activeRoute === 'project'
    if (pathOnly === 'environments')
      return activeRoute.startsWith('environments')
    return pathOnly === bestMatchUrl
  }
  const isParentActive = (item: ProjectNavItem) =>
    !!item.subItems?.some((s) => isActive(s.url))

  // Drill-down sub-view: show only the children of `drilledTo`.
  if (drilledTo) {
    const parent = items.find((it) => it.title === drilledTo)
    if (parent?.subItems?.length) {
      return (
        <>
          <SwapHeader
            title={parent.title}
            onBack={() => setDrilledTo(null)}
            backLabel={`Back to ${project.name}`}
          />
          <SidebarGroup className="pt-0">
            <SidebarMenu>
              {parent.subItems.map((sub) => {
                const active = isActive(sub.url)
                return (
                  <SidebarMenuItem key={sub.url}>
                    <SidebarMenuButton
                      asChild
                      tooltip={compact ? sub.title : undefined}
                      className={cn(
                        compact ? 'justify-center' : 'justify-start',
                        active &&
                          'bg-sidebar-accent text-sidebar-accent-foreground'
                      )}
                    >
                      <Link to={`/projects/${project.slug}/${sub.url}`}>
                        <sub.icon />
                        {!compact && <span>{sub.title}</span>}
                      </Link>
                    </SidebarMenuButton>
                  </SidebarMenuItem>
                )
              })}
            </SidebarMenu>
          </SidebarGroup>
        </>
      )
    }
  }

  return (
    <>
      <SwapHeader
        title={project.name}
        onBack={onBack}
        backLabel="Back to menu"
      />
      <ProjectSetupNavItem project={project} />
      <SidebarGroup className="pt-0">
        <SidebarMenu>
          {items.map((item) => {
            const active = isActive(item.url) || isParentActive(item)
            const hasSub = !!item.subItems?.length
            const splitRow = hasSub && item.navigateOnClick
            return (
              <SidebarMenuItem
                key={item.title}
                data-tour={item.url.split('?')[0]}
              >
                {splitRow ? (
                  <SidebarMenuButton
                    asChild
                    onClick={() => setDrilledTo(item.title)}
                    tooltip={compact ? item.title : undefined}
                    className={cn(
                      compact ? 'justify-center' : 'justify-start',
                      active &&
                        'bg-sidebar-accent text-sidebar-accent-foreground'
                    )}
                  >
                    <Link to={`/projects/${project.slug}/${item.url}`}>
                      <item.icon />
                      {!compact && (
                        <>
                          <span className="flex-1 text-left">{item.title}</span>
                          <ChevronRight className="size-4 text-muted-foreground" />
                        </>
                      )}
                    </Link>
                  </SidebarMenuButton>
                ) : hasSub ? (
                  <SidebarMenuButton
                    onClick={() => setDrilledTo(item.title)}
                    tooltip={compact ? item.title : undefined}
                    className={cn(
                      compact ? 'justify-center' : 'justify-start',
                      active &&
                        'bg-sidebar-accent text-sidebar-accent-foreground'
                    )}
                  >
                    <item.icon />
                    {!compact && (
                      <>
                        <span className="flex-1 text-left">{item.title}</span>
                        <ChevronRight className="size-4 text-muted-foreground" />
                      </>
                    )}
                  </SidebarMenuButton>
                ) : (
                  <SidebarMenuButton
                    asChild
                    tooltip={compact ? item.title : undefined}
                    className={cn(
                      compact ? 'justify-center' : 'justify-start',
                      active &&
                        'bg-sidebar-accent text-sidebar-accent-foreground'
                    )}
                  >
                    <Link to={`/projects/${project.slug}/${item.url}`}>
                      <item.icon />
                      {!compact && <span>{item.title}</span>}
                    </Link>
                  </SidebarMenuButton>
                )}
              </SidebarMenuItem>
            )
          })}
        </SidebarMenu>
      </SidebarGroup>
    </>
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
