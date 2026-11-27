// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useQuery } from '@tanstack/react-query'
import {
  Box,
  ChevronRight,
  Database,
  Folder,
  FolderOpen,
  Plus,
} from 'lucide-react'
import type { ReactNode } from 'react'
import { Link, useLocation } from 'react-router'
import {
  adminListNodesOptions,
  getProjectsMonitorHealthOptions,
  getProjectBySlugOptions,
  getProjectsOptions,
} from '@/api/client/@tanstack/react-query.gen'
import { ServiceIcon } from '@/components/project/ServiceIcon'
import { useProjectGroups } from '@/components/project-groups/useProjectGroups'
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from '@/components/ui/dropdown-menu'
import { ServiceLogo } from '@/components/ui/service-logo'
import {
  SidebarGroup,
  SidebarMenu,
  SidebarMenuAction,
  SidebarMenuButton,
  SidebarMenuItem,
  SidebarMenuSkeleton,
  SidebarMenuSub,
  SidebarMenuSubButton,
  SidebarMenuSubItem,
  useSidebar,
} from '@/components/ui/sidebar'
import {
  SERVER_BRANCHES,
  capRows,
  databaseState,
  locateInTree,
  pickProjects,
  projectNodes,
  withShownService,
  countedState,
  serverState,
  serviceState,
  worstState,
  type ProjectNode,
  type ServiceNode,
  type TreeLocation,
  type TreeState,
} from '@/lib/entity-tree'
import { ROUTE_FAILURE_IGNORE } from '@/lib/route-query-failures'
import { serverDatabasesQuery } from '@/lib/server-databases'
import { DEFAULT_PROJECT_PAGE_SIZE } from '@/lib/project-list-pagination'
import { projectRoot } from '@/lib/project-navigation'
import { SERVER_HOME, SERVER_PAGE } from '@/lib/server-navigation'
import { groupHref } from '@/lib/terminology'
import { cn } from '@/lib/utils'
import { RowState } from './RowState'
import { CompactServiceViews, ServiceViewRows } from './ServiceViewRows'
import { useRecentProjects } from './use-recent-projects'
import { useTreeDatabases, type ProjectDatabases } from './use-tree-databases'
import { useTreeFold } from './use-tree-fold'

const ACTIVE = 'bg-sidebar-accent text-sidebar-accent-foreground'
const QUERY = { staleTime: 60_000, retry: false } as const
/**
 * The tree's own reads. Each failure is shown in its row (with a retry), and
 * a role may be refused some (a Reader cannot list the nodes): none says
 * anything about the page, so none raises the page's failure banner.
 */
const TREE_QUERY = {
  ...QUERY,
  meta: { routeFailure: ROUTE_FAILURE_IGNORE },
} as const
/**
 * Projects listed on a large server; the Projects page, with search, has
 * the rest. Few enough that the tree never grows with the server, and as
 * many as the Projects page's first page, so the two list the same rows.
 */
const PROJECT_ROWS = DEFAULT_PROJECT_PAGE_SIZE
/** Services listed under a project; its own page has the rest. */
const SERVICE_ROWS = 20
/** Services a monitor answer is asked for in one request. */
const HEALTH_IDS = 100
/** With this few projects, each opens on its own: the whole tree fits. */
const OPEN_ALL_BELOW = 6

function Fold({
  name,
  open,
  onToggle,
  nested = false,
}: {
  name: string
  open: boolean
  onToggle: (open: boolean) => void
  nested?: boolean
}) {
  return (
    <SidebarMenuAction
      aria-expanded={open}
      aria-label={open ? `Hide ${name}` : `Show ${name}`}
      title={open ? 'Hide' : 'Show'}
      onClick={() => onToggle(!open)}
      className={cn(nested && 'top-1')}
    >
      <ChevronRight
        className={cn('transition-transform', open && 'rotate-90')}
      />
    </SidebarMenuAction>
  )
}

/** A row that stands for the rest of a list, or for one that is empty. */
function NoteRow({
  to,
  onNavigate,
  children,
}: {
  to: string
  onNavigate: () => void
  children: ReactNode
}) {
  return (
    <SidebarMenuSubItem>
      <SidebarMenuSubButton asChild className="text-muted-foreground">
        <Link to={to} onClick={onNavigate}>
          {children}
        </Link>
      </SidebarMenuSubButton>
    </SidebarMenuSubItem>
  )
}

