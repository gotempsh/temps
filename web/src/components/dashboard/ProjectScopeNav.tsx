// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useQuery } from '@tanstack/react-query'
import {
  ArrowLeft,
  Check,
  ChevronsUpDown,
  FolderOpen,
  Plus,
  RefreshCw,
  Rocket,
} from 'lucide-react'
import { Link, useLocation } from 'react-router'
import type {
  ProjectGroupResponse,
  ProjectResponse,
} from '@/api/client/types.gen'
import {
  getProjectBySlugOptions,
  getProjectGroupBySlugOptions,
} from '@/api/client/@tanstack/react-query.gen'
import {
  SidebarGroup,
  SidebarGroupAction,
  SidebarGroupLabel,
  SidebarMenu,
  SidebarMenuButton,
  SidebarMenuItem,
  SidebarMenuSkeleton,
  SidebarMenuSub,
  SidebarMenuSubButton,
  SidebarMenuSubItem,
  useSidebar,
} from '@/components/ui/sidebar'
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuLabel,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from '@/components/ui/dropdown-menu'
import { ServiceIcon } from '@/components/project/ServiceIcon'
import { useServiceGroup } from '@/components/project-groups/ServiceGroupControl'
import { useProjectGroups } from '@/components/project-groups/useProjectGroups'
import { newServiceInGroupHref } from '@/components/project-groups/useJoinGroupAfterCreate'
import {
  GROUP_NAV_GROUPS,
  groupSectionHref,
  resolveGroupSection,
  type GroupSectionId,
} from '@/lib/console-scope'
import {
  projectNavGroupsFor,
  projectRoot,
  resolveProjectLocation,
  type ProjectNavSection,
} from '@/lib/project-navigation'
import { ENVIRONMENT_PARAM } from '@/lib/project-environment-selection'
import { groupHref } from '@/lib/terminology'
import { cn } from '@/lib/utils'

const ACTIVE = 'bg-sidebar-accent text-sidebar-accent-foreground'

function useCompact() {
  const { isMinimal, isMobile, setOpenMobile } = useSidebar()
  return {
    compact: isMinimal && !isMobile,
    closeOnMobile: () => isMobile && setOpenMobile(false),
  }
}

/** Keeps the environment chosen in the service bar when changing section. */
function useEnvironmentQuery(): string {
  const { search } = useLocation()
  const env = new URLSearchParams(search).get(ENVIRONMENT_PARAM)
  return env ? `?${ENVIRONMENT_PARAM}=${encodeURIComponent(env)}` : ''
}

/**
 * Top of a project's sidebar: the way out to every project, and a switcher
 * to another project without going through the list.
 */
