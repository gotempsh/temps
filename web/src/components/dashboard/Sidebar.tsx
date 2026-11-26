// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

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
  ArrowLeft,
  BadgeCheck,
  ChevronsUpDown,
  Check,
  LayoutGrid,
  LogOut,
  Monitor,
  Moon,
  Search,
  Settings,
  Sun,
} from 'lucide-react'

import { useAuth } from '@/contexts/AuthContext'
import { useGettingStarted } from '@/hooks/useGettingStarted'
import { usePluginsContext } from '@/contexts/PluginsContext'
import { resolvePluginIcon } from '@/lib/pluginIcons'
import { SIDEBAR_HELP_LINKS } from '@/lib/support'
import {
  SETTINGS_OVERVIEW_URL,
  isSettingsSidebarRoute,
  setupMenuEntry,
} from './primary-navigation'
import { AskEntry } from '@/components/navigation/AskEntry'
import { EntityTree } from '@/components/navigation/EntityTree'
import { cn } from '@/lib/utils'
import { type LucideIcon } from 'lucide-react'
import { useEffect, useMemo, useState } from 'react'
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
import {
  resolveSettingsTab,
  type SettingsNavigationIcon,
} from '@/components/settings/settings-navigation'
import { useSettingsGroups } from '@/components/settings/use-settings-groups'

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
        Installed plugins
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
  const { isMinimal, isMobile, setOpenMobile } = useSidebar()
  const { platformNavEntries } = usePluginsContext()
  const location = useLocation()

  // The phone sidebar is a sheet over the page. Only a few links close it
  // themselves, so without this a tap on any other destination navigates
  // underneath a still-open sheet and the new page looks like it didn't load.
  useEffect(() => {
    setOpenMobile(false)
  }, [location.pathname, setOpenMobile])
  const { logoBadge, logoText, logoIcon } = useConsoleExtensions()

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

  // One menu everywhere: the tree of what the user owns, which opens to the
  // project, service, database, domain or server shown. Only Settings swaps
  // it for its own list of pages (back → the tree).
  const settingsMode = isSettingsSidebarRoute(location.pathname)

  // Override: user pressed Back from the settings swap; show DefaultNav
  // only for that exact navigation. Keyed on `location.key` rather than
  // pathname — a Link back to the very same URL (clicking "Settings" again
  // right after backing out of it) still produces a new history entry with
  // a new key, so the override correctly drops and the swap re-triggers.
  const [forceDefaultKey, setForceDefaultKey] = useState<string | null>(null)
  const forceDefault = forceDefaultKey === location.key

  const compact = isMinimal && !isMobile

  const showDefault = forceDefault || !settingsMode

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
                {logoIcon ?? (
                  <img
                    src="/svg/temps-icon.svg"
                    alt="logo"
                    className="size-full"
                  />
                )}
              </div>
              {!compact && (
                <div className="grid flex-1 text-left text-sm leading-tight">
                  <span className="flex items-center gap-1.5 truncate font-semibold">
                    {logoText ?? 'Temps'}
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
        <AskEntry />
        {showDefault ? (
          <DefaultNav pluginItems={pluginItems} />
        ) : (
          <SettingsNav onBack={() => setForceDefaultKey(location.key)} />
        )}
      </SidebarContent>
      <SidebarFooter>
        <SidebarMenu>
          {SIDEBAR_HELP_LINKS.map((link) => (
            <SidebarMenuItem key={link.id}>
              <SidebarMenuButton
                asChild
                tooltip={link.label}
                className={compact ? 'justify-center' : 'justify-start'}
              >
                <a
                  href={link.href}
                  target="_blank"
                  rel="noopener noreferrer"
                  aria-label={`${link.label} (opens in a new tab)`}
                >
                  <link.icon aria-hidden="true" />
                  {!compact && <span>{link.label}</span>}
                </a>
              </SidebarMenuButton>
            </SidebarMenuItem>
          ))}
        </SidebarMenu>
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
  /** Absent for entries that stand above the groups. */
  label?: string
  items: {
    id?: string
    title: string
    url: string
    icon: SettingsNavigationIcon
    activeWhen?: (pathname: string) => boolean
    featureKey?: string
    /** A short count beside the title, such as steps done. */
    detail?: string
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
      {label && (
        <SidebarGroupLabel className={compact ? 'hidden' : ''}>
          {label}
        </SidebarGroupLabel>
      )}
      <SidebarMenu>
        {items.map((item) => {
          const isActive =
            item.activeWhen?.(location.pathname) ?? item.url === activeUrl
          return (
            <SidebarMenuItem key={item.id ?? item.url}>
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
                  {!compact && item.detail && (
                    <span className="ml-auto text-xs tabular-nums text-muted-foreground">
                      {item.detail}
                    </span>
                  )}
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

function DefaultNav({ pluginItems }: NavProps) {
  const { isMinimal, isMobile } = useSidebar()
  const compact = isMinimal && !isMobile
  const location = useLocation()
  const { navItems: extraNavItems } = useConsoleExtensions()

  return (
    <>
      <EntityTree />
      <NavPlugins items={pluginItems} />
      <ExtensionNav items={extraNavItems} />
      <SidebarGroup className="mt-auto">
        <SidebarMenu>
          <SidebarMenuItem>
            <SidebarMenuButton
              asChild
              tooltip={compact ? 'Settings' : undefined}
              className={cn(
                compact ? 'justify-center' : 'justify-start',
                isSettingsSidebarRoute(location.pathname) &&
                  'bg-sidebar-accent text-sidebar-accent-foreground'
              )}
            >
              <Link to={SETTINGS_OVERVIEW_URL}>
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

const SETTINGS_OVERVIEW_ITEM = {
  title: 'Overview',
  url: SETTINGS_OVERVIEW_URL,
  icon: LayoutGrid,
}

function SettingsNav({ onBack }: { onBack: () => void }) {
  // The built-in groups, the links extensions add (an identity-provider
  // page from a console extension), and the audit log only for the roles
  // that can read it.
  const groups = useSettingsGroups()
  const { pathname } = useLocation()
  // Every url across every settings group, tabs included. Each section gets
  // the list minus its own items so active-state resolution sees the full
  // tree (prevents `/settings` lighting up on `/settings/keys`).
  const allSettingsUrls = [
    SETTINGS_OVERVIEW_URL,
    ...groups.flatMap((g) =>
      g.items.flatMap((i) => [i.url, ...(i.tabs ?? []).map((t) => t.url)])
    ),
  ]
  const tabbed = resolveSettingsTab(pathname, groups)?.item
  // What is left to set up is listed here until it is done or dismissed.
  const setup = setupMenuEntry(useGettingStarted())
  return (
    <>
      <SwapHeader title="Settings" onBack={onBack} backLabel="Back to menu" />
      <NavSection
        items={
          setup
            ? [
                SETTINGS_OVERVIEW_ITEM,
                {
                  ...setup,
                  icon: BadgeCheck,
                  activeWhen: (path) => path === setup.url,
                },
              ]
            : [SETTINGS_OVERVIEW_ITEM]
        }
        siblingUrls={allSettingsUrls}
      />
      {groups.map((group) => {
        const ownUrls = new Set(group.items.map((i) => i.url))
        const siblings = allSettingsUrls.filter((u) => !ownUrls.has(u))
        return (
          <NavSection
            key={group.label}
            label={group.label}
            // An entry with several pages is the one shown on any of them.
            items={group.items.map((item) => {
              const { ownsPath } = item
              if (ownsPath)
                return { ...item, activeWhen: () => ownsPath(pathname) }
              return item.tabs
                ? { ...item, activeWhen: () => item === tabbed }
                : item
            })}
            siblingUrls={siblings}
          />
        )
      })}
    </>
  )
}

// ─────────────────────────────────────────────────────────────────────────────
// AI nav — replaces the whole sidebar for the AI area (Providers, Usage,
// Limits, Activity). Back button returns to root.
// ─────────────────────────────────────────────────────────────────────────────

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