interface Leaf {
  key: string
  name: string
  url: string
  icon: ReactNode
  state: TreeState | undefined
  /** The page shown is this entity's, or one below it. */
  inside: boolean
  /** The service whose views are listed under the row while it is shown. */
  views?: ServiceNode
}

function LeafRow({
  leaf,
  pathname,
  onNavigate,
}: {
  leaf: Leaf
  pathname: string
  onNavigate: () => void
}) {
  // With its views listed, the view shown carries the mark; the service
  // only reads as the one opened.
  const opened = leaf.inside && leaf.views !== undefined
  return (
    <SidebarMenuSubItem>
      <SidebarMenuSubButton asChild isActive={leaf.inside && !opened}>
        <Link
          to={leaf.url}
          aria-current={
            pathname === leaf.url ? 'page' : leaf.inside ? 'true' : undefined
          }
          onClick={onNavigate}
          className={cn(
            leaf.inside && !opened && ACTIVE,
            opened && 'font-medium text-sidebar-accent-foreground'
          )}
        >
          {leaf.icon}
          <span className="min-w-0 flex-1 truncate">{leaf.name}</span>
          <RowState state={leaf.state} />
        </Link>
      </SidebarMenuSubButton>
      {opened && leaf.views && (
        <SidebarMenuSub
          aria-label={`${leaf.name} views`}
          className="ml-2 mr-0 pl-2 pr-0"
        >
          <ServiceViewRows service={leaf.views} onNavigate={onNavigate} />
        </SidebarMenuSub>
      )}
    </SidebarMenuSubItem>
  )
}

/** A list that could not be loaded: said, with a retry. */
function RetryRow({
  label,
  onRetry,
}: {
  label: string
  onRetry: () => unknown
}) {
  return (
    <SidebarMenuSubItem className="flex h-7 items-center gap-2 px-2 text-xs text-muted-foreground">
      <span className="min-w-0 flex-1 truncate">{label}</span>
      <button
        type="button"
        onClick={() => void onRetry()}
        className="shrink-0 rounded-sm font-medium text-foreground underline-offset-2 hover:underline focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-sidebar-ring"
      >
        Retry
      </button>
    </SidebarMenuSubItem>
  )
}

/** "Add": what a project can hold, a service or a database. */
function AddToProject({
  node,
  onNavigate,
}: {
  node: Extract<ProjectNode, { kind: 'project' }>
  onNavigate: () => void
}) {
  return (
    <SidebarMenuSubItem>
      <DropdownMenu>
        <DropdownMenuTrigger asChild>
          <SidebarMenuSubButton
            asChild
            className="w-full text-muted-foreground"
          >
            <button
              type="button"
              aria-label={`Add to ${node.name}`}
              title="Add to this project"
            >
              <Plus aria-hidden="true" />
              <span>Add</span>
            </button>
          </SidebarMenuSubButton>
        </DropdownMenuTrigger>
        <DropdownMenuContent side="bottom" align="start" className="w-48">
          <DropdownMenuItem asChild>
            <Link
              to={`/projects/new?group=${node.id}`}
              onClick={onNavigate}
              className="gap-2"
            >
              <Box
                className="size-4 shrink-0 text-muted-foreground"
                aria-hidden="true"
              />
              Service
            </Link>
          </DropdownMenuItem>
          <DropdownMenuItem asChild>
            <Link
              to={`${groupHref(node.slug)}/databases`}
              onClick={onNavigate}
              className="gap-2"
            >
              <Database
                className="size-4 shrink-0 text-muted-foreground"
                aria-hidden="true"
              />
              Database
            </Link>
          </DropdownMenuItem>
        </DropdownMenuContent>
      </DropdownMenu>
    </SidebarMenuSubItem>
  )
}

const isCurrentNode = (node: ProjectNode, location: TreeLocation) =>
  node.kind === 'project'
    ? node.slug === location.groupSlug ||
      node.services.some((service) => service.slug === location.serviceSlug)
    : node.service.slug === location.serviceSlug

/**
 * The console's one menu: a tree of what the user owns. Projects open to
 * their services and the databases those use, and the service shown to its
 * views (Deployments, Logs, Settings). Below them, one Server row stands
 * for every record of the server. A row says how its entity is doing, so
 * the tree answers "is it working" before anything is opened. A folded
 * project asks the server nothing.
 */