export function ProjectScopeHeader({
  title,
  currentGroupSlug,
}: {
  title: string
  currentGroupSlug?: string
}) {
  const { compact, closeOnMobile } = useCompact()
  const { groups, isPending } = useProjectGroups()
  if (compact) {
    return (
      <SidebarGroup className="pb-0">
        <SidebarMenu>
          <SidebarMenuItem>
            <SidebarMenuButton
              asChild
              tooltip="All projects"
              className="justify-center text-muted-foreground hover:text-foreground"
            >
              <Link to="/projects" aria-label="All projects">
                <ArrowLeft />
              </Link>
            </SidebarMenuButton>
          </SidebarMenuItem>
        </SidebarMenu>
      </SidebarGroup>
    )
  }
  return (
    <SidebarGroup className="gap-1 pb-0">
      <Link
        to="/projects"
        onClick={closeOnMobile}
        className="flex h-7 items-center gap-2 rounded-md px-2 text-xs text-muted-foreground transition-colors hover:bg-sidebar-accent hover:text-foreground"
      >
        <ArrowLeft className="size-3.5" />
        All projects
      </Link>
      <DropdownMenu>
        <DropdownMenuTrigger asChild>
          <button
            type="button"
            aria-label={`${title}: switch project`}
            className="flex h-9 w-full items-center gap-2 rounded-md border border-sidebar-border px-2 text-left text-sm transition-colors hover:bg-sidebar-accent"
          >
            <FolderOpen className="size-4 shrink-0 text-muted-foreground" />
            <span className="min-w-0 flex-1 truncate font-medium">{title}</span>
            <ChevronsUpDown className="size-4 shrink-0 text-muted-foreground" />
          </button>
        </DropdownMenuTrigger>
        <DropdownMenuContent
          align="start"
          className="max-h-80 w-(--radix-dropdown-menu-trigger-width) min-w-56 overflow-y-auto"
        >
          <DropdownMenuLabel className="text-xs text-muted-foreground">
            Projects
          </DropdownMenuLabel>
          {isPending ? (
            <DropdownMenuItem disabled>Loading projects…</DropdownMenuItem>
          ) : groups.length === 0 ? (
            <DropdownMenuItem disabled>No other projects</DropdownMenuItem>
          ) : (
            groups.map((group) => (
              <DropdownMenuItem key={group.id} asChild>
                <Link
                  to={groupHref(group.slug)}
                  onClick={closeOnMobile}
                  aria-current={
                    group.slug === currentGroupSlug ? 'page' : undefined
                  }
                >
                  <span className="min-w-0 flex-1 truncate">{group.name}</span>
                  {group.slug === currentGroupSlug && (
                    <Check className="size-4" aria-label="Current project" />
                  )}
                </Link>
              </DropdownMenuItem>
            ))
          )}
          <DropdownMenuSeparator />
          <DropdownMenuItem asChild>
            <Link to="/projects" onClick={closeOnMobile}>
              All projects
            </Link>
          </DropdownMenuItem>
        </DropdownMenuContent>
      </DropdownMenu>
    </SidebarGroup>
  )
}

/** A service's sections, nested under it in its project's menu. */
function ServiceSections({
  service,
  route,
}: {
  service: ProjectResponse
  route: string
}) {
  const { compact, closeOnMobile } = useCompact()
  const query = useEnvironmentQuery()
  const root = projectRoot(service.slug)
  const onHosting = route === 'hosting' || route.startsWith('hosting/')
  const active = onHosting ? null : resolveProjectLocation(route).section.id
  const sections: ProjectNavSection[] = projectNavGroupsFor(service).flatMap(
    (group) => group.sections
  )
  const items = sections.map((section) => ({
    key: section.id,
    title: section.title,
    icon: section.icon,
    url: `${root}/${section.tabs[0].url}${query}`,
    active: active === section.id,
  }))
  if (service.source_type === 'external') {
    items.push({
      key: 'overview',
      title: 'Add hosting',
      icon: Rocket,
      url: `${root}/hosting`,
      active: onHosting,
    })
  }
  if (compact) {
    // Collapsed, a nested list would be hidden: show the sections as icons.
    return (
      <>
        {items.map((item) => (
          <SidebarMenuItem key={`${item.key}-${item.title}`}>
            <SidebarMenuButton
              asChild
              tooltip={`${service.name}: ${item.title}`}
              className={cn('justify-center', item.active && ACTIVE)}
            >
              <Link
                to={item.url}
                aria-current={item.active ? 'page' : undefined}
              >
                <item.icon />
              </Link>
            </SidebarMenuButton>
          </SidebarMenuItem>
        ))}
      </>
    )
  }
  return (
    <SidebarMenuSub aria-label={`${service.name} sections`}>
      {items.map((item) => (
        <SidebarMenuSubItem
          key={`${item.key}-${item.title}`}
          data-tour={item.key}
        >
          <SidebarMenuSubButton asChild isActive={item.active}>
            <Link
              to={item.url}
              aria-current={item.active ? 'page' : undefined}
              onClick={closeOnMobile}
              className={cn(item.active && ACTIVE)}
            >
              <item.icon />
              <span>{item.title}</span>
            </Link>
          </SidebarMenuSubButton>
        </SidebarMenuSubItem>
      ))}
    </SidebarMenuSub>
  )
}

