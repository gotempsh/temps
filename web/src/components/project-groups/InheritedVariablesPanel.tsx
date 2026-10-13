// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { Link } from 'react-router'
import { useQuery } from '@tanstack/react-query'
import { Boxes, RefreshCw } from 'lucide-react'
import { getInheritedEnvVarsOptions } from '@/api/client/@tanstack/react-query.gen'
import type { ProjectResponse } from '@/api/client'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { groupHref } from '@/lib/terminology'
import { ServiceGroupControl } from './ServiceGroupControl'

/**
 * On a service's variables page: what it inherits from its project, and
 * which of those it overrides. Standalone services get the way in instead.
 */
export function InheritedVariablesPanel({ project }: { project: ProjectResponse }) {
  const inherited = useQuery({
    ...getInheritedEnvVarsOptions({ path: { project_id: project.id } }),
    retry: false,
  })

  if (inherited.isLoading) return null

  if (inherited.isError) {
    return (
      <div className="flex items-center gap-3 rounded-lg border px-4 py-3 text-sm">
        <p className="flex-1 text-muted-foreground">
          Variables inherited from this service's project could not be loaded,
          so the list below may not be everything it receives.
        </p>
        <Button variant="outline" size="sm" onClick={() => void inherited.refetch()}>
          <RefreshCw className="size-4" />
          Retry
        </Button>
      </div>
    )
  }

  const group = inherited.data?.group
  if (!group) {
    return (
      <div className="flex flex-wrap items-center gap-2 text-sm text-muted-foreground">
        <Boxes className="size-4" aria-hidden="true" />
        <span>Setting the same values on several services?</span>
        <ServiceGroupControl project={project} inline />
        <span>and set them once for all of them.</span>
      </div>
    )
  }

  const variables = inherited.data?.variables ?? []
  const manageHref = `${groupHref(group.slug)}?tab=variables`

  return (
    <div className="rounded-lg border">
      <div className="flex flex-wrap items-center gap-2 border-b px-4 py-2.5">
        <Boxes className="size-4 text-muted-foreground" aria-hidden="true" />
        <p className="flex-1 text-sm">
          Inherited from project{' '}
          <Link to={groupHref(group.slug)} className="font-medium hover:underline">
            {group.name}
          </Link>
        </p>
        <Button variant="ghost" size="sm" asChild>
          <Link to={manageHref}>Manage shared variables</Link>
        </Button>
      </div>
      {variables.length === 0 ? (
        <p className="px-4 py-3 text-sm text-muted-foreground">
          {group.name} has no shared variables yet.{' '}
          <Link to={manageHref} className="font-medium text-foreground underline">
            Add one
          </Link>{' '}
          to give every service in it the same value.
        </p>
      ) : (
        <>
          <ul className="divide-y">
            {variables.map((v) => (
              <li
                key={`${v.key}-${v.environment_slug ?? 'all'}`}
                className="flex flex-wrap items-center gap-2 px-4 py-2"
              >
                <span
                  className={
                    v.defined_on_project
                      ? 'font-mono text-sm text-muted-foreground line-through'
                      : 'font-mono text-sm'
                  }
                >
                  {v.key}
                </span>
                <Badge variant="secondary" className="font-normal">
                  {v.environment_slug ?? 'All environments'}
                </Badge>
                {v.is_secret && (
                  <Badge variant="outline" className="font-normal">
                    Secret
                  </Badge>
                )}
                {v.defined_on_project && (
                  <span className="text-xs text-muted-foreground">
                    Overridden by this service's own {v.key}
                  </span>
                )}
              </li>
            ))}
          </ul>
          <p className="border-t px-4 py-2 text-xs text-muted-foreground">
            This service's own variables below win when names match. Changes
            to shared variables apply on the next deployment.
          </p>
        </>
      )}
    </div>
  )
}