export function EntityTree() {
  const { pathname } = useLocation()
  const { isMinimal, isMobile, setOpenMobile } = useSidebar()
  const compact = isMinimal && !isMobile
  const closeOnMobile = () => {
    if (isMobile) setOpenMobile(false)
  }
  const location = locateInTree(pathname)
  const fold = useTreeFold()
  // Projects is what most visits are for, so it starts open.
  const projectsOpen = !compact && fold.isOpen('tree:projects', true)

  const groupList = useProjectGroups()
  const services = useQuery({
    ...getProjectsOptions({ query: { per_page: 100, page: 1 } }),
    ...TREE_QUERY,
    enabled: projectsOpen,
  })

  const listed = projectNodes(groupList.groups, services.data?.projects ?? [])
  // The service shown may be past the page read: it is read on its own (the
  // request its page already made), so the tree still holds its views.
  const shownUnlisted =
    location.serviceSlug !== undefined &&
    services.isSuccess &&
    !listed.some((node) =>
      node.kind === 'project'
        ? node.services.some((s) => s.slug === location.serviceSlug)
        : node.service.slug === location.serviceSlug
    )
  const shownService = useQuery({
    ...getProjectBySlugOptions({ path: { slug: location.serviceSlug ?? '' } }),
    ...QUERY,
    enabled: projectsOpen && shownUnlisted,
  })
  const nodes = withShownService(
    listed,
    shownUnlisted ? shownService.data : undefined
  )
  const current = nodes.find((node) => isCurrentNode(node, location))
  const recent = useRecentProjects(current?.key)
  const openAll = nodes.length < OPEN_ALL_BELOW
  const isProjectOpen = (node: ProjectNode) =>
    node.kind === 'project' &&
    fold.isOpen(`tree:${node.key}`, isCurrentNode(node, location) || openAll)

  // Health is asked for every project, not only those listed: on a large
  // server it decides which are.
  const healthIds = nodes
    .flatMap((node) =>
      node.kind === 'project'
        ? node.services.map((service) => service.id)
        : [node.service.id]
    )
    .slice(0, HEALTH_IDS)
  const health = useQuery({
    ...getProjectsMonitorHealthOptions({
      query: { project_ids: healthIds.join(',') },
    }),
    ...TREE_QUERY,
    refetchInterval: 60_000,
    enabled: projectsOpen && healthIds.length > 0,
  })
  const stateOf = (service: ServiceNode) => {
    const reported = health.data?.projects[String(service.id)]
    return serviceState(reported?.status, reported?.runtime_status)
  }
  const nodeState = (node: ProjectNode) =>
    node.kind === 'project'
      ? worstState(node.services.map(stateOf))
      : stateOf(node.service)
  const projects = pickProjects(nodes, PROJECT_ROWS, {
    isCurrent: (node) => node === current,
    inTrouble: (node) => {
      const tone = nodeState(node)?.tone
      return tone === 'bad' || tone === 'warn'
    },
    recent,
  })
  // Services in no project past the first page of them, and projects past
  // the groups' first page, are not counted: the count is then left out.
  const exactTotal =
    !groupList.truncated &&
    (services.data?.total ?? 0) <= (services.data?.projects.length ?? 0)
  const unlisted = projects.more > 0 || !exactTotal

  const databases = useTreeDatabases(
    projectsOpen
      ? projects.shown.flatMap((node) =>
          node.kind === 'project' && isProjectOpen(node) ? [node] : []
        )
      : []
  )
  // A database an open project lists is marked there; any other is the
  // server's, and the Server row stands for it.
  const listsCurrentDatabase =
    location.branch === 'databases' &&
    [...databases.values()].some((entry) =>
      entry.databases.some((database) => database.id === location.recordId)
    )

  // The Server row says how the machines and databases are doing, so
  // trouble shows without opening it.
  const serverNodes = useQuery({
    ...adminListNodesOptions(),
    ...TREE_QUERY,
    enabled: !compact,
  })
  const serverDatabases = useQuery({
    ...serverDatabasesQuery(),
    ...TREE_QUERY,
    enabled: !compact,
  })
  const onServer =
    location.branch !== undefined &&
    SERVER_BRANCHES.includes(location.branch) &&
    !listsCurrentDatabase
  const serverExact = pathname === SERVER_PAGE.url

  if (compact)
    return (
      <SidebarGroup className="py-2">
        <SidebarMenu aria-label="Your server">
          {[
            {
              ...SERVER_HOME,
              marked: location.branch === 'projects',
            },
            { ...SERVER_PAGE, marked: onServer },
          ].map((row) => (
            <SidebarMenuItem key={row.url}>
              <SidebarMenuButton
                asChild
                tooltip={row.title}
                className={cn('justify-center', row.marked && ACTIVE)}
              >
                <Link
                  to={row.url}
                  aria-label={row.title}
                  aria-current={pathname === row.url ? 'page' : undefined}
                >
                  <row.icon />
                </Link>
              </SidebarMenuButton>
            </SidebarMenuItem>
          ))}
        </SidebarMenu>
        {location.serviceSlug && (
          <CompactServiceViews slug={location.serviceSlug} />
        )}
      </SidebarGroup>
    )

  const serviceLeaf = (service: ServiceNode): Leaf => ({
    key: `service:${service.id}`,
    name: service.name,
    url: projectRoot(service.slug),
    icon: <ServiceIcon service={service} size="xxs" />,
    state: stateOf(service),
    inside: service.slug === location.serviceSlug,
    views: service,
  })

  const projectsExact = pathname === SERVER_HOME.url
  // Folded, the branch itself marks where the user is.
  const projectsMarked =
    projectsExact || (location.branch === 'projects' && !projectsOpen)

  return (
    <SidebarGroup className="py-2">
      <SidebarMenu aria-label="Your server">
        <SidebarMenuItem data-branch="projects">
          <SidebarMenuButton
            asChild
            className={cn('justify-start pr-8', projectsMarked && ACTIVE)}
          >
            <Link
              to={SERVER_HOME.url}
              aria-current={projectsExact ? 'page' : undefined}
              onClick={closeOnMobile}
            >
              <SERVER_HOME.icon />
              <span>{SERVER_HOME.title}</span>
            </Link>
          </SidebarMenuButton>
          <Fold
            name={SERVER_HOME.title}
            open={projectsOpen}
            onToggle={(next) => fold.setOpen('tree:projects', next)}
          />
          {projectsOpen && (
            <SidebarMenuSub aria-label={SERVER_HOME.title}>
              <ProjectRows
                query={
                  // Projects and the services in none are two lists; both
                  // must answer before the branch can say it is empty.
                  groupList.isError || groupList.isPending
                    ? groupList
                    : services
                }
                nodes={projects.shown}
                all={
                  unlisted
                    ? exactTotal
                      ? `All projects (${nodes.length})`
                      : 'All projects'
                    : undefined
                }
                location={location}
                pathname={pathname}
                isOpen={isProjectOpen}
                onToggle={(node, next) =>
                  fold.setOpen(`tree:${node.key}`, next)
                }
                databases={databases}
                leafOf={serviceLeaf}
                stateOf={stateOf}
                onNavigate={closeOnMobile}
              />
            </SidebarMenuSub>
          )}
        </SidebarMenuItem>
        <SidebarMenuItem data-branch="server">
          <SidebarMenuButton
            asChild
            isActive={onServer}
            className={cn('justify-start', onServer && ACTIVE)}
          >
            <Link
              to={SERVER_PAGE.url}
              aria-current={
                serverExact ? 'page' : onServer ? 'true' : undefined
              }
              onClick={closeOnMobile}
            >
              <SERVER_PAGE.icon />
              <span>{SERVER_PAGE.title}</span>
              <RowState
                state={countedState([
                  ...(serverNodes.data?.nodes ?? []).map((node) =>
                    serverState(node.status)
                  ),
                  ...(serverDatabases.data ?? []).map((database) =>
                    databaseState(database.status)
                  ),
                ])}
              />
            </Link>
          </SidebarMenuButton>
        </SidebarMenuItem>
      </SidebarMenu>
    </SidebarGroup>
  )
}

