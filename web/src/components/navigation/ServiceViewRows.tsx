// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useQuery } from '@tanstack/react-query'
import { Globe, Plus } from 'lucide-react'
import { useEffect, useRef } from 'react'
import { Link, useLocation } from 'react-router'
import {
  getProjectBySlugOptions,
  listCustomDomainsForProjectOptions,
} from '@/api/client/@tanstack/react-query.gen'
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuTrigger,
} from '@/components/ui/dropdown-menu'
import {
  SidebarMenu,
  SidebarMenuButton,
  SidebarMenuItem,
  SidebarMenuSubButton,
  SidebarMenuSubItem,
  SidebarSeparator,
} from '@/components/ui/sidebar'
import { countedState, domainState } from '@/lib/entity-tree'
import { projectRoot } from '@/lib/project-navigation'
import { scrollElementIntoView } from '@/lib/scroll-to-element'
import { cn } from '@/lib/utils'
import { UnusedViewItems, type EntityView } from './EntityViewTabs'
import { RowState } from './RowState'
import { useServiceViews, type ServiceRef } from './use-service-views'

const ACTIVE = 'bg-sidebar-accent text-sidebar-accent-foreground'

/** `page` on the view's own page, `true` on a page below it. */
function currentOf(view: EntityView, pathname: string) {
  if (!view.active) return undefined
  return view.to.split('?')[0] === pathname ? 'page' : 'true'
}

/**
 * The views of the service shown, as the rows under it in the tree:
 * Deployments, Logs, Errors, the data it has, then Settings. This is where
 * a service's views are chosen, on a phone as on a desktop; its page keeps
 * one row of tabs for the pages of the view shown.
 */
export function ServiceViewRows({
  service,
  onNavigate,
}: {
  service: ServiceRef
  onNavigate: () => void
}) {
  const { pathname } = useLocation()
  const { listed, unused } = useServiceViews(service)
  const domains = useQuery({
    ...listCustomDomainsForProjectOptions({
      path: { project_id: service.id ?? 0 },
    }),
    staleTime: 60_000,
    retry: false,
    enabled: service.id !== undefined && service.source_type !== 'external',
  })
  const domainList = domains.data?.domains ?? []
  const domainsUrl = `${projectRoot(service.slug)}/domains`
  // With its own row, the Domains page is marked there, not on the
  // Overview that holds it as a tab.
  const onDomains = domainList.length > 0 && pathname === domainsUrl
  const shown = onDomains ? 'domains' : listed.find((view) => view.active)?.key
  const list = useRef<HTMLLIElement>(null)
  // In a long tree, or a drawer just opened, the view shown may be below
  // the fold: the menu opens on where the user is.
  useEffect(() => {
    const row = list.current?.parentElement?.querySelector<HTMLElement>(
      '[data-view][aria-current]'
    )
    if (row) scrollElementIntoView(row, { block: 'nearest' })
  }, [shown])
  return (
    <>
      {listed.map((item, index) => {
        const view = onDomains ? { ...item, active: false } : item
        return (
          <SidebarMenuSubItem
            key={view.key}
            ref={index === 0 ? list : undefined}
          >
            <SidebarMenuSubButton asChild isActive={view.active}>
              <Link
                to={view.to}
                data-view={view.key}
                data-tour={view.tour}
                aria-current={currentOf(view, pathname)}
                onClick={onNavigate}
                className={cn(view.active && ACTIVE)}
              >
                <view.icon aria-hidden="true" />
                <span>{view.title}</span>
              </Link>
            </SidebarMenuSubButton>
          </SidebarMenuSubItem>
        )
      })}
      {unused.length > 0 && (
        <SidebarMenuSubItem>
          <DropdownMenu>
            <DropdownMenuTrigger asChild>
              <SidebarMenuSubButton
                asChild
                className="w-full text-muted-foreground"
              >
                <button type="button" aria-label="Add a view">
                  <Plus aria-hidden="true" />
                  <span>Add</span>
                </button>
              </SidebarMenuSubButton>
            </DropdownMenuTrigger>
            <DropdownMenuContent side="bottom" align="start" className="w-72">
              <UnusedViewItems views={unused} onNavigate={onNavigate} />
            </DropdownMenuContent>
          </DropdownMenu>
        </SidebarMenuSubItem>
      )}
      {/* Its custom domains, once it has any: one row, whatever their
          number, so each hostname is read in full on their page. A
          hostname fails on its own (DNS not pointed yet, a certificate
          pending), so the row says how many are in trouble. */}
      {domainList.length > 0 && (
        <SidebarMenuSubItem>
          <SidebarMenuSubButton asChild isActive={onDomains}>
            <Link
              to={domainsUrl}
              data-view="domains"
              aria-current={onDomains ? 'page' : undefined}
              onClick={onNavigate}
              // Its name and state are never cut: when both do not fit,
              // the state goes under the name, in the tree's column of
              // states.
              className={cn(
                'h-auto min-h-7 flex-wrap gap-y-0.5 py-1',
                onDomains && ACTIVE
              )}
            >
              <Globe aria-hidden="true" />
              <span className="shrink-0">Domains</span>
              <RowState
                state={countedState(
                  domainList.map((domain) => domainState(domain.status)),
                  'Active'
                )}
              />
            </Link>
          </SidebarMenuSubButton>
        </SidebarMenuSubItem>
      )}
    </>
  )
}

function CompactViews({ service }: { service: ServiceRef }) {
  const { pathname } = useLocation()
  const { listed, unused } = useServiceViews(service)
  return (
    <>
      <SidebarSeparator />
      <SidebarMenu aria-label={`${service.slug} views`}>
        {listed.map((view) => (
          <SidebarMenuItem key={view.key}>
            <SidebarMenuButton
              asChild
              tooltip={view.title}
              className={cn('justify-center', view.active && ACTIVE)}
            >
              <Link
                to={view.to}
                data-view={view.key}
                data-tour={view.tour}
                aria-label={view.title}
                aria-current={currentOf(view, pathname)}
              >
                <view.icon aria-hidden="true" />
              </Link>
            </SidebarMenuButton>
          </SidebarMenuItem>
        ))}
        {unused.length > 0 && (
          <SidebarMenuItem>
            <DropdownMenu>
              <DropdownMenuTrigger asChild>
                <SidebarMenuButton
                  tooltip="Add"
                  aria-label="Add a view"
                  className="justify-center text-muted-foreground"
                >
                  <Plus aria-hidden="true" />
                </SidebarMenuButton>
              </DropdownMenuTrigger>
              <DropdownMenuContent side="right" align="start" className="w-72">
                <UnusedViewItems views={unused} />
              </DropdownMenuContent>
            </DropdownMenu>
          </SidebarMenuItem>
        )}
      </SidebarMenu>
    </>
  )
}

/**
 * The same views while the sidebar is folded to icons: one icon each, named
 * by its tooltip, under the tree's branches. The service is read from the
 * request its page already made.
 */
export function CompactServiceViews({ slug }: { slug: string }) {
  const service = useQuery({
    ...getProjectBySlugOptions({ path: { slug } }),
    staleTime: 60_000,
    retry: false,
  })
  if (!service.data) return null
  return <CompactViews service={service.data} />
}