/**
 * A project's own sidebar: Overview, its services (the open one expanded to
 * its sections), then what is observed, owned and configured at project
 * level. The same sidebar stays while moving into and between its services.
 */
export function GroupNav({
  group,
  activeSection,
  activeService,
  serviceRoute = '',
}: {
  group: ProjectGroupResponse
  /** The project-level page shown, if any. */
  activeSection?: GroupSectionId
  /** The service open inside the project, if any. */
  activeService?: ProjectResponse
  serviceRoute?: string
}) {
  const { compact, closeOnMobile } = useCompact()
  const [overviewGroup, ...sectionGroups] = GROUP_NAV_GROUPS
  const renderSection = (section: (typeof overviewGroup.sections)[number]) => {
    const isActive = !activeService && activeSection === section.id
    return (
      <SidebarMenuItem key={section.id}>
        <SidebarMenuButton
          asChild
          tooltip={compact ? section.title : undefined}
          className={cn(
            compact ? 'justify-center' : 'justify-start',
            isActive && ACTIVE
          )}
        >
          <Link
            to={groupSectionHref(group.slug, section)}
            aria-current={isActive ? 'page' : undefined}
            onClick={closeOnMobile}
          >
            <section.icon />
            {!compact && <span>{section.title}</span>}
          </Link>
        </SidebarMenuButton>
      </SidebarMenuItem>
    )
  }
  return (
    <>
      <ProjectScopeHeader title={group.name} currentGroupSlug={group.slug} />
      <SidebarGroup className="py-2">
        <SidebarMenu aria-label="Project">
          {overviewGroup.sections.map(renderSection)}
        </SidebarMenu>
      </SidebarGroup>
      <SidebarGroup className="py-0">
        <SidebarGroupLabel className={compact ? 'hidden' : ''}>
          Services
        </SidebarGroupLabel>
        {!compact && (
          <SidebarGroupAction asChild title="Add a service">
            <Link
              to={newServiceInGroupHref(group.id)}
              aria-label={`Add a service to ${group.name}`}
            >
              <Plus />
            </Link>
          </SidebarGroupAction>
        )}
        <SidebarMenu aria-label="Services">
          {group.projects.map((member) => {
            const open = activeService?.id === member.id
            return (
              <SidebarMenuItem key={member.id}>
                <SidebarMenuButton
                  asChild
                  tooltip={compact ? member.name : undefined}
                  className={cn(
                    compact ? 'justify-center' : 'justify-start',
                    open && !compact && 'font-medium'
                  )}
                >
                  <Link
                    to={projectRoot(member.slug)}
                    aria-current={open && !serviceRoute ? 'page' : undefined}
                    aria-expanded={open}
                    onClick={closeOnMobile}
                  >
                    <ServiceIcon service={member} size="sm" />
                    {!compact && <span>{member.name}</span>}
                  </Link>
                </SidebarMenuButton>
                {open && activeService && !compact && (
                  <ServiceSections
                    service={activeService}
                    route={serviceRoute}
                  />
                )}
              </SidebarMenuItem>
            )
          })}
          {compact && activeService && (
            <ServiceSections service={activeService} route={serviceRoute} />
          )}
          {group.projects.length === 0 && !compact && (
            <SidebarMenuItem>
              <SidebarMenuButton asChild className="text-muted-foreground">
                <Link to={newServiceInGroupHref(group.id)}>
                  <Plus />
                  <span>Add a service</span>
                </Link>
              </SidebarMenuButton>
            </SidebarMenuItem>
          )}
        </SidebarMenu>
      </SidebarGroup>
      {sectionGroups.map((navGroup) => (
        <SidebarGroup key={navGroup.label} className="py-0">
          <SidebarGroupLabel className={compact ? 'hidden' : ''}>
            {navGroup.label}
          </SidebarGroupLabel>
          <SidebarMenu aria-label={navGroup.label}>
            {navGroup.sections.map(renderSection)}
          </SidebarMenu>
        </SidebarGroup>
      ))}
    </>
  )
}