/** The rows under an open project: its services, then their databases. */
function ProjectChildren({
  node,
  databases,
  location,
  pathname,
  leafOf,
  onNavigate,
}: {
  node: Extract<ProjectNode, { kind: 'project' }>
  databases: ProjectDatabases | undefined
  location: TreeLocation
  pathname: string
  leafOf: (service: ServiceNode) => Leaf
  onNavigate: () => void
}) {
  const url = groupHref(node.slug)
  const services = capRows(
    node.services,
    SERVICE_ROWS,
    (service) => service.slug === location.serviceSlug
  )
  return (
    <>
      {services.shown.map((service) => (
        <LeafRow
          key={service.id}
          leaf={leafOf(service)}
          pathname={pathname}
          onNavigate={onNavigate}
        />
      ))}
      {services.more > 0 && (
        <NoteRow to={url} onNavigate={onNavigate}>
          <span>{`${services.more} more`}</span>
        </NoteRow>
      )}
      {databases?.databases.map((database) => (
        <LeafRow
          key={`database:${database.id}`}
          leaf={{
            key: `database:${database.id}`,
            name: database.name,
            url: `/storage/${database.id}`,
            icon: (
              <ServiceLogo
                service={database.type}
                className="size-4 shrink-0"
              />
            ),
            state: databaseState(database.status),
            inside:
              location.branch === 'databases' &&
              location.recordId === database.id,
          }}
          pathname={pathname}
          onNavigate={onNavigate}
        />
      ))}
      {databases?.failed && (
        <RetryRow
          label="Databases could not be loaded"
          onRetry={databases.refetch}
        />
      )}
      {databases?.truncated && (
        <NoteRow to={`${url}/databases`} onNavigate={onNavigate}>
          <span>All databases</span>
        </NoteRow>
      )}
      <AddToProject node={node} onNavigate={onNavigate} />
    </>
  )
}

