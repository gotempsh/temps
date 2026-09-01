// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { Input } from '@/components/ui/input'
import { usePluginsContext } from '@/contexts/PluginsContext'
import { useCanViewAuditLogs } from '@/hooks/useAuditAccess'
import { resolvePluginIcon } from '@/lib/pluginIcons'
import { Search, Sparkles } from 'lucide-react'
import { useMemo, useState } from 'react'
import { Link } from 'react-router'
import {
  platformToolGroups,
  platformToolShortcuts,
  extensionToolGroupIcon,
  type PlatformToolGroup,
} from '@/components/platform/platform-tools'
import { FeatureMaturityBadge } from '@/components/feature-maturity/FeatureMaturityBadge'

export function PlatformTools() {
  const [query, setQuery] = useState('')
  const { platformNavEntries } = usePluginsContext()
  const canViewAuditLogs = useCanViewAuditLogs()

  const groups = useMemo<PlatformToolGroup[]>(() => {
    const permittedGroups = platformToolGroups.map((group) => ({
      ...group,
      items: group.items.filter(
        (item) => canViewAuditLogs || item.url !== '/audit-logs'
      ),
    }))
    if (platformNavEntries.length === 0) return permittedGroups
    return [
      ...permittedGroups,
      {
        label: 'Extensions',
        description: 'Open capabilities added by installed plugins.',
        icon: extensionToolGroupIcon,
        items: platformNavEntries.map((entry) => ({
          title: entry.label,
          description: `Open the ${entry.label} extension.`,
          url: entry.path,
          icon: resolvePluginIcon(entry.icon),
        })),
      },
    ]
  }, [canViewAuditLogs, platformNavEntries])

  const normalizedQuery = query.trim().toLowerCase()
  const filteredGroups = groups
    .map((group) => ({
      ...group,
      items: group.items.filter((item) =>
        [
          group.label,
          group.description,
          item.title,
          item.description,
          ...(item.keywords ?? []),
        ]
          .join(' ')
          .toLowerCase()
          .includes(normalizedQuery)
      ),
    }))
    .filter((group) => group.items.length > 0)

  return (
    <div className="mx-auto w-full max-w-7xl space-y-8 pb-12">
      <div className="flex flex-col gap-5 border-b pb-6 lg:flex-row lg:items-end lg:justify-between">
        <div className="max-w-2xl">
          <h1 className="text-2xl font-semibold tracking-tight">
            All platform tools
          </h1>
          <p className="mt-1 text-sm leading-relaxed text-muted-foreground">
            Every Temps capability remains available here while the main sidebar
            stays focused on daily work.
          </p>
        </div>
        <div className="relative w-full lg:max-w-md">
          <Search className="pointer-events-none absolute left-3 top-1/2 size-4 -translate-y-1/2 text-muted-foreground" />
          <Input
            value={query}
            onChange={(event) => setQuery(event.target.value)}
            placeholder="Search domains, backups, email, proxy…"
            className="h-11 pl-10"
            aria-label="Search platform tools"
          />
        </div>
      </div>

      {!normalizedQuery && (
        <section aria-labelledby="common-platform-tasks">
          <div className="mb-3">
            <h2 id="common-platform-tasks" className="text-sm font-semibold">
              Common tasks
            </h2>
            <p className="text-xs text-muted-foreground">
              Fast paths for setting up and operating this instance.
            </p>
          </div>
          <div className="grid gap-2 sm:grid-cols-2 xl:grid-cols-4">
            {platformToolShortcuts.map((item) => (
              <Link
                key={item.url}
                to={item.url}
                className="group rounded-xl border bg-card p-4 transition-colors hover:border-foreground/20 hover:bg-accent"
              >
                <item.icon className="mb-4 size-5 text-muted-foreground transition-colors group-hover:text-foreground" />
                <span className="block text-sm font-medium">{item.title}</span>
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
              <div className="mb-3 flex items-start gap-3 px-1">
                <span className="grid size-9 shrink-0 place-items-center rounded-lg border bg-muted/40 text-muted-foreground">
                  <group.icon className="size-4" />
                </span>
                <div className="min-w-0">
                  <h2 className="text-base font-semibold">{group.label}</h2>
                  <p className="mt-0.5 text-xs leading-relaxed text-muted-foreground">
                    {group.description}
                  </p>
                </div>
              </div>
              <div className="grid gap-1 sm:grid-cols-2">
                {group.items.map((item) => (
                  <Link
                    key={item.url}
                    to={item.url}
                    className="group flex min-w-0 items-center gap-3 rounded-lg border border-transparent px-3 py-2.5 transition-colors hover:border-border hover:bg-accent/60"
                  >
                    <span className="grid size-8 shrink-0 place-items-center rounded-lg border bg-background text-muted-foreground">
                      <item.icon className="size-4" />
                    </span>
                    <span className="min-w-0">
                      <span className="block truncate text-sm font-medium">
                        <span className="flex items-center gap-2">
                          <span className="truncate">{item.title}</span>
                          <FeatureMaturityBadge
                            featureKey={item.featureKey}
                            compact
                          />
                        </span>
                      </span>
                      <span className="block truncate text-xs text-muted-foreground">
                        {item.description}
                      </span>
                    </span>
                  </Link>
                ))}
              </div>
            </section>
          ))}
        </div>
      ) : (
        <div className="rounded-xl border border-dashed px-6 py-16 text-center">
          <Search className="mx-auto mb-3 size-6 text-muted-foreground" />
          <p className="text-sm font-medium">No matching platform tool</p>
          <p className="mt-1 text-xs text-muted-foreground">
            Try domains, backups, email, proxy, or certificates.
          </p>
        </div>
      )}

      <div className="flex items-start gap-3 rounded-xl border bg-muted/30 p-4 text-sm">
        <Sparkles className="mt-0.5 size-4 shrink-0 text-muted-foreground" />
        <p className="text-muted-foreground">
          You can also press{' '}
          <kbd className="rounded border bg-background px-1.5 py-0.5 font-mono text-[10px]">
            ⌘K
          </kbd>{' '}
          and search across projects, services, settings, and tools.
        </p>
      </div>
    </div>
  )
}