function NavLoading() {
  return (
    <SidebarGroup>
      <SidebarMenu aria-busy="true" aria-label="Loading navigation">
        {Array.from({ length: 6 }, (_, index) => (
          <SidebarMenuItem key={index}>
            <SidebarMenuSkeleton showIcon />
          </SidebarMenuItem>
        ))}
      </SidebarMenu>
    </SidebarGroup>
  )
}

/** Sidebar for `/p/:slug/*`. */
export function ProjectGroupScopeNav({
  slug,
  route,
}: {
  slug: string
  route: string
}) {
  const { data: group, isError } = useQuery({
    ...getProjectGroupBySlugOptions({ path: { slug } }),
    retry: false,
  })
  if (group) {
    return <GroupNav group={group} activeSection={resolveGroupSection(route)} />
  }
  return (
    <>
      <ProjectScopeHeader title={isError ? slug : 'Loading…'} />
      {!isError && <NavLoading />}
    </>
  )
}

/**
 * Sidebar for `/projects/:slug/*`: its project's sidebar with the service
 * open inside it, or, for a standalone service, the service's own menu.
 */
export function ServiceScopeNav({
  slug,
  route,
  standalone,
}: {
  slug: string
  route: string
  /**
   * The service's own menu: for a service that belongs to no project, or,
   * with a `notice`, when its project could not be looked up.
   */
  standalone: (
    service: ProjectResponse | undefined,
    notice?: React.ReactNode
  ) => React.ReactNode
}) {
  const project = useQuery(getProjectBySlugOptions({ path: { slug } }))
  const membership = useServiceGroup(project.data?.id)
  const group = membership.data?.group
  if (project.data && group) {
    return (
      <GroupNav
        group={group}
        activeService={project.data}
        serviceRoute={route}
      />
    )
  }
  // Until the membership is known, a skeleton rather than the standalone
  // menu, so a grouped service does not flash a different sidebar.
  if (project.isPending || (project.data && membership.isPending)) {
    return (
      <>
        <ProjectScopeHeader title={project.data?.name ?? 'Loading…'} />
        <NavLoading />
      </>
    )
  }
  // A failed lookup is not "no project": say so and offer a retry, keeping
  // the service's own pages reachable meanwhile.
  if (project.data && membership.isError) {
    return (
      <>
        {standalone(
          project.data,
          <MembershipError
            serviceName={project.data.name}
            onRetry={() => void membership.refetch()}
          />
        )}
      </>
    )
  }
  return <>{standalone(project.data)}</>
}

function MembershipError({
  serviceName,
  onRetry,
}: {
  serviceName: string
  onRetry: () => void
}) {
  const { compact } = useCompact()
  if (compact) {
    return (
      <SidebarGroup>
        <SidebarMenu>
          <SidebarMenuItem>
            <SidebarMenuButton
              tooltip={`Could not load ${serviceName}'s project. Retry`}
              className="justify-center text-destructive"
              onClick={onRetry}
            >
              <RefreshCw />
            </SidebarMenuButton>
          </SidebarMenuItem>
        </SidebarMenu>
      </SidebarGroup>
    )
  }
  return (
    <SidebarGroup>
      <div role="alert" className="space-y-2 rounded-md border p-3 text-xs">
        <p className="text-muted-foreground">
          Could not load the project {serviceName} belongs to, so its navigation
          is unavailable.
        </p>
        <button
          type="button"
          onClick={onRetry}
          className="inline-flex items-center gap-1.5 rounded-md border px-2 py-1 font-medium transition-colors hover:bg-sidebar-accent"
        >
          <RefreshCw className="size-3.5" />
          Retry
        </button>
      </div>
    </SidebarGroup>
  )
}