/** The rows under Projects: each project, foldable to what it is made of. */
function ProjectRows({
  query,
  nodes,
  all,
  location,
  pathname,
  isOpen,
  onToggle,
  databases,
  leafOf,
  stateOf,
  onNavigate,
}: {
  query: { isPending: boolean; isError: boolean; refetch: () => unknown }
  nodes: ProjectNode[]
  /** The link to every project, when some are not listed. */
  all: string | undefined
  location: TreeLocation
  pathname: string
  isOpen: (node: ProjectNode) => boolean
  onToggle: (node: ProjectNode, open: boolean) => void
  databases: Map<string, ProjectDatabases>
  leafOf: (service: ServiceNode) => Leaf
  stateOf: (service: ServiceNode) => TreeState | undefined
  onNavigate: () => void
}) {
  if (query.isError)
    return <RetryRow label="Could not be loaded" onRetry={query.refetch} />
  if (query.isPending)
    return (
      <li aria-busy="true" aria-label={`Loading ${SERVER_HOME.title}`}>
        <SidebarMenuSkeleton className="h-7" />
        <SidebarMenuSkeleton className="h-7" />
      </li>
    )
  if (nodes.length === 0)
    return SERVER_HOME.add ? (
      <NoteRow to={SERVER_HOME.add.href} onNavigate={onNavigate}>
        <Plus />
        <span>{SERVER_HOME.add.label}</span>
      </NoteRow>
    ) : null
  return (
    <>
      {nodes.map((node) => {
        if (node.kind === 'service')
          return (
            <LeafRow
              key={node.key}
              leaf={leafOf(node.service)}
              pathname={pathname}
              onNavigate={onNavigate}
            />
          )
        const url = groupHref(node.slug)
        const holdsCurrent = node.services.some(
          (service) => service.slug === location.serviceSlug
        )
        const open = isOpen(node)
        // Its own page, or folded over the service shown.
        const marked =
          node.slug === location.groupSlug || (holdsCurrent && !open)
        return (
          <SidebarMenuSubItem
            key={node.key}
            className="relative"
            data-project={node.slug}
          >
            <SidebarMenuSubButton asChild isActive={marked}>
              <Link
                to={url}
                aria-current={
                  pathname === url ? 'page' : marked ? 'true' : undefined
                }
                onClick={onNavigate}
                className={cn('pr-7', marked && ACTIVE)}
              >
                {open ? <FolderOpen /> : <Folder />}
                <span className="min-w-0 flex-1 truncate">{node.name}</span>
                {/* Open, each service says its own. */}
                {!open && (
                  <RowState state={worstState(node.services.map(stateOf))} />
                )}
              </Link>
            </SidebarMenuSubButton>
            <Fold
              nested
              name={node.name}
              open={open}
              onToggle={(next) => onToggle(node, next)}
            />
            {open && (
              <SidebarMenuSub
                aria-label={`${node.name} contents`}
                // Flush right, so states line up down the whole tree.
                className="ml-2 mr-0 pl-2 pr-0"
              >
                <ProjectChildren
                  node={node}
                  databases={databases.get(node.key)}
                  location={location}
                  pathname={pathname}
                  leafOf={leafOf}
                  onNavigate={onNavigate}
                />
              </SidebarMenuSub>
            )}
          </SidebarMenuSubItem>
        )
      })}
      {all && (
        <NoteRow to={SERVER_HOME.url} onNavigate={onNavigate}>
          <span>{all}</span>
        </NoteRow>
      )}
    </>
  )
}
