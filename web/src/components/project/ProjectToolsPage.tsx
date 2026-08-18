import { type ProjectResponse } from '@/api/client'
import { Button } from '@/components/ui/button'
import { FeatureMaturityBadge } from '@/components/feature-maturity/FeatureMaturityBadge'
import { Input } from '@/components/ui/input'
import { usePluginsContext } from '@/contexts/PluginsContext'
import { usePinnedProjectTools } from '@/hooks/usePinnedProjectTools'
import { resolvePluginIcon } from '@/lib/pluginIcons'
import { cn } from '@/lib/utils'
import { Pin, PinOff, Search, Sparkles } from 'lucide-react'
import { useMemo, useState } from 'react'
import { Link } from 'react-router'
import {
  flattenProjectTools,
  projectToolGroups,
  projectToolShortcuts,
  type ProjectToolGroup,
  type ProjectToolItem,
} from './project-tools'

export function ProjectToolsPage({ project }: { project: ProjectResponse }) {
  const [query, setQuery] = useState('')
  const { projectNavEntries } = usePluginsContext()

  const groups = useMemo<ProjectToolGroup[]>(() => {
    if (projectNavEntries.length === 0) return projectToolGroups
    return [
      ...projectToolGroups,
      {
        label: 'Extensions',
        description: 'Open capabilities added by installed plugins.',
        items: projectNavEntries.map((entry) => ({
          title: entry.label,
          url: entry.path,
          icon: resolvePluginIcon(entry.icon),
        })),
      },
    ]
  }, [projectNavEntries])

  const tools = useMemo(() => flattenProjectTools(groups), [groups])
  const toolByUrl = useMemo(
    () => new Map(tools.map((tool) => [tool.url, tool])),
    [tools]
  )
  const { pinnedUrls, pinnedSet, togglePinned } = usePinnedProjectTools(
    project.slug,
    tools.map((tool) => tool.url)
  )
  const pinnedTools = pinnedUrls
    .map((url) => toolByUrl.get(url))
    .filter((tool): tool is ProjectToolItem => tool !== undefined)

  const normalizedQuery = query.trim().toLowerCase()
  const filteredGroups = groups
    .map((group) => ({
      ...group,
      items: group.items.filter(
        (item) =>
          !normalizedQuery ||
          group.label.toLowerCase().includes(normalizedQuery) ||
          group.description.toLowerCase().includes(normalizedQuery) ||
          item.title.toLowerCase().includes(normalizedQuery)
      ),
    }))
    .filter((group) => group.items.length > 0)

  const hrefFor = (url: string) =>
    url.startsWith('/') ? url : `/projects/${project.slug}/${url}`

  const toolRow = (item: ProjectToolItem, compact = false) => {
    const pinned = pinnedSet.has(item.url)
    return (
      <div
        key={item.url}
        className={cn(
          'group flex min-w-0 items-center gap-1 rounded-lg border border-transparent transition-colors hover:border-border hover:bg-accent/60',
          compact ? 'bg-muted/30' : 'bg-background'
        )}
      >
        <Link
          to={hrefFor(item.url)}
          className="flex min-w-0 flex-1 items-center gap-3 px-3 py-2.5 focus-visible:outline-2 focus-visible:outline-ring"
        >
          <span className="grid size-8 shrink-0 place-items-center rounded-lg border bg-background text-muted-foreground">
            <item.icon className="size-4" />
          </span>
          <span className="min-w-0 truncate text-sm font-medium">
            {item.title}
          </span>
          <FeatureMaturityBadge featureKey={item.featureKey} compact />
        </Link>
        <Button
          type="button"
          variant="ghost"
          size="sm"
          onClick={() => togglePinned(item.url)}
          aria-label={`${pinned ? 'Unpin' : 'Pin'} ${item.title}`}
          title={`${pinned ? 'Unpin from' : 'Pin to'} project sidebar`}
          className="mr-1.5 shrink-0 text-muted-foreground hover:text-foreground"
        >
          {pinned ? <PinOff className="size-4" /> : <Pin className="size-4" />}
          <span className="hidden xl:inline">{pinned ? 'Unpin' : 'Pin'}</span>
        </Button>
      </div>
    )
  }

  return (
    <div className="w-full space-y-8 pb-12">
      <div className="flex flex-col gap-5 border-b pb-6 lg:flex-row lg:items-end lg:justify-between">
        <div className="max-w-2xl">
          <h1 className="text-2xl font-semibold tracking-tight">
            All project tools
          </h1>
          <p className="mt-1 text-sm leading-relaxed text-muted-foreground">
            Browse every secondary capability for {project.name}. Pin the tools
            you use often to keep them one click away in the project sidebar.
          </p>
        </div>
        <div className="relative w-full lg:max-w-md">
          <Search className="pointer-events-none absolute left-3 top-1/2 size-4 -translate-y-1/2 text-muted-foreground" />
          <Input
            value={query}
            onChange={(event) => setQuery(event.target.value)}
            placeholder="Search tools, for example “uptime” or “secrets”"
            className="h-11 pl-10"
            aria-label="Search project tools"
          />
        </div>
      </div>

      {pinnedTools.length > 0 && !normalizedQuery && (
        <section aria-labelledby="pinned-project-tools">
          <div className="mb-3 flex items-center gap-2">
            <Pin className="size-4" />
            <div>
              <h2 id="pinned-project-tools" className="text-sm font-semibold">
                Pinned to your sidebar
              </h2>
              <p className="text-xs text-muted-foreground">
                Stored in this browser for {project.slug}.
              </p>
            </div>
          </div>
          <div className="grid gap-2 sm:grid-cols-2 xl:grid-cols-3">
            {pinnedTools.map((item) => toolRow(item, true))}
          </div>
        </section>
      )}

      {!normalizedQuery && (
        <section aria-labelledby="common-project-tasks">
          <div className="mb-3">
            <h2 id="common-project-tasks" className="text-sm font-semibold">
              Common tasks
            </h2>
            <p className="text-xs text-muted-foreground">
              Fast paths for the work that usually starts here.
            </p>
          </div>
          <div className="grid gap-2 sm:grid-cols-2 xl:grid-cols-4">
            {projectToolShortcuts.map((item) => (
              <Link
                key={item.url}
                to={hrefFor(item.url)}
                className="group rounded-xl border bg-card p-4 transition-colors hover:border-foreground/20 hover:bg-accent"
              >
                <item.icon className="mb-4 size-5 text-muted-foreground transition-colors group-hover:text-foreground" />
                <span className="flex items-center gap-2 text-sm font-medium">
                  {item.title}
                  <FeatureMaturityBadge featureKey={item.featureKey} compact />
                </span>
                <span className="mt-1 block text-xs leading-relaxed text-muted-foreground">
                  {item.description}
                </span>
              </Link>
            ))}
          </div>
        </section>
      )}

      {filteredGroups.length > 0 ? (
        <div className="grid items-start gap-4 lg:grid-cols-2">
          {filteredGroups.map((group) => (
            <section
              key={group.label}
              className="rounded-xl border bg-card p-4"
            >
              <div className="mb-3 px-1">
                <h2 className="text-base font-semibold">{group.label}</h2>
                <p className="mt-0.5 text-xs leading-relaxed text-muted-foreground">
                  {group.description}
                </p>
              </div>
              <div className="grid gap-1 sm:grid-cols-2">
                {group.items.map((item) => toolRow(item))}
              </div>
            </section>
          ))}
        </div>
      ) : (
        <div className="rounded-xl border border-dashed px-6 py-16 text-center">
          <Search className="mx-auto mb-3 size-6 text-muted-foreground" />
          <p className="text-sm font-medium">No matching project tool</p>
          <p className="mt-1 text-xs text-muted-foreground">
            Try a capability such as visitors, uptime, secrets, or webhooks.
          </p>
        </div>
      )}

      <div className="flex items-start gap-3 rounded-xl border bg-muted/30 p-4 text-sm">
        <Sparkles className="mt-0.5 size-4 shrink-0 text-muted-foreground" />
        <p className="text-muted-foreground">
          Not sure what it is called? Press{' '}
          <kbd className="rounded border bg-background px-1.5 py-0.5 font-mono text-[10px]">
            ⌘K
          </kbd>{' '}
          and describe where you want to go.
        </p>
      </div>
    </div>
  )
}
