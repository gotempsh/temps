// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { ArrowRight } from 'lucide-react'
import { Link } from 'react-router'
import type { ProjectResponse } from '@/api/client'
import { Button } from '@/components/ui/button'
import { Skeleton } from '@/components/ui/skeleton'
import { CombinedProjectSettings } from '@/components/project/settings/CombinedProjectSettings'
import { environmentRuntimeFacts } from '@/lib/environment-runtime'
import { ENVIRONMENT_PARAM } from '@/lib/project-environment-selection'
import { projectRoot } from '@/lib/project-navigation'
import { useProjectEnvironment } from './project-environment-context'

/**
 * What the selected environment actually runs, above the project-wide build
 * settings. Branch, replicas and resources are per environment, so they are
 * edited on the environment; this summary is where someone looking for them
 * under "Build & deploy" finds them.
 */
function EnvironmentRuntimeSummary({ project }: { project: ProjectResponse }) {
  const { selection, isPending } = useProjectEnvironment()
  const environment = selection.environment
  if (isPending)
    return (
      <section aria-busy="true">
        <Skeleton className="h-24 w-full" />
      </section>
    )
  if (!environment) return null
  const settingsHref = `${projectRoot(project.slug)}/environments?view=settings&${ENVIRONMENT_PARAM}=${encodeURIComponent(environment.slug)}`
  const facts = environmentRuntimeFacts(project, environment)
  return (
    <section
      aria-label={`How ${environment.name} deploys`}
      className="flex min-w-0 flex-col gap-2 rounded-lg border px-4 py-3 text-sm sm:flex-row sm:items-center sm:justify-between"
    >
      <p className="min-w-0 text-muted-foreground">
        <span className="font-medium text-foreground">{environment.name}</span>{' '}
        deploys{' '}
        <code className="font-mono text-foreground">{facts.branch}</code>{' '}
        {facts.trigger} · {facts.details.join(' · ')}
      </p>
      <Button
        asChild
        variant="ghost"
        size="sm"
        className="shrink-0 self-start sm:self-auto"
      >
        <Link to={settingsHref}>
          Edit {environment.name} settings
          <ArrowRight className="size-4" aria-hidden="true" />
        </Link>
      </Button>
    </section>
  )
}

/** Build & deploy: the environment's runtime, then project-wide delivery. */
export function BuildAndDeployPage({
  project,
  refetch,
}: {
  project: ProjectResponse
  refetch: () => void
}) {
  return (
    <CombinedProjectSettings
      page="delivery"
      project={project}
      refetch={refetch}
      intro={<EnvironmentRuntimeSummary project={project} />}
    />
  )
}
